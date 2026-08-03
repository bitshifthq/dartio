part of 'native.dart';

const _maxDartWriteBytes = 1024 * 1024;

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
  Object? _terminalFailure;
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
      onListen: () => _observeModes(true, 'modeChanges.listen'),
      onCancel: () => _observeModes(false, 'modeChanges.cancel'),
    );
    _exit.future.ignore();
    _finalizer.attach(
      this,
      Pointer<Void>.fromAddress(_handle),
      detach: this,
      externalSize: 64 * 1024,
    );
  }

  static Future<_NativeSession> spawn(PtySpawnOptions options) async {
    final request = _snapshotSpawnRequest(options);
    final controller = await _NativeRuntime.instance;

    final completion = Completer<_NativeSession>();
    try {
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
        onFailure: (failure) {
          completion.completeError(
            exceptionFromFailure(failure, operation: 'spawn'),
          );
        },
      );
    } on NativeFailure catch (failure) {
      throw exceptionFromFailure(failure, operation: 'spawn');
    }
    return completion.future;
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
    try {
      return sessionMode(_handle);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'mode');
    }
  }

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _output.stream;

  @override
  int? get pid {
    try {
      return sessionPid(_handle);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'pid');
    }
  }

  @override
  PtySize get size {
    try {
      return sessionSize(_handle);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'size');
    }
  }

  @override
  String? get ttyName {
    if (!capabilities.terminalName) {
      return null;
    }
    try {
      return sessionTtyName(_handle);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'ttyName');
    }
  }

  @override
  void write(Uint8List data) {
    const operation = 'write';
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw inputException(previous: inputFailure, operation: operation);
    }
    if (data.isEmpty) {
      throw const PtyInvalidArgumentException(
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
    if (data.length > _maxDartWriteBytes) {
      throw PtyInvalidArgumentException(
        'input writes may contain at most 1 MiB per invocation',
        operation: operation,
        context: '${data.length} bytes',
      );
    }
    try {
      sessionWrite(_handle, data);
    } on NativeFailure catch (failure) {
      final error = exceptionFromFailure(failure, operation: operation);
      if (error is PtyInputException) {
        _inputFailure ??= error;
        throw inputException(previous: _inputFailure, operation: operation);
      }
      if (error is PtyInfrastructureException) {
        _nativeInfrastructureFailed(failure);
      }
      throw error;
    }
  }

  @override
  bool kill([ProcessSignal signal = .sigterm]) {
    if (_close != null || _exit.isCompleted) {
      return false;
    }
    try {
      return sessionTerminate(_handle, signal.signalNumber);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'kill');
    }
  }

  @override
  void resize(PtySize size) {
    try {
      sessionResize(_handle, size);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'resize');
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
    final terminalFailure = _terminalFailure;
    if (terminalFailure != null) {
      completion.completeError(terminalFailure);
      return completion.future;
    }
    try {
      _cancelOutput();
      sessionClose(_handle);
    } on Object catch (error, stackTrace) {
      if (completion.isCompleted) {
        return completion.future;
      }
      final publicError = error is NativeFailure
          ? exceptionFromFailure(error, operation: 'close')
          : error;
      if (error is NativeFailure && publicError is PtyInfrastructureException) {
        _nativeInfrastructureFailed(error);
        return completion.future;
      }
      _failReleasedSession(publicError, stackTrace);
    }
    return completion.future;
  }

  void _nativeOutput(Uint8List bytes, int token) {
    _output.deliver(bytes, token);
  }

  void _nativeInputFailed(NativeFailure failure) {
    final error = exceptionFromFailure(failure);
    if (error is PtyInputException) {
      _inputFailure ??= error;
      return;
    }
    _nativeInfrastructureFailed(failure);
  }

  void _nativeOutputFailed(NativeFailure failure) {
    _output.fail(exceptionFromFailure(failure));
  }

  void _nativeInfrastructureFailed(NativeFailure failure) {
    final error = exceptionFromFailure(failure);
    if (_terminalFailure != null) {
      return;
    }
    _terminalFailure = error;
    _failReleasedSession(error);
  }

  void _nativeOutputDone() {
    _output.complete(_inputFailure);
  }

  void _nativeExit(int status) {
    if (!_exit.isCompleted) {
      _exit.complete(status);
    }
  }

  void _nativeExitFailed(NativeFailure failure) {
    if (!_exit.isCompleted) {
      _exit.completeError(exceptionFromFailure(failure));
    }
  }

  void _nativeCloseComplete(NativeFailure? failure) {
    _finalizer.detach(this);
    _output.close();
    if (!_modeController.isClosed) {
      unawaited(_modeController.close());
    }
    if (!_exit.isCompleted) {
      _exit.completeError(
        const PtyExitException(
          'session closed before the child status was observed',
        ),
      );
    }

    final close = _close ??= Completer<void>();
    if (close.isCompleted) {
      return;
    }
    final terminalFailure = _terminalFailure;
    if (terminalFailure != null) {
      close.completeError(terminalFailure);
      return;
    }
    if (failure != null) {
      close.completeError(exceptionFromFailure(failure, operation: 'close'));
      return;
    }
    close.complete();
  }

  void _nativeModeChanged(int modes) {
    final mode = modeFromBits(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  void _nativeModeFailed(NativeFailure failure) {
    if (!_modeController.isClosed) {
      _modeController.addError(
        exceptionFromFailure(failure, operation: 'modeChanges.observe'),
      );
    }
  }

  void _onNativeEvent(NativeEvent event) {
    switch (event.kind) {
      case .output:
        if (event.data case final bytes?) {
          _nativeOutput(bytes, event.token);
        }
      case .inputFailed:
        _nativeInputFailed(_eventFailure(event));
      case .outputFailed:
        _nativeOutputFailed(_eventFailure(event));
      case .infrastructureFailed:
        _nativeInfrastructureFailed(_eventFailure(event));
      case .outputDone:
        _nativeOutputDone();
      case .exit:
        _nativeExit(event.value);
      case .exitFailed:
        _nativeExitFailed(_eventFailure(event));
      case .closeComplete:
        _nativeCloseComplete(event.failure);
      case .modeChanged:
        _nativeModeChanged(event.value);
      case .modeFailed:
        _nativeModeFailed(_eventFailure(event));
      case .spawnReady || .spawnFailed:
        break;
    }
  }

  void _failReleasedSession(Object error, [StackTrace? stackTrace]) {
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

  void _cancelOutput() {
    try {
      _output.cancel();
    } on NativeFailure catch (failure, stackTrace) {
      Error.throwWithStackTrace(
        _operationFailure(failure, operation: 'output.cancel'),
        stackTrace,
      );
    }
  }

  void _observeModes(bool enabled, String operation) {
    if (!capabilities.terminalModes || _close != null) {
      return;
    }
    try {
      sessionObserveMode(_handle, enabled: enabled);
    } on NativeFailure catch (failure, stackTrace) {
      final error = _operationFailure(failure, operation: operation);
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  PtyException _operationFailure(
    NativeFailure failure, {
    required String operation,
  }) {
    final error = exceptionFromFailure(failure, operation: operation);
    if (error is PtyInfrastructureException) {
      _nativeInfrastructureFailed(failure);
    }
    return error;
  }
}

NativeFailure _eventFailure(NativeEvent event) =>
    event.failure ??
    syntheticFailure(
      operation: switch (event.kind) {
        .inputFailed => ptyx_operation.PTYX_OPERATION_WRITE,
        .outputFailed => ptyx_operation.PTYX_OPERATION_OUTPUT,
        .infrastructureFailed => ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN,
        .exitFailed => ptyx_operation.PTYX_OPERATION_EXIT,
        .modeFailed => ptyx_operation.PTYX_OPERATION_TERMINAL_MODE,
        _ => ptyx_operation.PTYX_OPERATION_NONE,
      },
      kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
      message: 'native event omitted its failure details',
    );

/// Projects the native output lease into Dart's single-subscription stream.
///
/// Native owns the queue and output credit. This class owns only the one
/// delivery lease needed to bridge native backpressure to StreamController
/// pause, resume, cancellation, and terminal delivery.
final class _OutputLease {
  final void Function() _onNativeCancel;
  final void Function(int token) _onAcknowledge;
  final void Function(NativeFailure failure) _onInfrastructureFailure;
  late final StreamController<Uint8List> _controllerStream;
  ({Uint8List bytes, int token})? _pending;
  ({Object? error, StackTrace? stackTrace})? _termination;
  var _paused = true;
  var _cancelled = false;

  _OutputLease({
    required void Function() onCancel,
    required void Function() onNativeCancel,
    required void Function(int token) onAcknowledge,
    required void Function(NativeFailure failure) onInfrastructureFailure,
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

  void deliver(Uint8List bytes, int token) {
    if (_cancelled || _controllerStream.isClosed) {
      _acknowledge(token);
      return;
    }
    if (_paused || !_controllerStream.hasListener) {
      if (_pending != null) {
        _acknowledge(token);
        _onInfrastructureFailure(
          syntheticFailure(
            operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
            kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
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

  void complete(Object? error, {StackTrace? stackTrace}) =>
      fail(error, stackTrace: stackTrace);

  void fail(Object? error, {bool force = false, StackTrace? stackTrace}) {
    _termination ??= (error: error, stackTrace: stackTrace);
    if (force) _discardPending();
    _completeIfReady();
  }

  void close() {
    _discardPending();
    fail(null, force: true);
  }

  void cancel() {
    if (_cancelled) return;
    _cancelled = true;
    _paused = false;
    if (_termination == null) _onNativeCancel();
    _discardPending();
    _completeIfReady();
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

  void _acknowledge(int token) {
    if (token == 0) return;
    try {
      _onAcknowledge(token);
    } on NativeFailure catch (failure) {
      _onInfrastructureFailure(failure);
    }
  }

  void _discardPending() {
    final event = _takePending();
    if (event != null) _acknowledge(event.token);
  }

  ({Uint8List bytes, int token})? _takePending() {
    final event = _pending;
    _pending = null;
    return event;
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
}

_NativeSpawnRequest _snapshotSpawnRequest(PtySpawnOptions options) {
  final effectiveEnvironment = _effectiveEnvironment(options);
  return (
    executable: options.executable,
    arguments: List.unmodifiable(options.arguments),
    environment: List.unmodifiable([
      for (final entry in effectiveEnvironment.entries)
        '${entry.key}=${entry.value}',
    ]),
    inheritEnvironment: options.environmentMode == .inherit,
    workingDirectory: options.workingDirectory ?? Directory.current.path,
    rows: options.initialSize.rows,
    columns: options.initialSize.columns,
    pixelWidth: options.initialSize.pixelWidth,
    pixelHeight: options.initialSize.pixelHeight,
    inputCapacity: options.maxBufferedInput,
    outputCapacity: options.maxBufferedOutput,
    gracefulCloseTimeout: options.gracefulCloseTimeout,
  );
}

Map<String, String> _effectiveEnvironment(PtySpawnOptions options) =>
    switch (options.environmentMode) {
      .inherit || .clear => const {},
      .overlay => {...Platform.environment, ...options.environment},
      .replace => options.environment,
    };
