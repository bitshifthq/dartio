part of 'native.dart';

const _maxDartWriteBytes = 1024 * 1024;

final class _NativeSession implements Finalizable, PtySession {
  final _NativeRuntime _controller;
  final int _handle;
  final int _inputCapacity;
  late final StreamController<Uint8List> _outputController;
  late final StreamController<PtyTermMode> _modeController;
  final PtyCapabilities _capabilities;
  final _exit = Completer<int>();
  ({Uint8List bytes, int token})? _pendingOutput;
  Completer<void>? _close;
  PtyInputException? _inputFailure;
  Object? _terminalFailure;
  ({Object? error, StackTrace? stackTrace})? _outputTermination;
  var _paused = true;
  var _outputCancelled = false;
  PtyTermMode? _lastMode;

  _NativeSession._(_NativeRuntime controller, this._handle, this._inputCapacity)
    : _controller = controller,
      _capabilities = _capabilitiesFromBits(controller.capabilityBits) {
    // Synchronous delivery preserves FIFO ordering and avoids an extra event
    // turn for each native output chunk. Callbacks are therefore reentrant.
    _outputController = StreamController<Uint8List>(
      sync: true,
      onListen: _resumeOutput,
      onPause: _pauseOutput,
      onResume: _resumeOutput,
      onCancel: _cancelOutput,
    );
    _modeController = StreamController<PtyTermMode>.broadcast(
      sync: true,
      onListen: () => _observeModes(true, 'modeChanges.listen'),
      onCancel: () => _observeModes(false, 'modeChanges.cancel'),
    );
    _exit.future.ignore();
    _controller.attachFinalizer(this, _handle);
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
          completion.completeError(_exception(failure, operation: 'spawn'));
        },
      );
    } on _NativeFailure catch (failure) {
      throw _exception(failure, operation: 'spawn');
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
    _checkOpen('mode');
    if (!capabilities.terminalModes) {
      return null;
    }
    final modes = _snapshot('mode').modes;
    return modes == null ? null : _mode(modes);
  }

  @override
  Stream<PtyTermMode> get modeChanges => _modeController.stream;

  @override
  Stream<Uint8List> get output => _outputController.stream;

  @override
  int? get pid {
    _checkOpen('pid');
    return _snapshot('pid').pid;
  }

  @override
  PtySize get size {
    _checkOpen('size');
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
    _checkOpen('ttyName');
    if (!capabilities.terminalName) {
      return null;
    }
    final bytes = _snapshot('ttyName').terminalName;
    return bytes == null ? null : utf8.decode(bytes);
  }

  @override
  void write(Uint8List data) {
    const operation = 'write';
    _checkOpen(operation);
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw _inputError(operation, inputFailure);
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
      _controller.write(_handle, data);
    } on _NativeFailure catch (failure) {
      final error = _exception(failure, operation: operation);
      if (error is PtyInputException) {
        _inputFailure ??= error;
        throw _inputError(operation, _inputFailure!);
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
      return _controller.terminate(_handle, signal.signalNumber);
    } on _NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'kill');
    }
  }

  @override
  void resize(PtySize size) {
    _checkOpen('resize');
    try {
      _controller.resize(
        _handle,
        rows: size.rows,
        columns: size.columns,
        pixelWidth: size.pixelWidth,
        pixelHeight: size.pixelHeight,
      );
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
      _controller.closeSession(_handle);
    } on Object catch (error, stackTrace) {
      if (completion.isCompleted) {
        return completion.future;
      }
      final publicError = error is _NativeFailure
          ? _exception(error, operation: 'close')
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
    if (_outputCancelled || _outputController.isClosed) {
      _acknowledge(token);
      return;
    }
    if (_paused || !_outputController.hasListener) {
      if (_pendingOutput != null) {
        _acknowledge(token);
        _failReleasedSession(
          const PtyInfrastructureException(
            'native output exceeded the one-event delivery lease',
            operation: 'output',
          ),
          StackTrace.current,
        );
        return;
      }
      _pendingOutput = (bytes: bytes, token: token);
      return;
    }
    _outputController.add(bytes);
    _acknowledge(token);
  }

  void _nativeInputFailed(_NativeFailure failure) {
    final error = _exception(failure);
    if (error is PtyInputException) {
      _inputFailure ??= error;
      return;
    }
    _nativeInfrastructureFailed(failure);
  }

  void _nativeOutputFailed(_NativeFailure failure) {
    _endOutput(_exception(failure));
  }

  void _nativeInfrastructureFailed(_NativeFailure failure) {
    final error = _exception(failure);
    if (_terminalFailure != null) {
      return;
    }
    _terminalFailure = error;
    _failReleasedSession(error);
  }

  void _nativeOutputDone() {
    _endOutput(_inputFailure);
  }

  void _nativeExit(int status) {
    if (!_exit.isCompleted) {
      _exit.complete(status);
    }
  }

  void _nativeExitFailed(_NativeFailure failure) {
    if (!_exit.isCompleted) {
      _exit.completeError(_exception(failure));
    }
  }

  void _nativeCloseComplete(int _, _NativeFailure? failure) {
    _controller.detachFinalizer(this);
    _discardPendingOutput();
    _endOutput(null, force: true);
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
      close.completeError(_exception(failure, operation: 'close'));
      return;
    }
    close.complete();
  }

  void _nativeModeChanged(int modes) {
    final mode = _mode(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  void _nativeModeFailed(_NativeFailure failure) {
    if (!_modeController.isClosed) {
      _modeController.addError(
        _exception(failure, operation: 'modeChanges.observe'),
      );
    }
  }

  void _checkOpen(String operation) {
    if (_close != null) {
      throw PtyClosedException('session closed', operation: operation);
    }
  }

  void _failReleasedSession(Object error, [StackTrace? stackTrace]) {
    _terminalFailure ??= error;
    if (!_exit.isCompleted) {
      _exit.completeError(error, stackTrace);
    }
    _endOutput(error, force: true, stackTrace: stackTrace);
    if (!_modeController.isClosed) {
      _modeController.addError(error, stackTrace);
      unawaited(_modeController.close());
    }
    final close = _close;
    if (close != null && !close.isCompleted) {
      close.completeError(error, stackTrace);
    }
    _controller.detachFinalizer(this);
    _controller.releaseSession(_handle);
  }

  _NativeSnapshot _snapshot(String operation) {
    try {
      return _controller.snapshot(_handle);
    } on _NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: operation);
    }
  }

  void _pauseOutput() {
    _paused = true;
  }

  void _resumeOutput() {
    if (_outputCancelled) {
      return;
    }
    _paused = false;
    final event = _takePendingOutput();
    if (!_paused && event != null) {
      _outputController.add(event.bytes);
      _acknowledge(event.token);
    }
    _completeOutputIfReady();
  }

  void _cancelOutput() {
    if (_outputCancelled) {
      return;
    }
    _outputCancelled = true;
    _paused = false;
    Object? failure;
    StackTrace? failureStack;
    if (_outputTermination == null) {
      try {
        _controller.cancelOutput(_handle);
      } on _NativeFailure catch (error, stackTrace) {
        failure = _operationFailure(error, operation: 'output.cancel');
        failureStack = stackTrace;
      }
    }
    _discardPendingOutput();
    _completeOutputIfReady();
    if (failure != null) {
      Error.throwWithStackTrace(failure, failureStack!);
    }
  }

  void _acknowledge(int token) {
    if (token == 0) {
      return;
    }
    try {
      _controller.acknowledge(token);
    } on _NativeFailure catch (failure) {
      _nativeInfrastructureFailed(failure);
    }
  }

  void _endOutput(Object? error, {bool force = false, StackTrace? stackTrace}) {
    _outputTermination ??= (error: error, stackTrace: stackTrace);
    if (force) {
      _discardPendingOutput();
    }
    _completeOutputIfReady();
  }

  void _completeOutputIfReady() {
    final termination = _outputTermination;
    if (termination == null ||
        _pendingOutput != null ||
        _outputController.isClosed) {
      return;
    }
    final error = termination.error;
    if (error != null && !_outputCancelled) {
      _outputController.addError(error, termination.stackTrace);
    }
    unawaited(_outputController.close());
  }

  void _discardPendingOutput() {
    final event = _takePendingOutput();
    if (event != null) {
      _acknowledge(event.token);
    }
  }

  ({Uint8List bytes, int token})? _takePendingOutput() {
    final event = _pendingOutput;
    _pendingOutput = null;
    return event;
  }

  void _observeModes(bool enabled, String operation) {
    if (!capabilities.terminalModes || _close != null) {
      return;
    }
    try {
      _controller.observeMode(_handle, enabled: enabled);
    } on _NativeFailure catch (failure, stackTrace) {
      final error = _operationFailure(failure, operation: operation);
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  static PtyTermMode _mode(int bits) => PtyTermMode(
    canonical: bits & PTYX_MODE_CANONICAL != 0,
    echo: bits & PTYX_MODE_ECHO != 0,
    signals: bits & PTYX_MODE_SIGNALS != 0,
  );

  PtyException _operationFailure(
    _NativeFailure failure, {
    required String operation,
  }) {
    final error = _exception(failure, operation: operation);
    if (error is PtyInfrastructureException) {
      _nativeInfrastructureFailed(failure);
    }
    return error;
  }
}

PtyCapabilities _capabilitiesFromBits(int bits) => PtyCapabilities(
  signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
  processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
  terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
  terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
);

PtyException _exception(_NativeFailure failure, {String? operation}) {
  final publicOperation = operation ?? _operationName(failure.operation);
  final nativeCode = failure.nativeCode == 0 ? null : failure.nativeCode;
  if (publicOperation == 'mode' || publicOperation.startsWith('modeChanges.')) {
    return PtyModeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_BACKPRESSURE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_QUEUE_FULL) {
    return PtyBackpressureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_INVALID_ARGUMENT ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_INVALID_ARGUMENT) {
    return PtyInvalidArgumentException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_UNSUPPORTED ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_UNSUPPORTED) {
    return PtyUnsupportedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST) {
    return PtyInfrastructureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_INPUT ||
      failure.operation == ptyx_operation.PTYX_OPERATION_WRITE) {
    return _inputException(failure, operation: publicOperation);
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT ||
      failure.operation == ptyx_operation.PTYX_OPERATION_OUTPUT) {
    return PtyOutputException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_CLOSED ||
      failure.status == ptyx_status.PTYX_STATUS_STALE_HANDLE ||
      failure.status == ptyx_status.PTYX_STATUS_WRONG_STATE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_CLOSED ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_STALE_HANDLE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_WRONG_STATE) {
    return PtyClosedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  return switch (failure.operation) {
    ptyx_operation.PTYX_OPERATION_SPAWN => PtySpawnException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_TERMINATE => PtySignalException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_EXIT => PtyExitException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_RESIZE => PtyResizeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_METADATA => PtyMetadataException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_CLOSE => PtyCloseException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    _ => PtyException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
  };
}

PtyInputException _inputException(
  _NativeFailure failure, {
  required String operation,
}) => PtyInputException(
  failure.message,
  operation: operation,
  nativeCode: failure.nativeCode == 0 ? null : failure.nativeCode,
);

PtyInputException _inputError(String operation, PtyInputException failure) =>
    PtyInputException(
      failure.message,
      operation: operation,
      nativeCode: failure.nativeCode,
      context: failure.context,
    );

String _operationName(int operation) => switch (operation) {
  ptyx_operation.PTYX_OPERATION_SPAWN => 'spawn',
  ptyx_operation.PTYX_OPERATION_WRITE => 'write',
  ptyx_operation.PTYX_OPERATION_OUTPUT => 'output',
  ptyx_operation.PTYX_OPERATION_RESIZE => 'resize',
  ptyx_operation.PTYX_OPERATION_TERMINATE => 'kill',
  ptyx_operation.PTYX_OPERATION_EXIT => 'exit',
  ptyx_operation.PTYX_OPERATION_METADATA => 'metadata',
  ptyx_operation.PTYX_OPERATION_CLOSE => 'close',
  _ => 'controller',
};

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
