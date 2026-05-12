import 'dart:async';
import 'dart:ffi';
import 'dart:io' show ProcessSignal;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:meta/meta.dart';

import '../api/api.dart';
import '../bindings/dart_api.dart';
import '../bindings/session.dart' as bindings;
import '../bindings/status.dart';
import '../ffi/ptyx.g.dart' as native;

bool? _termModeFlag(int fields, int fieldBit, int value) {
  if (fields & fieldBit == 0) return null;
  return value != 0;
}

@internal
final class NativePtySession implements PtySession {
  final int _handle;
  final ReceivePort _eventPort;
  final ReceivePort _outputPort;
  final _exitCode = Completer<int>();
  final StreamController<Uint8List> _outputController;
  final _modeController = StreamController<PtyTermMode>.broadcast();
  final _pendingAcks = <int>[];

  late final StreamSubscription<Object?> _outputSub;
  late final StreamSubscription<Object?> _eventSub;
  var _outputDone = false;
  var _outputCancelled = false;
  var _closed = false;

  factory NativePtySession.spawn(PtySpawnOptions options) {
    ensureDartInitialized();

    final outputPort = ReceivePort();
    final eventPort = ReceivePort();

    try {
      final sessionHandle = bindings.sessionSpawn(
        options: options,
        eventPort: eventPort.sendPort.nativePort,
        outputPort: outputPort.sendPort.nativePort,
      );
      return NativePtySession._(sessionHandle, outputPort, eventPort);
    } catch (_) {
      outputPort.close();
      eventPort.close();
      rethrow;
    }
  }

  NativePtySession._(this._handle, this._outputPort, this._eventPort)
    : _outputController = StreamController<Uint8List>() {
    _outputController.onListen = _flushAcks;
    _outputController.onResume = _flushAcks;
    _outputController.onCancel = () {
      _outputCancelled = true;
      _flushAcks();
    };

    _outputSub = _outputPort.listen(_handleOutput);
    _eventSub = _eventPort.listen(_handleEvent);
    unawaited(_exitCode.future.catchError((_) => 0));
  }

  @override
  Future<int> get exitCode => _exitCode.future;

  @override
  PtyTermMode? get mode => bindings.sessionMode(_openHandle);

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _outputController.stream;

  @override
  int? get pid => bindings.sessionPid(_openHandle);

  @override
  PtySize get size => bindings.sessionSize(_openHandle);

  @override
  String? get ttyName => bindings.sessionTtyName(_openHandle);

  int get _openHandle {
    if (_closed) throw const PtyClosedException('session closed');
    return _handle;
  }

  @override
  Future<void> close() async {
    if (_closed) return;

    if (!_exitCode.isCompleted) {
      bindings.sessionKill(_handle, ProcessSignal.sigterm);
      await _ignoreExitCodeFor(const Duration(milliseconds: 50));
      if (!_exitCode.isCompleted) {
        bindings.sessionKill(_handle, ProcessSignal.sigkill);
        await _ignoreExitCodeFor(const Duration(milliseconds: 50));
      }
    }

    _closed = true;
    bindings.sessionFree(_handle);
    _outputPort.close();
    _eventPort.close();
    unawaited(_closeOutput());
    _completeExitCodeError(const PtyClosedException('session closed'));
    await Future.wait<void>([
      _outputSub.cancel(),
      _eventSub.cancel(),
      _modeController.close(),
    ]);
  }

  @override
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]) {
    if (_closed) return false;
    return bindings.sessionKill(_handle, signal);
  }

  @override
  void resize(PtySize size) => bindings.sessionResize(_openHandle, size);

  @override
  void write(Uint8List data) => bindings.sessionWrite(_openHandle, data);

  void _ack(int byteCount) {
    if (_closed) return;
    bindings.sessionAckOutput(_handle, byteCount);
  }

  Future<void> _closeOutput() async {
    if (_outputDone) return;
    _outputDone = true;
    return _outputController.close();
  }

  void _completeExitCodeError(PtyException error) {
    if (!_exitCode.isCompleted) _exitCode.completeError(error);
  }

  void _flushAcks() {
    if (!_canAckOutput) return;
    for (final ack in _pendingAcks) {
      _ack(ack);
    }
    _pendingAcks.clear();
  }

  bool get _canAckOutput {
    return _outputCancelled ||
        (_outputController.hasListener && !_outputController.isPaused);
  }

  void _handleEvent(Object? message) {
    if (_closed) return;
    switch (message) {
      case [native.PTYX_EVENT_EXIT, final int exitCode]:
        if (!_exitCode.isCompleted) _exitCode.complete(exitCode);
      case [
        native.PTYX_EVENT_ERROR,
        final int source,
        final int status,
        final String text,
      ]:
        _handleNativeError(source, exceptionFromParts(status, text));
      case [
        native.PTYX_EVENT_TERM_MODE,
        final int fields,
        final int canonical,
        final int echo,
        final int signals,
      ]:
        _modeController.add(
          PtyTermMode(
            canonical: _termModeFlag(
              fields,
              native.PTYX_TERM_MODE_CANONICAL_VALID,
              canonical,
            ),
            echo: _termModeFlag(fields, native.PTYX_TERM_MODE_ECHO_VALID, echo),
            signals: _termModeFlag(
              fields,
              native.PTYX_TERM_MODE_SIGNALS_VALID,
              signals,
            ),
          ),
        );
    }
  }

  void _handleNativeError(int source, PtyException error) {
    switch (source) {
      case native.PTYX_ERROR_SOURCE_OUTPUT || native.PTYX_ERROR_SOURCE_WRITE:
        if (!_outputDone && !_outputCancelled) {
          _outputController.addError(error);
        }
      case native.PTYX_ERROR_SOURCE_WAIT:
        _completeExitCodeError(error);
      case native.PTYX_ERROR_SOURCE_MODE:
        _modeController.addError(error);
      default:
    }
  }

  void _handleOutput(Object? message) {
    if (_closed) return;
    switch (message) {
      case [native.PTYX_MESSAGE_OUTPUT, final Uint8List bytes]:
        if (_outputDone) return;
        if (_outputCancelled) {
          _ack(bytes.length);
          return;
        }
        _outputController.add(bytes);
        if (_canAckOutput) {
          _ack(bytes.length);
        } else {
          _pendingAcks.add(bytes.length);
        }
      case [native.PTYX_MESSAGE_CLOSED]:
        unawaited(_closeOutput());
    }
  }

  Future<void> _ignoreExitCodeFor(Duration timeout) async {
    try {
      await _exitCode.future.timeout(timeout);
    } on Object {
      return;
    }
  }
}
