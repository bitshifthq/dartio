import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Directory, Platform, ProcessSignal;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/controller.dart';

final _sessionFinalizer = NativeFinalizer(
  Native.addressOf<NativeFinalizerFunction>(controllerFinalize),
);
final _sessionRegistryFinalizer = Finalizer<_SessionRegistryToken>((token) {
  controllerAbandon(token.handle);
  token.runtime._removeSession(token.handle);
});
const _terminalDrainTimeout = Duration(seconds: 15);
const _supervisorRemove = 3;
const _supervisorOwnerExit = 4;
const _supervisorIdle = 5;
const _supervisorArm = 6;
const _supervisorSpawn = 7;
const _supervisorStartupTimeout = Duration(seconds: 30);

int? _lastNativeCode() {
  final code = controllerLastErrorCode();
  return code == 0 ? null : code;
}

final class _SessionRegistryToken {
  const _SessionRegistryToken(this.runtime, this.handle);

  final _ControllerRuntime runtime;
  final int handle;
}

Future<void> _superviseOwner(SendPort ready) async {
  final commands = ReceivePort();
  final startupDeadline = Timer(_supervisorStartupTimeout, commands.close);
  ready.send(commands.sendPort);
  final handles = <int>{};
  var ownerExited = false;
  var idle = false;
  await for (final message in commands) {
    switch (message) {
      case [_supervisorRemove, final int handle]:
        handles.remove(handle);
      case [_supervisorOwnerExit]:
        ownerExited = true;
      case [_supervisorIdle]:
        idle = true;
      case [_supervisorArm, final SendPort acknowledgement]:
        startupDeadline.cancel();
        acknowledgement.send(null);
      case [
        _supervisorSpawn,
        final PtySpawnOptions options,
        final String workingDirectory,
        final SendPort reply,
      ]:
        try {
          final result = _spawnNative(options, workingDirectory);
          if (result.handle != 0) {
            handles.add(result.handle);
          }
          reply.send([result.handle, result.nativeCode]);
        } on Object catch (error, stackTrace) {
          reply.send([error, stackTrace.toString()]);
        }
    }
    if (ownerExited) {
      for (final handle in handles) {
        controllerAbandon(handle);
      }
      commands.close();
    } else if (idle && handles.isEmpty) {
      commands.close();
    }
  }
  startupDeadline.cancel();
}

@internal
final class NativeSession implements PtySession, Finalizable {
  NativeSession._(
    this._runtime,
    this._handle,
    this._inputCapacity,
    this._gracefulCloseTimeout,
    this._outputController,
    this._modeController,
  ) {
    // These completion channels are optional to observe. Retain their error
    // for callers without reporting it as an unhandled zone error first.
    _exit.future.ignore();
  }

  static Future<NativeSession> spawn(PtySpawnOptions options) async {
    final snapshot = PtySpawnOptions(
      executable: options.executable,
      arguments: List.unmodifiable(options.arguments),
      environment: Map.unmodifiable(options.environment),
      environmentMode: options.environmentMode,
      workingDirectory: options.workingDirectory,
      initialSize: options.initialSize,
      maxBufferedInput: options.maxBufferedInput,
      maxBufferedOutput: options.maxBufferedOutput,
      gracefulCloseTimeout: options.gracefulCloseTimeout,
    );
    final workingDirectory =
        snapshot.workingDirectory ?? Directory.current.path;
    _validateSpawnOptions(snapshot, workingDirectory);
    final runtime = _ControllerRuntime.instance;
    final supervisor = await runtime._beginSpawn();
    var handle = 0;
    var retained = false;
    try {
      final reply = ReceivePort();
      late final ({int handle, int? nativeCode}) spawnResult;
      try {
        supervisor.send([
          _supervisorSpawn,
          snapshot,
          workingDirectory,
          reply.sendPort,
        ]);
        final response = await reply.first;
        if (response case [final int handle, final int? nativeCode]) {
          spawnResult = (handle: handle, nativeCode: nativeCode);
        } else if (response case [
          final Object error,
          final String stackTrace,
        ]) {
          Error.throwWithStackTrace(error, StackTrace.fromString(stackTrace));
        } else {
          throw const PtyInfrastructureException(
            'native spawn supervisor returned an invalid response',
            operation: 'spawn',
          );
        }
      } finally {
        reply.close();
      }
      handle = spawnResult.handle;
      if (handle == 0) {
        if (_isUnsupportedNativeCode(spawnResult.nativeCode)) {
          throw PtyUnsupportedException(
            'native PTY support is unavailable on this operating-system build',
            operation: 'spawn',
            nativeCode: spawnResult.nativeCode,
          );
        }
        throw PtySpawnException(
          'native process spawn failed',
          nativeCode: spawnResult.nativeCode,
        );
      }

      late final NativeSession session;
      // The returned session owns and closes this controller.
      // ignore: close_sinks
      final output = StreamController<Uint8List>(
        sync: true,
        onListen: () => session._setPaused(false),
        onPause: () => session._setPaused(true),
        onResume: () => session._setPaused(false),
        onCancel: () => session._cancelOutput(),
      );
      // The returned session owns and closes this controller.
      // ignore: close_sinks
      final modes = StreamController<PtyTermMode>.broadcast(
        sync: true,
        onListen: () => session._startModeObservation(),
        onCancel: () => session._stopModeObservation(),
      );
      session = NativeSession._(
        runtime,
        handle,
        snapshot.maxBufferedInput,
        snapshot.gracefulCloseTimeout,
        output,
        modes,
      );
      runtime._addSession(session);
      _sessionFinalizer.attach(
        session,
        Pointer<Void>.fromAddress(handle),
        detach: session,
      );
      _sessionRegistryFinalizer.attach(
        session,
        _SessionRegistryToken(runtime, handle),
        detach: session,
      );
      if (!controllerActivate(
        handle,
        runtime.notificationPort,
        runtime.notificationPort,
      )) {
        runtime._removeSession(handle);
        controllerClose(handle);
        throw const PtyInfrastructureException(
          'native session activation failed',
          operation: 'spawn',
        );
      }
      retained = true;
      return session;
    } finally {
      if (handle != 0 && !retained) {
        runtime._forgetSupervisedHandle(handle);
        controllerAbandon(handle);
      }
      runtime._finishSpawn();
    }
  }

  static bool _isUnsupportedNativeCode(int? nativeCode) {
    if (Platform.isWindows) {
      return nativeCode == 50;
    }
    if (Platform.isMacOS) {
      return nativeCode == 45;
    }
    return nativeCode == 95;
  }

  final _ControllerRuntime _runtime;
  final int _handle;
  final StreamController<Uint8List> _outputController;
  final StreamController<PtyTermMode> _modeController;
  final _exit = Completer<int>();
  final _outputDone = Completer<void>();
  final int _inputCapacity;
  final Duration _gracefulCloseTimeout;
  Completer<void>? _close;
  var _pendingCredit = 0;
  final Queue<Uint8List> _pendingOutput = Queue();
  var _paused = true;
  var _outputCancelled = false;
  var _infrastructureLost = false;
  var _terminalOutputScheduled = false;
  Object? _terminalFailure;
  PtyInputException? _inputFailure;
  var _creditScheduled = false;
  Timer? _modeTimer;
  PtyTermMode? _lastMode;
  var _unchangedModeSamples = 0;

  @override
  PtyCapabilities get capabilities {
    final bits = controllerCapabilities();
    return PtyCapabilities(
      signals: bits & 1 != 0,
      processGroups: bits & 2 != 0,
      terminalModes: bits & 4 != 0,
      conPty: bits & 8 != 0,
      terminalName: bits & 16 != 0,
    );
  }

  @override
  Future<int> get exitCode => _exit.future;

  @override
  Future<PtyExitStatus> get exitStatus async {
    final code = await _exit.future;
    return capabilities.conPty || code >= 0
        ? PtyExited(code)
        : PtySignaled(-code);
  }

  @override
  PtyTermMode? get mode {
    _checkOpen('mode');
    if (!capabilities.terminalModes) {
      return null;
    }
    return _readMode();
  }

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _outputController.stream;

  @override
  void discardOutput() => _cancelOutput();

  @override
  int? get pid {
    _checkOpen('pid');
    final value = controllerPid(_handle);
    if (value < 0) {
      throw PtyMetadataException(
        'native child process identifier was unavailable',
        operation: 'pid',
        nativeCode: _lastNativeCode(),
      );
    }
    return value;
  }

  @override
  PtySize get size {
    _checkOpen('size');
    return using((arena) {
      final values = arena<Uint32>(4);
      if (!controllerSize(_handle, values)) {
        throw PtyMetadataException(
          'native terminal size query failed',
          operation: 'size',
          nativeCode: _lastNativeCode(),
        );
      }
      return PtySize(
        rows: values[0],
        columns: values[1],
        pixelWidth: values[2],
        pixelHeight: values[3],
      );
    });
  }

  @override
  String? get ttyName {
    _checkOpen('ttyName');
    if (!capabilities.terminalName) {
      return null;
    }
    final length = controllerTtyName(_handle, nullptr, 0);
    if (length < 0) {
      throw PtyMetadataException(
        'native terminal name query failed',
        operation: 'ttyName',
        nativeCode: _lastNativeCode(),
      );
    }
    return using((arena) {
      final bytes = arena<Uint8>(length);
      final written = controllerTtyName(_handle, bytes, length);
      if (written != length) {
        throw PtyMetadataException(
          'native terminal name changed during read',
          operation: 'ttyName',
          nativeCode: _lastNativeCode(),
        );
      }
      return utf8.decode(bytes.asTypedList(length));
    });
  }

  bool get _isTerminal => _infrastructureLost || (_close?.isCompleted ?? false);

  void _checkOpen([String operation = 'state']) {
    if (_close != null) {
      throw PtyClosedException('session closed', operation: operation);
    }
  }

  @override
  void write(Uint8List data) {
    const operation = 'write';
    _validateWrite(data, operation);
    final result = using((arena) {
      final pointer = arena<Uint8>(data.length);
      pointer.asTypedList(data.length).setAll(0, data);
      return controllerWrite(_handle, pointer, data.length);
    });
    if (result == -2) {
      final failure = PtyInfrastructureException(
        'native session runtime is unavailable',
        operation: operation,
        nativeCode: controllerLastErrorCode(),
      );
      _infrastructureLost = true;
      _fail(failure);
      throw failure;
    }
    if (result < 0) {
      final nativeCode = controllerLastErrorCode();
      final failure = PtyInputException(
        'session can no longer accept input',
        operation: operation,
        nativeCode: nativeCode == 0 ? null : nativeCode,
      );
      _inputFailure ??= failure;
      throw _inputError(operation, _inputFailure!);
    }
    if (result == 0) {
      throw PtyBackpressureException(
        'bounded native input storage is full',
        context: '${data.length} bytes rejected',
      );
    }
  }

  void _validateWrite(Uint8List data, String operation) {
    _checkOpen(operation);
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw _inputError(operation, inputFailure);
    }
    if (data.isEmpty) {
      throw PtyInvalidArgumentException(
        'input data must not be empty',
        operation: operation,
      );
    }
    if (data.length > _inputCapacity) {
      throw PtyInvalidArgumentException(
        'input data exceeds this session input capacity',
        operation: operation,
        context: '${data.length} bytes exceeds $_inputCapacity bytes',
      );
    }
  }

  @override
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]) {
    if (_close != null || _exit.isCompleted) {
      return false;
    }
    final result = controllerSignal(_handle, signal.signalNumber);
    if (result < 0) {
      throw PtySignalException(
        'native signal delivery failed',
        operation: 'kill',
        nativeCode: _lastNativeCode(),
      );
    }
    return result == 1;
  }

  @override
  void resize(PtySize size) {
    _checkOpen('resize');
    _validateSize(size, operation: 'resize');
    if (!controllerResize(
      _handle,
      size.rows,
      size.columns,
      size.pixelWidth,
      size.pixelHeight,
    )) {
      throw PtyResizeException(
        'native terminal resize failed',
        nativeCode: _lastNativeCode(),
      );
    }
  }

  @override
  Future<void> close() {
    final existing = _close;
    if (existing != null) {
      return existing.future;
    }
    final completion = Completer<void>();
    _close = completion;
    unawaited(_closeNative(completion));
    return completion.future;
  }

  void _setPaused(bool paused) {
    if (_outputCancelled || _isTerminal) {
      return;
    }
    _paused = paused;
    if (!paused) {
      _queueCredit(_pendingCredit);
      _pendingCredit = 0;
    }
    controllerPause(_handle, paused);
    if (!paused) {
      while (!_paused && _pendingOutput.isNotEmpty) {
        _deliverOutput(_pendingOutput.removeFirst());
      }
    }
  }

  void _cancelOutput() {
    if (_outputCancelled) {
      return;
    }
    _outputCancelled = true;
    _paused = false;
    _queueCredit(_pendingCredit);
    _pendingCredit = 0;
    while (_pendingOutput.isNotEmpty) {
      _queueCredit(_pendingOutput.removeFirst().length);
    }
    controllerPause(_handle, false);
  }

  void _queueCredit(int bytes) {
    if (bytes == 0) {
      return;
    }
    _runtime._credit[_handle] = (_runtime._credit[_handle] ?? 0) + bytes;
    if (_creditScheduled) {
      return;
    }
    _creditScheduled = true;
    scheduleMicrotask(_drainCredit);
  }

  void _drainCredit() {
    final bytes = _runtime._credit[_handle] ?? 0;
    if (bytes == 0 || _isTerminal) {
      if (_isTerminal) {
        _runtime._credit.remove(_handle);
      }
      _creditScheduled = false;
      return;
    }
    if (controllerCredit(_handle, bytes)) {
      _runtime._credit.remove(_handle);
      _creditScheduled = false;
      return;
    }
    Timer.run(_drainCredit);
  }

  void _onOutput(Uint8List bytes) {
    if (_outputCancelled || _outputController.isClosed) {
      _queueCredit(bytes.length);
      return;
    }
    if (_paused) {
      _pendingOutput.addLast(bytes);
      return;
    }
    _deliverOutput(bytes);
  }

  void _deliverOutput(Uint8List bytes) {
    _pendingCredit = bytes.length;
    _outputController.add(bytes);
    if (!_paused) {
      _queueCredit(_pendingCredit);
      _pendingCredit = 0;
    }
  }

  void _completeExit() {
    if (_exit.isCompleted) {
      return;
    }
    final status = using((arena) {
      final pointer = arena<Int64>();
      return controllerExitStatus(_handle, pointer) ? pointer.value : null;
    });
    if (status != null) {
      _exit.complete(status);
    } else {
      _exit.completeError(
        const PtyExitException('native child status was unavailable'),
      );
    }
  }

  void _completeInputFailure() {
    _inputFailure ??= const PtyInputException(
      'accepted input was not fully written',
    );
  }

  PtyInputException _inputError(String operation, PtyInputException failure) =>
      PtyInputException(
        failure.message,
        operation: operation,
        nativeCode: failure.nativeCode,
        context: failure.context,
      );

  void _completeOutput() {
    if (!_outputDone.isCompleted) {
      _outputDone.complete();
    }
    if (!_outputController.isClosed) {
      final inputFailure = _inputFailure;
      if (inputFailure != null) {
        _outputController.addError(inputFailure);
      }
      unawaited(_outputController.close());
    }
  }

  void _completeOutputFailure() {
    if (!_outputDone.isCompleted) {
      _outputDone.complete();
    }
    if (!_outputController.isClosed) {
      _outputController.addError(
        const PtyOutputException('native terminal output read failed'),
      );
      unawaited(_outputController.close());
    }
  }

  void _fail(Object error) {
    _terminalFailure ??= error;
    final terminalFailure = _terminalFailure!;
    // A terminal infrastructure notice means native ownership has already
    // moved to forced cleanup. Keeping either finalizer attached would invoke
    // a stale native callback while the isolate itself is shutting down.
    _sessionFinalizer.detach(this);
    _sessionRegistryFinalizer.detach(this);
    if (!_exit.isCompleted) {
      _exit.completeError(error);
    }
    if (!_outputDone.isCompleted) {
      _outputDone.complete();
    }
    if (!_outputController.isClosed && !_terminalOutputScheduled) {
      _terminalOutputScheduled = true;
      scheduleMicrotask(() {
        if (!_outputController.isClosed) {
          _outputController.addError(terminalFailure);
          unawaited(_outputController.close());
        }
      });
    }
    _runtime._removeSession(_handle);
  }

  Future<void> _closeNative(Completer<void> completion) async {
    _cancelOutput();
    Object? failure;
    StackTrace? failureStack;
    void recordFailure(Object error, [StackTrace? stackTrace]) {
      failure ??= error;
      failureStack ??= stackTrace ?? StackTrace.current;
    }

    final terminalFailure = _terminalFailure;
    if (terminalFailure != null) {
      recordFailure(terminalFailure);
    }
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      recordFailure(inputFailure);
    }
    if (!_infrastructureLost && !_exit.isCompleted) {
      controllerSignal(_handle, ProcessSignal.sigterm.signalNumber);
      // Graceful delivery is advisory. The timed forced-close path below
      // remains authoritative and reports any cleanup failure.
    }
    var nativeCloseRequested = false;
    try {
      await _exit.future.timeout(_gracefulCloseTimeout);
    } on TimeoutException {
      nativeCloseRequested = true;
      if (!controllerClose(_handle)) {
        recordFailure(
          const PtyCloseException('forced native cleanup request failed'),
        );
      }
    } on Object catch (error, stackTrace) {
      recordFailure(error, stackTrace);
      nativeCloseRequested = true;
      if (!controllerClose(_handle)) {
        recordFailure(
          const PtyCloseException('forced native cleanup request failed'),
        );
      }
    }
    if (!nativeCloseRequested && !_infrastructureLost) {
      if (!controllerClose(_handle)) {
        recordFailure(const PtyCloseException('native cleanup request failed'));
      }
    }
    try {
      await Future.wait<Object?>([
        _exit.future,
        _outputDone.future,
      ]).timeout(_terminalDrainTimeout);
    } on TimeoutException catch (_, stackTrace) {
      recordFailure(
        const PtyCloseException(
          'native resources did not reach a terminal state',
        ),
        stackTrace,
      );
      controllerClose(_handle);
    } on Object catch (error, stackTrace) {
      recordFailure(error, stackTrace);
      controllerClose(_handle);
    }
    final delayedInputFailure = _inputFailure;
    if (delayedInputFailure != null) {
      if (_terminalFailure == null) {
        failure = delayedInputFailure;
        failureStack = StackTrace.current;
      } else {
        recordFailure(delayedInputFailure);
      }
    }

    var destroyed = controllerDestroy(_handle);
    for (var retry = 0; !destroyed && retry < 100; retry++) {
      await Future<void>.delayed(const Duration(milliseconds: 2));
      destroyed = controllerDestroy(_handle);
    }
    if (!destroyed) {
      recordFailure(
        const PtyCloseException(
          'native resources did not reach a terminal state',
        ),
      );
    }

    try {
      _runtime._removeSession(_handle);
      _stopModeObservation();
      await _modeController.close();
    } on Object catch (error, stackTrace) {
      recordFailure(error, stackTrace);
    }
    if (destroyed) {
      _sessionFinalizer.detach(this);
      _sessionRegistryFinalizer.detach(this);
    }
    if (failure case final Object error) {
      completion.completeError(error, failureStack);
    } else {
      completion.complete();
    }
  }

  PtyTermMode? _readMode() {
    final flags = controllerMode(_handle);
    if (flags < 0) {
      throw PtyModeException(
        'native terminal-mode query failed',
        nativeCode: _lastNativeCode(),
      );
    }
    return PtyTermMode(
      canonical: flags & 1 != 0,
      echo: flags & 2 != 0,
      signals: flags & 4 != 0,
    );
  }

  void _startModeObservation() {
    if (!capabilities.terminalModes || _modeTimer != null || _close != null) {
      return;
    }
    scheduleMicrotask(_observeMode);
  }

  void _stopModeObservation() {
    _modeTimer?.cancel();
    _modeTimer = null;
    _unchangedModeSamples = 0;
  }

  void _observeMode() {
    if (!_modeController.hasListener || _close != null) {
      _stopModeObservation();
      return;
    }
    PtyTermMode? current;
    try {
      current = _readMode();
    } on Object catch (error, stackTrace) {
      _modeController.addError(error, stackTrace);
      _stopModeObservation();
      return;
    }
    if (current != null && current != _lastMode) {
      _lastMode = current;
      _unchangedModeSamples = 0;
      _modeController.add(current);
    } else {
      _unchangedModeSamples++;
    }
    final milliseconds = switch (_unchangedModeSamples) {
      <= 10 => 10,
      <= 20 => 25,
      <= 40 => 50,
      <= 80 => 100,
      _ => 250,
    };
    _modeTimer = Timer(Duration(milliseconds: milliseconds), _observeMode);
  }
}

final class _ControllerRuntime {
  _ControllerRuntime._() {
    final abi = controllerAbiVersion();
    if (abi != controllerAbiVersionExpected) {
      throw PtyInfrastructureException(
        'native ABI mismatch: expected $controllerAbiVersionExpected, got $abi',
      );
    }
    if (!controllerInit(NativeApi.initializeApiDLData)) {
      throw const PtyInfrastructureException(
        'native controller initialization failed',
      );
    }
    // Output and terminal notices must share one port. Native sends them in
    // order, but Dart does not preserve ordering across separate ports; an EOF
    // notice could otherwise overtake the final output chunk.
    _notifications = RawReceivePort(_onNotification)..keepIsolateAlive = false;
  }

  static final instance = _ControllerRuntime._();

  late final RawReceivePort _notifications;
  final Map<int, WeakReference<NativeSession>> _sessions = {};
  final Map<int, int> _credit = {};
  SendPort? _supervisor;
  Future<SendPort>? _supervisorStart;
  var _pendingSpawns = 0;
  var _idleReleaseScheduled = false;

  int get notificationPort => _notifications.sendPort.nativePort;

  Future<SendPort> _beginSpawn() async {
    _pendingSpawns++;
    try {
      return await _ensureSupervisor();
    } on Object {
      _pendingSpawns--;
      rethrow;
    }
  }

  Future<SendPort> _ensureSupervisor() async {
    final supervisor = _supervisor;
    if (supervisor != null) {
      return supervisor;
    }
    final existing = _supervisorStart;
    if (existing != null) {
      return existing;
    }
    final starting = _startSupervisor();
    _supervisorStart = starting;
    try {
      return await starting;
    } on Object {
      if (identical(_supervisorStart, starting)) {
        _supervisorStart = null;
      }
      rethrow;
    }
  }

  Future<SendPort> _startSupervisor() async {
    final ready = ReceivePort();
    final armed = ReceivePort();
    SendPort? supervisor;
    var exitListenerInstalled = false;
    try {
      await Isolate.spawn(_superviseOwner, ready.sendPort);
      supervisor =
          await ready.first.timeout(_supervisorStartupTimeout) as SendPort;
      Isolate.current.addOnExitListener(
        supervisor,
        response: const [_supervisorOwnerExit],
      );
      exitListenerInstalled = true;
      supervisor.send([_supervisorArm, armed.sendPort]);
      await armed.first.timeout(_supervisorStartupTimeout);
      _supervisor = supervisor;
      return supervisor;
    } on Object {
      final failedSupervisor = supervisor;
      if (failedSupervisor != null) {
        failedSupervisor.send(const [_supervisorIdle]);
        if (exitListenerInstalled) {
          Isolate.current.removeOnExitListener(failedSupervisor);
        }
      }
      rethrow;
    } finally {
      armed.close();
      ready.close();
    }
  }

  void _finishSpawn() {
    _pendingSpawns--;
    _stopSupervisorIfIdle();
  }

  void _forgetSupervisedHandle(int handle) {
    _supervisor?.send([_supervisorRemove, handle]);
  }

  void _stopSupervisorIfIdle() {
    final supervisor = _supervisor;
    if (supervisor == null || _pendingSpawns != 0 || _sessions.isNotEmpty) {
      return;
    }
    supervisor.send(const [_supervisorIdle]);
    Isolate.current.removeOnExitListener(supervisor);
    _supervisor = null;
    _supervisorStart = null;
  }

  void _addSession(NativeSession session) {
    _sessions[session._handle] = WeakReference(session);
    _notifications.keepIsolateAlive = true;
  }

  void _removeSession(int handle) {
    if (_sessions.remove(handle) == null) {
      return;
    }
    _credit.remove(handle);
    if (_sessions.isEmpty) {
      _scheduleIdleRelease();
    }
    _forgetSupervisedHandle(handle);
  }

  void _scheduleIdleRelease() {
    if (_idleReleaseScheduled) return;
    _idleReleaseScheduled = true;
    Timer.run(() {
      _idleReleaseScheduled = false;
      if (_sessions.isEmpty && _pendingSpawns == 0) {
        _notifications.keepIsolateAlive = false;
        _stopSupervisorIfIdle();
      }
    });
  }

  void _onNotification(Object? message) {
    if (message case [final int handle, final Uint8List bytes]) {
      _session(handle)?._onOutput(bytes);
      return;
    }
    _onEvent(message);
  }

  void _onEvent(Object? message) {
    if (message is! int) {
      return;
    }
    final kind = message & 7;
    final payload = message >> 3;
    switch (kind) {
      case 0:
        _session(payload)?._completeInputFailure();
      case 1 || 2 || 5:
        return;
      case 3:
        _session(payload)?._completeExit();
      case 4:
        _session(payload)?._completeOutput();
      case 6:
        _session(payload)?._completeOutputFailure();
      case 7:
        final session = _session(payload);
        if (session != null) {
          session._infrastructureLost = true;
          session._fail(
            const PtyInfrastructureException(
              'native PTY infrastructure terminated',
            ),
          );
        }
    }
  }

  NativeSession? _session(int handle) {
    final session = _sessions[handle]?.target;
    if (session == null) {
      _removeSession(handle);
    }
    return session;
  }
}

({int handle, int? nativeCode}) _spawnNative(
  PtySpawnOptions options,
  String workingDirectory,
) {
  return using((arena) {
    Pointer<Char> nativeString(String value) {
      if (value.contains('\u0000')) {
        throw ArgumentError.value(value, 'value', 'must not contain NUL');
      }
      final bytes = utf8.encode(value);
      final result = arena<Char>(bytes.length + 1);
      result.cast<Uint8>().asTypedList(bytes.length + 1)
        ..setRange(0, bytes.length, bytes)
        ..[bytes.length] = 0;
      return result;
    }

    Pointer<Pointer<Char>> nativeStrings(List<String> values) {
      if (values.isEmpty) {
        return nullptr;
      }
      final result = arena<Pointer<Char>>(values.length);
      for (var index = 0; index < values.length; index++) {
        result[index] = nativeString(values[index]);
      }
      return result;
    }

    final executable = nativeString(options.executable);
    final arguments = nativeStrings(options.arguments);
    final inheritEnvironment =
        options.environmentMode == PtyEnvironmentMode.inherit;
    final environment = _effectiveEnvironment(options);
    for (final key in environment.keys) {
      if (key.isEmpty || key.contains('=') || key.contains('\u0000')) {
        throw ArgumentError.value(key, 'environment key', 'is invalid');
      }
    }
    final environmentValues = environment.entries
        .map((entry) => '${entry.key}=${entry.value}')
        .toList(growable: false);
    final nativeEnvironment = nativeStrings(environmentValues);
    final cwd = nativeString(workingDirectory);
    final handle = controllerSpawn(
      executable,
      arguments,
      options.arguments.length,
      nativeEnvironment,
      environmentValues.length,
      inheritEnvironment,
      cwd,
      options.initialSize.rows,
      options.initialSize.columns,
      options.initialSize.pixelWidth,
      options.initialSize.pixelHeight,
      options.maxBufferedInput,
      options.maxBufferedOutput,
    );
    final nativeCode = handle == 0 ? controllerLastErrorCode() : 0;
    return (handle: handle, nativeCode: nativeCode == 0 ? null : nativeCode);
  });
}

void _validateSpawnOptions(PtySpawnOptions options, String workingDirectory) {
  _validateSize(options.initialSize, operation: 'spawn');
  if (options.arguments.length > 256) {
    throw PtyInvalidArgumentException(
      'process spawn accepts at most 256 arguments',
      operation: 'spawn',
      context: '${options.arguments.length} arguments',
    );
  }
  if (options.maxBufferedInput <= 0 ||
      options.maxBufferedInput > 64 * 1024 * 1024) {
    throw PtyInvalidArgumentException(
      'maxBufferedInput must be between 1 byte and 64 MiB',
      operation: 'spawn',
      context: '${options.maxBufferedInput} bytes',
    );
  }
  if (options.maxBufferedOutput <= 0 ||
      options.maxBufferedOutput > 64 * 1024 * 1024) {
    throw PtyInvalidArgumentException(
      'maxBufferedOutput must be between 1 byte and 64 MiB',
      operation: 'spawn',
      context: '${options.maxBufferedOutput} bytes',
    );
  }
  if (options.gracefulCloseTimeout.isNegative ||
      options.gracefulCloseTimeout > const Duration(minutes: 1)) {
    throw const PtyInvalidArgumentException(
      'gracefulCloseTimeout must be between zero and one minute',
      operation: 'spawn',
    );
  }
  if (options.executable.isEmpty || options.executable.contains('\u0000')) {
    throw const PtyInvalidArgumentException(
      'invalid executable in process spawn options',
      operation: 'spawn',
    );
  }
  for (final argument in options.arguments) {
    if (argument.contains('\u0000')) {
      throw const PtyInvalidArgumentException(
        'invalid argument in process spawn options',
        operation: 'spawn',
      );
    }
  }
  if (options.workingDirectory?.contains('\u0000') ?? false) {
    throw const PtyInvalidArgumentException(
      'invalid working directory in process spawn options',
      operation: 'spawn',
    );
  }
  final environment = _effectiveEnvironment(options);
  if (environment.length > 4096) {
    throw PtyInvalidArgumentException(
      'process spawn accepts at most 4096 environment entries',
      operation: 'spawn',
      context: '${environment.length} entries',
    );
  }
  for (final entry in environment.entries) {
    if (entry.key.isEmpty ||
        entry.key.contains('=') ||
        entry.key.contains('\u0000') ||
        entry.value.contains('\u0000')) {
      throw const PtyInvalidArgumentException(
        'invalid environment entry in process spawn options',
        operation: 'spawn',
      );
    }
  }
  var encodedPayloadBytes =
      36 + 4 * (1 + options.arguments.length + environment.length);
  encodedPayloadBytes += utf8.encode(options.executable).length;
  for (final argument in options.arguments) {
    encodedPayloadBytes += utf8.encode(argument).length;
  }
  for (final entry in environment.entries) {
    encodedPayloadBytes += utf8.encode('${entry.key}=${entry.value}').length;
  }
  encodedPayloadBytes += utf8.encode(workingDirectory).length;
  if (encodedPayloadBytes > 64 * 1024) {
    throw PtyInvalidArgumentException(
      'encoded process spawn payload exceeds 64 KiB',
      operation: 'spawn',
      context: '$encodedPayloadBytes bytes',
    );
  }
}

void _validateSize(PtySize size, {required String operation}) {
  final maximumCells = Platform.isWindows ? 32767 : 65535;
  if (size.rows <= 0 ||
      size.rows > maximumCells ||
      size.columns <= 0 ||
      size.columns > maximumCells ||
      size.pixelWidth < 0 ||
      size.pixelWidth > 65535 ||
      size.pixelHeight < 0 ||
      size.pixelHeight > 65535) {
    throw PtyInvalidArgumentException(
      'terminal dimensions are outside native bounds',
      operation: operation,
    );
  }
}

Map<String, String> _effectiveEnvironment(PtySpawnOptions options) =>
    switch (options.environmentMode) {
      PtyEnvironmentMode.inherit || PtyEnvironmentMode.clear => const {},
      PtyEnvironmentMode.overlay => {
        ...Platform.environment,
        ...options.environment,
      },
      PtyEnvironmentMode.replace => options.environment,
    };
