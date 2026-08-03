part of 'native.dart';

const _maxDartWriteBytes = 1024 * 1024;

Map<String, String> _effectiveEnvironment(PtySpawnOptions options) =>
    switch (options.environmentMode) {
      .inherit || .clear => const {},
      .overlay => {...Platform.environment, ...options.environment},
      .replace => options.environment,
    };

PtyException _eventError(NativeEvent event) =>
    event.error ??
    syntheticError(
      operation: 'controller',
      message: 'native event omitted its failure details',
    );

SpawnRequest _snapshotSpawnRequest(PtySpawnOptions options) {
  final effectiveEnvironment = _effectiveEnvironment(options);
  return SpawnRequest(
    executable: options.executable,
    arguments: List<String>.unmodifiable(options.arguments),
    environment: List<String>.unmodifiable([
      for (final entry in effectiveEnvironment.entries)
        '${entry.key}=${entry.value}',
    ]),
    inheritEnvironment: options.environmentMode == .inherit,
    workingDirectory: options.workingDirectory ?? Directory.current.path,
    size: options.initialSize,
    inputCapacity: options.maxBufferedInput,
    outputCapacity: options.maxBufferedOutput,
    gracefulCloseTimeout: options.gracefulCloseTimeout,
  );
}

final class _NativeSession implements PtyxFinalizable, PtySession {
  static final _finalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_session_finalize),
  );
  final _NativeRuntime _controller;
  final int _handle;
  final int _inputCapacity;
  late final _OutputLease _output;
  late final StreamController<PtyTermMode> _modeController;
  final _exit = Completer<int>();
  Completer<void>? _close;
  PtyInputException? _inputFailure;
  PtyException? _terminalFailure;
  PtyTermMode? _lastMode;

  _NativeSession._(_NativeRuntime controller, this._handle, this._inputCapacity)
    : _controller = controller {
    _output = _OutputLease(
      onCancel: _cancelOutput,
      onNativeCancel: () => sessionCancelOutput(_handle),
      onAcknowledge: controller.acknowledge,
      onInfrastructureFailure: _nativeInfrastructureFailed,
    );
    _modeController = StreamController<PtyTermMode>.broadcast(
      sync: true,
      onListen: () => _observeModes(true),
      onCancel: () => _observeModes(false),
    );
    _exit.future.ignore();
    _finalizer.attach(
      this,
      Pointer<Void>.fromAddress(_handle),
      detach: this,
      externalSize: 64 * 1024,
    );
  }

  @override
  PtyCapabilities get capabilities => _controller.capabilities;

  @override
  Future<int> get exitCode => _exit.future;

  @override
  Future<PtyExitStatus> get exitStatus async {
    final code = await _exit.future;
    return code >= 0 ? PtyExited(code) : PtySignaled(-code);
  }

  @override
  PtyTermMode? get mode {
    if (!capabilities.terminalModes) return null;
    return sessionMode(_handle);
  }

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _output.stream;

  @override
  int? get pid {
    return sessionPid(_handle);
  }

  @override
  PtySize get size {
    return sessionSize(_handle);
  }

  @override
  String? get ttyName {
    if (!capabilities.terminalName) return null;

    return sessionTtyName(_handle);
  }

  @override
  Future<void> close() {
    final existing = _close;
    if (existing != null) {
      return existing.future;
    }
    final completion = Completer<void>();
    _close = completion;
    final terminalFailure = _terminalFailure;
    if (terminalFailure != null) {
      completion.completeError(terminalFailure);
      return completion.future;
    }
    try {
      _cancelOutput();
      sessionClose(_handle);
    } on PtyException catch (error, stackTrace) {
      if (completion.isCompleted) {
        return completion.future;
      }
      if (error is PtyInfraException) {
        _nativeInfrastructureFailed(error);
        return completion.future;
      }
      _failReleasedSession(error, stackTrace);
    }
    return completion.future;
  }

  @override
  bool kill([ProcessSignal signal = .sigterm]) {
    if (_close != null || _exit.isCompleted) return false;

    return sessionTerminate(_handle, signal.signalNumber);
  }

  @override
  void resize(PtySize size) {
    sessionResize(_handle, size);
  }

  @override
  void write(Uint8List data) {
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw inputFailure;
    }
    if (data.isEmpty) {
      throw ArgumentError.value(data, 'data', 'input data must not be empty');
    }
    if (data.length > _inputCapacity) {
      throw ArgumentError.value(
        data.length,
        'data.length',
        'input data exceeds this session input capacity',
      );
    }
    if (data.length > _maxDartWriteBytes) {
      throw ArgumentError.value(
        data.length,
        'data.length',
        'input writes may contain at most 1 MiB per invocation',
      );
    }
    try {
      sessionWrite(_handle, data);
    } on PtyInputException catch (error) {
      _inputFailure ??= error;
      throw _inputFailure!;
    } on PtyInfraException catch (error) {
      _nativeInfrastructureFailed(error);
      rethrow;
    }
  }

  void _cancelOutput() {
    _output.cancel();
  }

  void _failReleasedSession(PtyException error, [StackTrace? stackTrace]) {
    _terminalFailure ??= error;
    if (!_exit.isCompleted) _exit.completeError(error, stackTrace);
    _output.fail(error, force: true, stackTrace: stackTrace);
    if (!_modeController.isClosed) {
      _modeController.addError(error, stackTrace);
      unawaited(_modeController.close());
    }
    final close = _close;
    if (close != null && !close.isCompleted) {
      close.completeError(error, stackTrace);
    }
    _finalizer.detach(this);
    _controller.releaseSession(_handle);
  }

  void _nativeCloseComplete(PtyException? error) {
    _finalizer.detach(this);
    _output.close();
    if (!_modeController.isClosed) unawaited(_modeController.close());

    if (!_exit.isCompleted) {
      _exit.completeError(
        const PtyException(
          'session closed before the child status was observed',
          operation: 'exit',
          category: .process,
        ),
      );
    }

    final close = _close ??= Completer<void>();
    if (close.isCompleted) return;

    final terminalFailure = _terminalFailure;
    if (terminalFailure != null) {
      close.completeError(terminalFailure);
      return;
    }
    if (error != null) {
      close.completeError(error);
      return;
    }
    close.complete();
  }

  void _nativeExit(int status) {
    if (!_exit.isCompleted) _exit.complete(status);
  }

  void _nativeExitFailed(PtyException error) {
    if (!_exit.isCompleted) _exit.completeError(error);
  }

  void _nativeInfrastructureFailed(PtyException error) {
    if (_terminalFailure != null) return;

    _terminalFailure = error;
    _failReleasedSession(error);
  }

  void _nativeInputFailed(PtyException error) {
    if (error is PtyInputException) {
      _inputFailure ??= error;
      return;
    }
    _nativeInfrastructureFailed(error);
  }

  void _nativeModeChanged(int modes) {
    final mode = modeFromBits(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  void _nativeModeFailed(PtyException error) {
    if (!_modeController.isClosed) {
      _modeController.addError(error);
    }
  }

  void _nativeOutput(Uint8List bytes, int token) {
    _output.deliver(bytes, token);
  }

  void _nativeOutputDone() => _output.complete(_inputFailure);

  void _nativeOutputFailed(PtyException error) {
    _output.fail(error);
  }

  void _observeModes(bool enabled) {
    if (!capabilities.terminalModes || _close != null) {
      return;
    }
    try {
      sessionObserveMode(_handle, enabled: enabled);
    } on PtyException catch (error, stackTrace) {
      if (error is PtyInfraException) _nativeInfrastructureFailed(error);
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  void _onNativeEvent(NativeEvent event) {
    switch (event.kind) {
      case .output:
        if (event.data case final bytes?) {
          _nativeOutput(bytes, event.token);
        }
      case .inputFailed:
        _nativeInputFailed(_eventError(event));
      case .outputFailed:
        _nativeOutputFailed(_eventError(event));
      case .infrastructureFailed:
        _nativeInfrastructureFailed(_eventError(event));
      case .outputDone:
        _nativeOutputDone();
      case .exit:
        _nativeExit(event.value);
      case .exitFailed:
        _nativeExitFailed(_eventError(event));
      case .closeComplete:
        _nativeCloseComplete(event.error);
      case .modeChanged:
        _nativeModeChanged(event.value);
      case .modeFailed:
        _nativeModeFailed(_eventError(event));
      case .spawnReady || .spawnFailed:
        break;
    }
  }

  static Future<_NativeSession> spawn(PtySpawnOptions options) async {
    final request = _snapshotSpawnRequest(options);
    final controller = await _NativeRuntime.instance;

    final completion = Completer<_NativeSession>();
    controller.startSpawn(
      request,
      onReady: (handle) {
        final session = _NativeSession._(
          controller,
          handle,
          request.inputCapacity,
        );
        completion.complete(session);
        return session;
      },
      onFailure: completion.completeError,
    );
    return completion.future;
  }
}

/// Projects the native output lease into Dart's single-subscription stream.
///
/// Native owns the queue and output credit. This class owns only the one
/// delivery lease needed to bridge native backpressure to StreamController
/// pause, resume, cancellation, and terminal delivery.
final class _OutputLease {
  final void Function() _onNativeCancel;
  final void Function(int token) _onAcknowledge;
  final void Function(PtyException error) _onInfrastructureFailure;
  late final StreamController<Uint8List> _controllerStream;
  ({Uint8List bytes, int token})? _pending;
  ({Object? error, StackTrace? stackTrace})? _termination;
  var _paused = true;
  var _cancelled = false;

  _OutputLease({
    required void Function() onCancel,
    required void Function() onNativeCancel,
    required void Function(int token) onAcknowledge,
    required void Function(PtyException error) onInfrastructureFailure,
  }) : _onNativeCancel = onNativeCancel,
       _onAcknowledge = onAcknowledge,
       _onInfrastructureFailure = onInfrastructureFailure {
    _controllerStream = StreamController<Uint8List>(
      sync: true,
      onListen: _resume,
      onPause: _pause,
      onResume: _resume,
      onCancel: onCancel,
    );
  }

  Stream<Uint8List> get stream => _controllerStream.stream;

  void cancel() {
    if (_cancelled) return;
    _cancelled = true;
    _paused = false;
    if (_termination == null) _onNativeCancel();
    _discardPending();
    _completeIfReady();
  }

  void close() {
    _discardPending();
    fail(null, force: true);
  }

  void complete(Object? error, {StackTrace? stackTrace}) =>
      fail(error, stackTrace: stackTrace);

  void deliver(Uint8List bytes, int token) {
    if (_cancelled || _controllerStream.isClosed) {
      _acknowledge(token);
      return;
    }
    if (_paused || !_controllerStream.hasListener) {
      if (_pending != null) {
        _acknowledge(token);
        _onInfrastructureFailure(
          syntheticError(
            operation: 'controller',
            message: 'native output exceeded the one-event delivery lease',
          ),
        );
        return;
      }
      _pending = (bytes: bytes, token: token);
      return;
    }
    _controllerStream.add(bytes);
    _acknowledge(token);
  }

  void fail(Object? error, {bool force = false, StackTrace? stackTrace}) {
    _termination ??= (error: error, stackTrace: stackTrace);
    if (force) _discardPending();
    _completeIfReady();
  }

  void _acknowledge(int token) {
    if (token == 0) return;
    try {
      _onAcknowledge(token);
    } on PtyException catch (error) {
      _onInfrastructureFailure(error);
    }
  }

  void _completeIfReady() {
    final termination = _termination;
    if (termination == null || _pending != null || _controllerStream.isClosed) {
      return;
    }
    if (termination.error != null && !_cancelled) {
      _controllerStream.addError(termination.error!, termination.stackTrace);
    }
    unawaited(_controllerStream.close());
  }

  void _discardPending() {
    final event = _takePending();
    if (event != null) _acknowledge(event.token);
  }

  void _pause() => _paused = true;

  void _resume() {
    if (_cancelled) return;
    _paused = false;
    final event = _takePending();
    if (event != null) {
      _controllerStream.add(event.bytes);
      _acknowledge(event.token);
    }
    _completeIfReady();
  }

  ({Uint8List bytes, int token})? _takePending() {
    final event = _pending;
    _pending = null;
    return event;
  }
}
