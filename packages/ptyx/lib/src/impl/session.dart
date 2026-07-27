import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Directory, Platform, ProcessSignal;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/controller.dart';

final _sessionFinalizer = Finalizer<int>(
  (handle) => controllerFinalize(Pointer<Void>.fromAddress(handle)),
);
final _sessionRegistryFinalizer = Finalizer<_SessionRegistryToken>(
  (token) => token.runtime._removeSession(token.handle),
);
const _terminalDrainTimeout = Duration(seconds: 15);

int? _lastNativeCode() {
  final code = controllerLastErrorCode();
  return code == 0 ? null : code;
}

final class _SessionRegistryToken {
  const _SessionRegistryToken(this.runtime, this.handle);

  final _ControllerRuntime runtime;
  final int handle;
}

@internal
final class NativeSession implements PtySession {
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
    _inputDone.future.ignore();
  }

  static Future<NativeSession> spawn(PtySpawnOptions options) async {
    final workingDirectory = options.workingDirectory ?? Directory.current.path;
    _validateSpawnOptions(options, workingDirectory);
    final runtime = _ControllerRuntime.instance;
    final outputPort = runtime.outputPort;
    final eventPort = runtime.eventPort;
    final spawnResult = await Isolate.run(
      () => _spawnNative(options, workingDirectory, outputPort, eventPort),
    );
    final handle = spawnResult.handle;
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
      options.maxBufferedInput,
      options.gracefulCloseTimeout,
      output,
      modes,
    );
    runtime._addSession(session);
    _sessionFinalizer.attach(session, handle, detach: session);
    _sessionRegistryFinalizer.attach(
      session,
      _SessionRegistryToken(runtime, handle),
      detach: session,
    );
    if (!controllerActivate(handle)) {
      runtime._removeSession(handle);
      controllerClose(handle);
      throw const PtyInfrastructureException(
        'native session activation failed',
      );
    }
    return session;
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
  final _inputDone = Completer<void>();
  final int _inputCapacity;
  final Duration _gracefulCloseTimeout;
  Completer<void>? _close;
  var _lastSequence = 0;
  var _pendingCredit = 0;
  Uint8List? _pendingOutput;
  var _paused = true;
  var _outputCancelled = false;
  var _infrastructureLost = false;
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
  Future<void> get inputDone => _inputDone.future;

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
  bool tryWrite(Uint8List data) => _tryWrite(data, 'tryWrite');

  bool _tryWrite(Uint8List data, String operation) {
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
    final sequence = using((arena) {
      final pointer = arena<Uint8>(data.length);
      pointer.asTypedList(data.length).setAll(0, data);
      return controllerWrite(_handle, pointer, data.length);
    });
    if (sequence < 0) {
      final nativeCode = controllerLastErrorCode();
      final failure = PtyInputException(
        'session can no longer accept input',
        operation: operation,
        nativeCode: nativeCode == 0 ? null : nativeCode,
      );
      _inputFailure ??= failure;
      throw _inputError(operation, _inputFailure!);
    }
    if (sequence == 0) {
      return false;
    }
    _lastSequence = sequence;
    return true;
  }

  @override
  Future<void> waitForInputCapacity(int byteCount) =>
      _waitForInputCapacity(byteCount, 'waitForInputCapacity');

  Future<void> _waitForInputCapacity(int byteCount, String operation) {
    _checkOpen(operation);
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw _inputError(operation, inputFailure);
    }
    if (byteCount <= 0 || byteCount > _inputCapacity) {
      throw PtyInvalidArgumentException(
        'requested input capacity is outside this session limit',
        operation: operation,
        context: '$byteCount bytes requested; limit is $_inputCapacity bytes',
      );
    }
    return _runtime._wait(
      _handle,
      operation,
      (waiter) => controllerWaitCapacity(_handle, byteCount, waiter),
    );
  }

  @override
  Future<void> write(Uint8List data) async {
    while (!_tryWrite(data, 'write')) {
      await _waitForInputCapacity(data.length, 'write');
    }
  }

  @override
  Future<void> flush() {
    _checkOpen('flush');
    final inputFailure = _inputFailure;
    if (inputFailure != null) {
      throw _inputError('flush', inputFailure);
    }
    return _runtime._wait(
      _handle,
      'flush',
      (waiter) => controllerWaitFlush(_handle, _lastSequence, waiter),
    );
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
      final pending = _pendingOutput;
      _pendingOutput = null;
      if (pending != null) {
        _deliverOutput(pending);
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
    final pending = _pendingOutput;
    _pendingOutput = null;
    if (pending != null) {
      _queueCredit(pending.length);
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
      _pendingOutput = bytes;
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
      if (!_inputDone.isCompleted) {
        _inputDone.complete();
      }
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
    if (!_inputDone.isCompleted) {
      _inputDone.completeError(_inputFailure!);
    }
  }

  PtyException _waitFailure(String operation) {
    if (_close != null) {
      return PtyClosedException('session closed', operation: operation);
    }
    final failure = _inputFailure;
    return failure == null
        ? PtyInputException('terminal input failure', operation: operation)
        : _inputError(operation, failure);
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
    _runtime._failWaiters(_handle, error);
    if (!_exit.isCompleted) {
      _exit.completeError(error);
    }
    if (!_inputDone.isCompleted) {
      _inputDone.completeError(error);
    }
    if (!_outputDone.isCompleted) {
      _outputDone.complete();
    }
    if (!_outputController.isClosed) {
      _outputController.addError(error);
      unawaited(_outputController.close());
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
    if (!_infrastructureLost) {
      final signal = controllerSignal(
        _handle,
        ProcessSignal.sigterm.signalNumber,
      );
      if (signal < 0) {
        recordFailure(
          const PtyCloseException('graceful termination request failed'),
        );
      }
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
    } on Object catch (error, stackTrace) {
      recordFailure(error, stackTrace);
      controllerClose(_handle);
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
      if (!_inputDone.isCompleted) {
        _inputDone.complete();
      }
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
    if (_modeTimer != null || _close != null) {
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

final class _PendingWaiter {
  const _PendingWaiter(this.handle, this.operation, this.completion);

  final int handle;
  final String operation;
  final Completer<void> completion;
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
    _output = RawReceivePort(_onOutput)..keepIsolateAlive = false;
    _events = RawReceivePort(_onEvent)..keepIsolateAlive = false;
  }

  static final instance = _ControllerRuntime._();

  late final RawReceivePort _output;
  late final RawReceivePort _events;
  final Map<int, WeakReference<NativeSession>> _sessions = {};
  final Map<int, _PendingWaiter> _waiters = {};
  final Map<int, int> _credit = {};
  var _nextWaiter = 1;

  int get outputPort => _output.sendPort.nativePort;
  int get eventPort => _events.sendPort.nativePort;

  void _addSession(NativeSession session) {
    _sessions[session._handle] = WeakReference(session);
    _output.keepIsolateAlive = true;
    _events.keepIsolateAlive = true;
  }

  void _removeSession(int handle) {
    _sessions.remove(handle);
    if (_sessions.isEmpty) {
      _output.keepIsolateAlive = false;
      _events.keepIsolateAlive = false;
    }
  }

  Future<void> _wait(
    int handle,
    String operation,
    int Function(int waiter) register,
  ) async {
    final waiter = _nextWaiter++;
    final completion = Completer<void>();
    _waiters[waiter] = _PendingWaiter(handle, operation, completion);
    switch (register(waiter)) {
      case 1:
        _waiters.remove(waiter);
      case 2:
        await completion.future;
      default:
        _waiters.remove(waiter);
        throw _session(handle)?._waitFailure(operation) ??
            PtyInputException('terminal input failure', operation: operation);
    }
  }

  void _onOutput(Object? message) {
    if (message case [final int handle, final Uint8List bytes]) {
      _session(handle)?._onOutput(bytes);
    }
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
      case 1 || 2:
        _waiters.remove(payload)?.completion.complete();
      case 3:
        _session(payload)?._completeExit();
      case 4:
        _session(payload)?._completeOutput();
      case 5:
        final waiter = _waiters.remove(payload);
        if (waiter != null) {
          waiter.completion.completeError(
            _session(waiter.handle)?._waitFailure(waiter.operation) ??
                PtyInputException(
                  'terminal input failure',
                  operation: waiter.operation,
                ),
          );
        }
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

  void _failWaiters(int handle, Object error) {
    final tokens = _waiters.entries
        .where((entry) => entry.value.handle == handle)
        .map((entry) => entry.key)
        .toList(growable: false);
    for (final token in tokens) {
      _waiters.remove(token)?.completion.completeError(error);
    }
  }
}

({int handle, int? nativeCode}) _spawnNative(
  PtySpawnOptions options,
  String workingDirectory,
  int outputPort,
  int eventPort,
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
      outputPort,
      eventPort,
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
