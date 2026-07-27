import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io' show Platform, ProcessSignal;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/controller.dart';

@internal
final class NativeSession implements PtySession {
  NativeSession._(
    this._runtime,
    this._handle,
    this._size,
    this._inputCapacity,
    this._gracefulCloseTimeout,
    this._outputController,
    this._modeController,
  );

  static Future<NativeSession> spawn(PtySpawnOptions options) async {
    _validateSpawnOptions(options);
    final runtime = _ControllerRuntime.instance;
    final outputPort = runtime.outputPort;
    final eventPort = runtime.eventPort;
    final handle = await Isolate.run(
      () => _spawnNative(options, outputPort, eventPort),
    );
    if (handle == 0) {
      throw const PtyException('native process spawn failed');
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
      options.initialSize,
      options.maxBufferedInput,
      options.gracefulCloseTimeout,
      output,
      modes,
    );
    runtime._sessions[handle] = session;
    if (!controllerActivate(handle)) {
      runtime._sessions.remove(handle);
      controllerClose(handle);
      throw const PtyInfrastructureException(
        'native session activation failed',
      );
    }
    return session;
  }

  final _ControllerRuntime _runtime;
  final int _handle;
  final StreamController<Uint8List> _outputController;
  final StreamController<PtyTermMode> _modeController;
  final _exit = Completer<int>();
  final _outputDone = Completer<void>();
  final _inputDone = Completer<void>();
  PtySize _size;
  final int _inputCapacity;
  final Duration _gracefulCloseTimeout;
  Completer<void>? _close;
  var _lastSequence = 0;
  var _pendingCredit = 0;
  Uint8List? _pendingOutput;
  var _paused = true;
  var _outputCancelled = false;
  var _portLost = false;
  var _infrastructureLost = false;
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
    );
  }

  @override
  Future<int> get exitCode => _exit.future;

  @override
  Future<void> get inputDone => _inputDone.future;

  @override
  PtyTermMode? get mode {
    _checkOpen();
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
    _checkOpen();
    final value = controllerPid(_handle);
    return value < 0 ? null : value;
  }

  @override
  PtySize get size {
    _checkOpen();
    return using((arena) {
      final values = arena<Uint32>(4);
      if (!controllerSize(_handle, values)) {
        return _size;
      }
      return _size = PtySize(
        rows: values[0],
        columns: values[1],
        pixelWidth: values[2],
        pixelHeight: values[3],
      );
    });
  }

  @override
  String? get ttyName {
    _checkOpen();
    final length = controllerTtyName(_handle, nullptr, 0);
    if (length < 0) {
      return null;
    }
    return using((arena) {
      final bytes = arena<Uint8>(length);
      final written = controllerTtyName(_handle, bytes, length);
      if (written != length) {
        throw const PtyException('native terminal name changed during read');
      }
      return utf8.decode(bytes.asTypedList(length));
    });
  }

  bool get _isTerminal =>
      _portLost || _infrastructureLost || (_close?.isCompleted ?? false);

  void _checkOpen() {
    if (_close != null) {
      throw const PtyClosedException('session closed');
    }
  }

  @override
  bool tryWrite(Uint8List data) {
    _checkOpen();
    if (data.isEmpty) {
      throw ArgumentError.value(data, 'data', 'must not be empty');
    }
    if (data.length > _inputCapacity) {
      return false;
    }
    final sequence = using((arena) {
      final pointer = arena<Uint8>(data.length);
      pointer.asTypedList(data.length).setAll(0, data);
      return controllerWrite(_handle, pointer, data.length);
    });
    if (sequence == 0) {
      return false;
    }
    _lastSequence = sequence;
    return true;
  }

  @override
  Future<void> waitForInputCapacity(int byteCount) {
    _checkOpen();
    if (byteCount <= 0) {
      throw RangeError.range(byteCount, 1, null, 'byteCount');
    }
    return _runtime._wait(
      _handle,
      (waiter) => controllerWaitCapacity(_handle, byteCount, waiter),
    );
  }

  @override
  Future<void> write(Uint8List data) async {
    while (!tryWrite(data)) {
      await waitForInputCapacity(data.length);
    }
  }

  @override
  Future<void> flush() {
    _checkOpen();
    return _runtime._wait(
      _handle,
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
      throw const PtyException('native signal delivery failed');
    }
    return result == 1;
  }

  @override
  void resize(PtySize size) {
    _checkOpen();
    _validateSize(size);
    if (!controllerResize(
      _handle,
      size.rows,
      size.columns,
      size.pixelWidth,
      size.pixelHeight,
    )) {
      throw const PtyException('native terminal resize failed');
    }
    _size = size;
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
      final pointer = arena<Int32>();
      return controllerExitStatus(_handle, pointer) ? pointer.value : null;
    });
    if (status != null) {
      _exit.complete(status);
      if (!_inputDone.isCompleted) {
        _inputDone.complete();
      }
    }
  }

  void _completeInputFailure() {
    if (!_inputDone.isCompleted) {
      _inputDone.completeError(
        const PtyInputException('accepted input was not fully written'),
      );
    }
  }

  void _completeOutput() {
    if (!_outputDone.isCompleted) {
      _outputDone.complete();
    }
    if (!_outputController.isClosed) {
      unawaited(_outputController.close());
    }
  }

  void _fail(Object error) {
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
    _runtime._sessions.remove(_handle);
  }

  Future<void> _closeNative(Completer<void> completion) async {
    _cancelOutput();
    controllerSignal(_handle, ProcessSignal.sigterm.signalNumber);
    try {
      await _exit.future.timeout(_gracefulCloseTimeout);
    } on TimeoutException {
      controllerClose(_handle);
    } on Object {
      controllerClose(_handle);
    }
    _completeExit();
    try {
      await Future.wait<Object?>([_exit.future, _outputDone.future]);
      if (!_inputDone.isCompleted) {
        _inputDone.complete();
      }
      controllerDestroy(_handle);
      _runtime._sessions.remove(_handle);
      _stopModeObservation();
      await _modeController.close();
      completion.complete();
    } on Object catch (error, stackTrace) {
      completion.completeError(error, stackTrace);
    }
  }

  PtyTermMode? _readMode() {
    final flags = controllerMode(_handle);
    if (flags < 0) {
      return null;
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
    final current = _readMode();
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
  const _PendingWaiter(this.handle, this.completion);

  final int handle;
  final Completer<void> completion;
}

final class _ControllerRuntime {
  _ControllerRuntime._() : _output = ReceivePort(), _events = ReceivePort() {
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
    _output.listen(_onOutput);
    _events.listen(_onEvent);
  }

  static final instance = _ControllerRuntime._();

  final ReceivePort _output;
  final ReceivePort _events;
  final Map<int, NativeSession> _sessions = {};
  final Map<int, _PendingWaiter> _waiters = {};
  final Map<int, int> _credit = {};
  var _nextWaiter = 1;

  int get outputPort => _output.sendPort.nativePort;
  int get eventPort => _events.sendPort.nativePort;

  Future<void> _wait(int handle, int Function(int waiter) register) async {
    final waiter = _nextWaiter++;
    final completion = Completer<void>();
    _waiters[waiter] = _PendingWaiter(handle, completion);
    switch (register(waiter)) {
      case 1:
        _waiters.remove(waiter);
      case 2:
        await completion.future;
      default:
        _waiters.remove(waiter);
        throw const PtyInputException('terminal input failure');
    }
  }

  void _onOutput(Object? message) {
    if (message case [final int handle, final Uint8List bytes]) {
      _sessions[handle]?._onOutput(bytes);
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
        _sessions[payload]?._completeInputFailure();
      case 1 || 2:
        _waiters.remove(payload)?.completion.complete();
      case 3:
        _sessions[payload]?._completeExit();
      case 4:
        _sessions[payload]?._completeOutput();
      case 5:
        _waiters
            .remove(payload)
            ?.completion
            .completeError(const PtyInputException('terminal input failure'));
      case 6:
        final session = _sessions[payload];
        if (session != null) {
          session._portLost = true;
          session._fail(
            const PtyInfrastructureException('Dart native port closed'),
          );
        }
      case 7:
        final session = _sessions[payload];
        if (session != null) {
          session._infrastructureLost = true;
          session._fail(
            const PtyInfrastructureException('Unix PTY broker terminated'),
          );
        }
    }
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

int _spawnNative(PtySpawnOptions options, int outputPort, int eventPort) {
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
    final environment = switch (options.environmentMode) {
      PtyEnvironmentMode.inherit => const <String, String>{},
      PtyEnvironmentMode.overlay => {
        ...Platform.environment,
        ...options.environment,
      },
      PtyEnvironmentMode.replace => options.environment,
      PtyEnvironmentMode.clear => const <String, String>{},
    };
    for (final key in environment.keys) {
      if (key.isEmpty || key.contains('=') || key.contains('\u0000')) {
        throw ArgumentError.value(key, 'environment key', 'is invalid');
      }
    }
    final environmentValues = environment.entries
        .map((entry) => '${entry.key}=${entry.value}')
        .toList(growable: false);
    final nativeEnvironment = nativeStrings(environmentValues);
    final cwd = options.workingDirectory == null
        ? nullptr
        : nativeString(options.workingDirectory!);
    return controllerSpawn(
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
  });
}

void _validateSpawnOptions(PtySpawnOptions options) {
  _validateSize(options.initialSize);
  if (options.maxBufferedInput <= 0 ||
      options.maxBufferedInput > 64 * 1024 * 1024) {
    throw RangeError.range(
      options.maxBufferedInput,
      1,
      64 * 1024 * 1024,
      'maxBufferedInput',
    );
  }
  if (options.maxBufferedOutput <= 0 ||
      options.maxBufferedOutput > 64 * 1024 * 1024) {
    throw RangeError.range(
      options.maxBufferedOutput,
      1,
      64 * 1024 * 1024,
      'maxBufferedOutput',
    );
  }
  if (options.gracefulCloseTimeout.isNegative ||
      options.gracefulCloseTimeout > const Duration(minutes: 1)) {
    throw RangeError(
      'gracefulCloseTimeout must be between zero and one minute',
    );
  }
  if (options.executable.isEmpty || options.executable.contains('\u0000')) {
    throw const PtyException('invalid executable in process spawn options');
  }
  for (final argument in options.arguments) {
    if (argument.contains('\u0000')) {
      throw const PtyException('invalid argument in process spawn options');
    }
  }
  if (options.workingDirectory?.contains('\u0000') ?? false) {
    throw const PtyException(
      'invalid working directory in process spawn options',
    );
  }
  for (final entry in options.environment.entries) {
    if (entry.key.isEmpty ||
        entry.key.contains('=') ||
        entry.key.contains('\u0000') ||
        entry.value.contains('\u0000')) {
      throw const PtyException(
        'invalid environment entry in process spawn options',
      );
    }
  }
}

void _validateSize(PtySize size) {
  if (size.rows <= 0 ||
      size.rows > 65535 ||
      size.columns <= 0 ||
      size.columns > 65535 ||
      size.pixelWidth < 0 ||
      size.pixelWidth > 65535 ||
      size.pixelHeight < 0 ||
      size.pixelHeight > 65535) {
    throw RangeError('terminal dimensions are outside native bounds');
  }
}
