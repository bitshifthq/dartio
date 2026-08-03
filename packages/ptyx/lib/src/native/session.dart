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

@internal
final class NativeSession implements Finalizable, PtySession {
  static final _finalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_session_finalize),
  );
  final _NativeRuntime _controller;
  final int _handle;
  final int _inputCapacity;
  late final StreamController<Uint8List> _output;
  late final StreamController<PtyTermMode> _modeController;
  final _exit = Completer<int>();
  // Native close completes through a later event; one completer shares that
  // result with every concurrent or repeated close call.
  Completer<void>? _close;
  PtyInputException? _inputFailure;
  PtyException? _terminalFailure;
  PtyTermMode? _lastMode;
  ({Uint8List bytes, int token})? _pendingOutput;
  ({Object? error, StackTrace? stackTrace})? _outputTermination;
  var _outputPaused = true;
  var _outputCancelled = false;

  NativeSession._(_NativeRuntime controller, this._handle, this._inputCapacity)
    : _controller = controller {
    _output = StreamController<Uint8List>(
      sync: true,
      onListen: _resumeOutput,
      onPause: () => _outputPaused = true,
      onResume: _resumeOutput,
      onCancel: _cancelOutput,
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
  int? get pid => sessionPid(_handle);

  @override
  PtySize get size => sessionSize(_handle);

  @override
  String? get ttyName {
    if (!capabilities.terminalName) return null;
    return sessionTtyName(_handle);
  }

  @override
  Future<void> close() {
    final existing = _close;
    if (existing != null) return existing.future;

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
      if (!completion.isCompleted) _failSession(error, stackTrace);
    }
    return completion.future;
  }

  @override
  bool kill([ProcessSignal signal = .sigterm]) {
    if (_close != null || _exit.isCompleted) return false;
    return sessionTerminate(_handle, signal.signalNumber);
  }

  @override
  void resize(PtySize size) => sessionResize(_handle, size);

  @override
  void write(Uint8List data) {
    final inputFailure = _inputFailure;
    if (inputFailure != null) throw inputFailure;
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
      _failSession(error);
      rethrow;
    }
  }

  void _acknowledgeOutput(int token) {
    if (token == 0) return;
    try {
      _controller.acknowledge(token);
    } on PtyException catch (error) {
      _failSession(error);
    }
  }

  void _cancelOutput() {
    if (_outputCancelled) return;
    _outputCancelled = true;
    _outputPaused = false;
    if (_outputTermination == null) sessionCancelOutput(_handle);
    _discardOutput();
    _completeOutput();
  }

  void _completeClose(PtyException? error) {
    _finalizer.detach(this);
    _finishOutput(null, force: true);
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

    final failure = _terminalFailure ?? error;
    if (failure != null) {
      close.completeError(failure);
    } else {
      close.complete();
    }
  }

  void _completeOutput() {
    final termination = _outputTermination;
    if (termination == null || _pendingOutput != null || _output.isClosed) {
      return;
    }
    if (termination.error != null && !_outputCancelled) {
      _output.addError(termination.error!, termination.stackTrace);
    }
    unawaited(_output.close());
  }

  void _deliverOutput(Uint8List bytes, int token) {
    if (_outputCancelled || _output.isClosed) {
      _acknowledgeOutput(token);
      return;
    }
    if (_outputPaused || !_output.hasListener) {
      if (_pendingOutput != null) {
        _acknowledgeOutput(token);
        _failSession(
          syntheticError(
            operation: 'controller',
            message: 'native output exceeded the one-event delivery lease',
          ),
        );
        return;
      }
      _pendingOutput = (bytes: bytes, token: token);
      return;
    }
    _output.add(bytes);
    _acknowledgeOutput(token);
  }

  void _discardOutput() {
    final pending = _pendingOutput;
    _pendingOutput = null;
    if (pending != null) _acknowledgeOutput(pending.token);
  }

  void _failSession(PtyException error, [StackTrace? stackTrace]) {
    if (_terminalFailure != null) return;
    _terminalFailure = error;
    if (!_exit.isCompleted) _exit.completeError(error, stackTrace);
    _finishOutput(error, force: true, stackTrace: stackTrace);
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

  void _finishOutput(
    Object? error, {
    bool force = false,
    StackTrace? stackTrace,
  }) {
    _outputTermination ??= (error: error, stackTrace: stackTrace);
    if (force) _discardOutput();
    _completeOutput();
  }

  void _observeModes(bool enabled) {
    if (!capabilities.terminalModes || _close != null) return;

    try {
      sessionObserveMode(_handle, enabled: enabled);
    } on PtyException catch (error, stackTrace) {
      if (error is PtyInfraException) {
        _failSession(error, stackTrace);
      } else if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  void _onNativeEvent(NativeEvent event) {
    switch (event.kind) {
      case .output:
        if (event.data case final bytes?) {
          _deliverOutput(bytes, event.token);
        }
      case .inputFailed:
        final error = _eventError(event);
        if (error is PtyInputException) {
          _inputFailure ??= error;
        } else {
          _failSession(error);
        }
      case .outputFailed:
        _finishOutput(_eventError(event));
      case .infrastructureFailed:
        _failSession(_eventError(event));
      case .outputDone:
        _finishOutput(_inputFailure);
      case .exit:
        if (!_exit.isCompleted) _exit.complete(event.value);
      case .exitFailed:
        if (!_exit.isCompleted) _exit.completeError(_eventError(event));
      case .closeComplete:
        _completeClose(event.error);
      case .modeChanged:
        _updateMode(event.value);
      case .modeFailed:
        if (!_modeController.isClosed) {
          _modeController.addError(_eventError(event));
        }
      case .spawnReady || .spawnFailed:
        break;
    }
  }

  void _resumeOutput() {
    if (_outputCancelled) return;
    _outputPaused = false;
    final pending = _pendingOutput;
    _pendingOutput = null;
    if (pending != null) {
      _output.add(pending.bytes);
      _acknowledgeOutput(pending.token);
    }
    _completeOutput();
  }

  void _updateMode(int modes) {
    final mode = modeFromBits(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  static Future<NativeSession> spawn(PtySpawnOptions options) async {
    final request = _snapshotSpawnRequest(options);
    final controller = await _NativeRuntime.instance;

    final completion = Completer<NativeSession>();
    controller.startSpawn(
      request,
      onReady: (handle) {
        final session = NativeSession._(
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
