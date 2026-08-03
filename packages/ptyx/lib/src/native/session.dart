part of 'native.dart';

const _maxDartWriteBytes = 1024 * 1024;

final class _NativeSession implements PtyxFinalizable, PtySession {
  static final _finalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_session_finalize),
  );
  final _NativeRuntime _controller;
  final int _handle;
  final int _inputCapacity;
  late final _NativeOutput _output;
  late final StreamController<PtyTermMode> _modeController;
  final PtyCapabilities _capabilities;
  final _exit = Completer<int>();
  Completer<void>? _close;
  PtyInputException? _inputFailure;
  Object? _terminalFailure;
  PtyTermMode? _lastMode;

  _NativeSession._(_NativeRuntime controller, this._handle, this._inputCapacity)
    : _controller = controller,
      _capabilities = ptyxCapabilitiesFromBits(controller.capabilityBits) {
    _output = _NativeOutput(
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
          completion.completeError(ptyxException(failure, operation: 'spawn'));
        },
      );
    } on _NativeFailure catch (failure) {
      throw ptyxException(failure, operation: 'spawn');
    }
    return completion.future;
  }

  @override
  PtyCapabilities get capabilities => _capabilities;

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
    final modes = _snapshot('mode').modes;
    return modes == null ? null : ptyxModeFromBits(modes);
  }

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _output.stream;

  @override
  int? get pid {
    return _snapshot('pid').pid;
  }

  @override
  PtySize get size {
    final snapshot = _snapshot('size');
    return PtySize(
      rows: snapshot.rows,
      columns: snapshot.columns,
      pixelWidth: snapshot.pixelWidth,
      pixelHeight: snapshot.pixelHeight,
    );
  }

  @override
  String? get ttyName {
    if (!capabilities.terminalName) {
      return null;
    }
    final bytes = _snapshot('ttyName').terminalName;
    return bytes == null ? null : utf8.decode(bytes);
  }

  @override
  void write(Uint8List data) {
    const operation = 'write';
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw ptyxInputErrorForOperation(operation, inputFailure);
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
    } on _NativeFailure catch (failure) {
      final error = ptyxException(failure, operation: operation);
      if (error is PtyInputException) {
        _inputFailure ??= error;
        throw ptyxInputErrorForOperation(operation, _inputFailure!);
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
    } on _NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'kill');
    }
  }

  @override
  void resize(PtySize size) {
    try {
      sessionResize(_handle, size);
    } on _NativeFailure catch (failure) {
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
      final publicError = error is _NativeFailure
          ? ptyxException(error, operation: 'close')
          : error;
      if (error is _NativeFailure &&
          publicError is PtyInfrastructureException) {
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

  void _nativeInputFailed(_NativeFailure failure) {
    final error = ptyxException(failure);
    if (error is PtyInputException) {
      _inputFailure ??= error;
      return;
    }
    _nativeInfrastructureFailed(failure);
  }

  void _nativeOutputFailed(_NativeFailure failure) {
    _output.fail(ptyxException(failure));
  }

  void _nativeInfrastructureFailed(_NativeFailure failure) {
    final error = ptyxException(failure);
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

  void _nativeExitFailed(_NativeFailure failure) {
    if (!_exit.isCompleted) {
      _exit.completeError(ptyxException(failure));
    }
  }

  void _nativeCloseComplete(_NativeFailure? failure) {
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
      close.completeError(ptyxException(failure, operation: 'close'));
      return;
    }
    close.complete();
  }

  void _nativeModeChanged(int modes) {
    final mode = ptyxModeFromBits(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  void _nativeModeFailed(_NativeFailure failure) {
    if (!_modeController.isClosed) {
      _modeController.addError(
        ptyxException(failure, operation: 'modeChanges.observe'),
      );
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

  _NativeSnapshot _snapshot(String operation) {
    try {
      return sessionSnapshot(_handle);
    } on _NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: operation);
    }
  }

  void _cancelOutput() {
    try {
      _output.cancel();
    } on _NativeFailure catch (failure, stackTrace) {
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
    } on _NativeFailure catch (failure, stackTrace) {
      final error = _operationFailure(failure, operation: operation);
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  PtyException _operationFailure(
    _NativeFailure failure, {
    required String operation,
  }) {
    final error = ptyxException(failure, operation: operation);
    if (error is PtyInfrastructureException) {
      _nativeInfrastructureFailed(failure);
    }
    return error;
  }
}

/// Projects the native output lease into Dart's single-subscription stream.
///
/// Native owns the queue and output credit. This class owns only the one
/// delivery lease needed to bridge native backpressure to StreamController
/// pause, resume, cancellation, and terminal delivery.
final class _NativeOutput {
  final void Function() _onNativeCancel;
  final void Function(int token) _onAcknowledge;
  final void Function(_NativeFailure failure) _onInfrastructureFailure;
  late final StreamController<Uint8List> _controllerStream;
  ({Uint8List bytes, int token})? _pending;
  ({Object? error, StackTrace? stackTrace})? _termination;
  var _paused = true;
  var _cancelled = false;

  _NativeOutput({
    required void Function() onCancel,
    required void Function() onNativeCancel,
    required void Function(int token) onAcknowledge,
    required void Function(_NativeFailure failure) onInfrastructureFailure,
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
          ptyxOutputInfrastructureFailure(
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
    } on _NativeFailure catch (failure) {
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
