import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:io' show Directory, Platform, ProcessSignal;
import 'dart:typed_data';

import 'package:meta/meta.dart';

import '../api/api.dart';
import '../bindings/controller.dart';
import '../ffi/ptyx.g.dart'
    show
        PTYX_CAPABILITY_CONPTY,
        PTYX_CAPABILITY_PROCESS_GROUPS,
        PTYX_CAPABILITY_SIGNALS,
        PTYX_CAPABILITY_TERMINAL_MODES,
        PTYX_CAPABILITY_TERMINAL_NAME,
        PTYX_ERROR_CLOSED,
        PTYX_ERROR_DOMAIN_INPUT,
        PTYX_ERROR_DOMAIN_OUTPUT,
        PTYX_ERROR_DOMAIN_RUNTIME,
        PTYX_ERROR_INFRASTRUCTURE_LOST,
        PTYX_ERROR_INVALID_ARGUMENT,
        PTYX_ERROR_QUEUE_FULL,
        PTYX_ERROR_STALE_HANDLE,
        PTYX_ERROR_UNSUPPORTED,
        PTYX_ERROR_WRONG_STATE,
        PTYX_EVENT_CLOSE_INPUT_FAILED,
        PTYX_OPERATION_CLOSE,
        PTYX_OPERATION_METADATA,
        PTYX_OPERATION_OUTPUT,
        PTYX_OPERATION_RESIZE,
        PTYX_OPERATION_SPAWN,
        PTYX_OPERATION_TERMINATE,
        PTYX_OPERATION_WRITE,
        PTYX_STATUS_BACKPRESSURE,
        PTYX_STATUS_CLOSED,
        PTYX_STATUS_INVALID_ARGUMENT,
        PTYX_STATUS_STALE_HANDLE,
        PTYX_STATUS_UNSUPPORTED,
        PTYX_STATUS_WRONG_STATE;

@internal
final class NativeSession implements PtySession, NativeEventTarget {
  NativeSession._(
    this._controller,
    this._handle,
    this._inputCapacity,
    this._outputController,
    this._modeController,
  ) {
    _exit.future.ignore();
    _paused = true;
    _outputCancelled = false;
    _outputEnded = false;
    _controller.attachFinalizer(this, _handle);
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
    final effectiveEnvironment = _effectiveEnvironment(snapshot);
    final environment = List<String>.unmodifiable([
      for (final entry in effectiveEnvironment.entries)
        '${entry.key}=${entry.value}',
    ]);
    _validateSpawnOptions(
      snapshot,
      workingDirectory,
      effectiveEnvironment,
      environment,
    );
    final controller = NativeController.instance;
    await controller.ensureOwnerGuardian();

    final completion = Completer<NativeSession>();
    try {
      controller.startSpawn(
        NativeSpawnRequest(
          executable: snapshot.executable,
          arguments: snapshot.arguments,
          environment: environment,
          inheritEnvironment:
              snapshot.environmentMode == PtyEnvironmentMode.inherit,
          workingDirectory: workingDirectory,
          rows: snapshot.initialSize.rows,
          columns: snapshot.initialSize.columns,
          pixelWidth: snapshot.initialSize.pixelWidth,
          pixelHeight: snapshot.initialSize.pixelHeight,
          inputCapacity: snapshot.maxBufferedInput,
          outputCapacity: snapshot.maxBufferedOutput,
          gracefulCloseTimeout: snapshot.gracefulCloseTimeout,
        ),
        onReady: (handle) {
          late final NativeSession session;
          // The returned session owns and closes this controller.
          // ignore: close_sinks
          final output = StreamController<Uint8List>(
            sync: true,
            onListen: () => session._resumeOutput(),
            onPause: () => session._pauseOutput(),
            onResume: () => session._resumeOutput(),
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
            controller,
            handle,
            snapshot.maxBufferedInput,
            output,
            modes,
          );
          completion.complete(session);
          return session;
        },
        onFailure: (failure) {
          completion.completeError(_exception(failure, operation: 'spawn'));
        },
      );
    } on NativeFailure catch (failure) {
      throw _exception(failure, operation: 'spawn');
    }
    return completion.future;
  }

  final NativeController _controller;
  final int _handle;
  final int _inputCapacity;
  final StreamController<Uint8List> _outputController;
  final StreamController<PtyTermMode> _modeController;
  final Queue<({Uint8List bytes, int token})> _pendingOutput = Queue();
  final _exit = Completer<int>();
  Completer<void>? _close;
  PtyInputException? _inputFailure;
  Object? _terminalFailure;
  Object? _outputTerminalError;
  StackTrace? _outputTerminalStack;
  late bool _paused;
  late bool _outputCancelled;
  late bool _outputEnded;
  PtyTermMode? _lastMode;

  @override
  PtyCapabilities get capabilities {
    final bits = _controller.capabilityBits;
    return PtyCapabilities(
      signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
      processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
      terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
      conPty: bits & PTYX_CAPABILITY_CONPTY != 0,
      terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
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
    try {
      _controller.write(_handle, data);
    } on NativeFailure catch (failure) {
      final error = _exception(failure, operation: operation);
      if (error is PtyInputException) {
        _inputFailure ??= error;
        throw _inputError(operation, _inputFailure!);
      }
      if (error is PtyInfrastructureException) {
        nativeInfrastructureFailed(failure);
      }
      throw error;
    }
  }

  @override
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]) {
    if (_close != null || _exit.isCompleted) {
      return false;
    }
    try {
      return _controller.terminate(_handle, signal.signalNumber);
    } on NativeFailure catch (failure) {
      throw _operationFailure(failure, operation: 'kill');
    }
  }

  @override
  void resize(PtySize size) {
    _checkOpen('resize');
    _validateSize(size, operation: 'resize');
    try {
      _controller.resize(
        _handle,
        rows: size.rows,
        columns: size.columns,
        pixelWidth: size.pixelWidth,
        pixelHeight: size.pixelHeight,
      );
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
      _controller.closeSession(_handle);
    } on Object catch (error, stackTrace) {
      if (completion.isCompleted) {
        return completion.future;
      }
      final publicError = error is NativeFailure
          ? _exception(error, operation: 'close')
          : error;
      if (error is NativeFailure && publicError is PtyInfrastructureException) {
        nativeInfrastructureFailed(error);
        return completion.future;
      }
      _failReleasedSession(publicError, stackTrace);
    }
    return completion.future;
  }

  @override
  void nativeOutput(Uint8List bytes, int token) {
    if (_outputCancelled || _outputController.isClosed) {
      _acknowledge(token);
      return;
    }
    if (_paused || !_outputController.hasListener) {
      _pendingOutput.addLast((bytes: bytes, token: token));
      return;
    }
    _outputController.add(bytes);
    _acknowledge(token);
  }

  @override
  void nativeInputFailed(NativeFailure failure) {
    _inputFailure ??= _exception(failure) as PtyInputException;
  }

  @override
  void nativeOutputFailed(NativeFailure failure) {
    _endOutput(_exception(failure));
  }

  @override
  void nativeInfrastructureFailed(NativeFailure failure) {
    final error = _exception(failure);
    if (_terminalFailure != null) {
      return;
    }
    _terminalFailure = error;
    if (!_exit.isCompleted) {
      _exit.completeError(error);
    }
    _endOutput(error, force: true);
    if (!_modeController.isClosed) {
      _modeController.addError(error);
      unawaited(_modeController.close());
    }
    final close = _close;
    if (close != null && !close.isCompleted) {
      close.completeError(error);
    }
    _controller.detachFinalizer(this);
    _controller.releaseSession(_handle);
  }

  @override
  void nativeOutputDone() {
    _endOutput(_inputFailure);
  }

  @override
  void nativeExit(int status) {
    if (!_exit.isCompleted) {
      _exit.complete(status);
    }
  }

  @override
  void nativeCloseComplete(int flags, NativeFailure? failure) {
    _controller.detachFinalizer(this);
    while (_pendingOutput.isNotEmpty) {
      _acknowledge(_pendingOutput.removeFirst().token);
    }
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
      final error = flags & PTYX_EVENT_CLOSE_INPUT_FAILED != 0
          ? _inputException(failure, operation: 'close')
          : _exception(failure, operation: 'close');
      close.completeError(error);
      return;
    }
    close.complete();
  }

  @override
  void nativeModeChanged(int modes) {
    final mode = _mode(modes);
    if (mode != _lastMode && !_modeController.isClosed) {
      _lastMode = mode;
      _modeController.add(mode);
    }
  }

  void _checkOpen(String operation) {
    if (_close != null) {
      throw PtyClosedException('session closed', operation: operation);
    }
  }

  void _failReleasedSession(Object error, StackTrace stackTrace) {
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

  NativeSnapshot _snapshot(String operation) {
    try {
      return _controller.snapshot(_handle);
    } on NativeFailure catch (failure) {
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
    while (!_paused && _pendingOutput.isNotEmpty) {
      final event = _pendingOutput.removeFirst();
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
    if (!_outputEnded) {
      try {
        _controller.cancelOutput(_handle);
      } on NativeFailure catch (error, stackTrace) {
        failure = _operationFailure(error, operation: 'output.cancel');
        failureStack = stackTrace;
      }
    }
    while (_pendingOutput.isNotEmpty) {
      _acknowledge(_pendingOutput.removeFirst().token);
    }
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
    } on NativeFailure catch (failure) {
      nativeInfrastructureFailed(failure);
    }
  }

  void _endOutput(Object? error, {bool force = false, StackTrace? stackTrace}) {
    if (!_outputEnded) {
      _outputEnded = true;
      _outputTerminalError = error;
      _outputTerminalStack = stackTrace;
    }
    if (force) {
      while (_pendingOutput.isNotEmpty) {
        _acknowledge(_pendingOutput.removeFirst().token);
      }
    }
    _completeOutputIfReady();
  }

  void _completeOutputIfReady() {
    if (!_outputEnded ||
        _pendingOutput.isNotEmpty ||
        _outputController.isClosed) {
      return;
    }
    final error = _outputTerminalError;
    if (error != null && !_outputCancelled) {
      _outputController.addError(error, _outputTerminalStack);
    }
    unawaited(_outputController.close());
  }

  void _startModeObservation() {
    if (!capabilities.terminalModes || _close != null) {
      return;
    }
    try {
      _controller.observeMode(_handle, enabled: true);
    } on NativeFailure catch (failure, stackTrace) {
      final error = _operationFailure(failure, operation: 'modeChanges.listen');
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  void _stopModeObservation() {
    if (!capabilities.terminalModes || _close != null) {
      return;
    }
    try {
      _controller.observeMode(_handle, enabled: false);
    } on NativeFailure catch (failure, stackTrace) {
      final error = _operationFailure(failure, operation: 'modeChanges.cancel');
      if (!_modeController.isClosed) {
        _modeController.addError(error, stackTrace);
      }
    }
  }

  static PtyTermMode _mode(int bits) => PtyTermMode(
    canonical: bits & 1 != 0,
    echo: bits & 2 != 0,
    signals: bits & 4 != 0,
  );

  PtyException _operationFailure(
    NativeFailure failure, {
    required String operation,
  }) {
    final error = _exception(failure, operation: operation);
    if (error is PtyInfrastructureException) {
      nativeInfrastructureFailed(failure);
    }
    return error;
  }
}

PtyException _exception(NativeFailure failure, {String? operation}) {
  final publicOperation = operation ?? _operationName(failure.operation);
  final nativeCode = failure.nativeCode == 0 ? null : failure.nativeCode;
  if (publicOperation == 'mode' || publicOperation.startsWith('modeChanges.')) {
    return PtyModeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == PTYX_STATUS_BACKPRESSURE ||
      failure.kind == PTYX_ERROR_QUEUE_FULL) {
    return PtyBackpressureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == PTYX_STATUS_INVALID_ARGUMENT ||
      failure.kind == PTYX_ERROR_INVALID_ARGUMENT) {
    return PtyInvalidArgumentException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == PTYX_STATUS_UNSUPPORTED ||
      failure.kind == PTYX_ERROR_UNSUPPORTED) {
    return PtyUnsupportedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == PTYX_ERROR_DOMAIN_RUNTIME ||
      failure.kind == PTYX_ERROR_INFRASTRUCTURE_LOST) {
    return PtyInfrastructureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == PTYX_ERROR_DOMAIN_INPUT ||
      failure.operation == PTYX_OPERATION_WRITE) {
    return _inputException(failure, operation: publicOperation);
  }
  if (failure.domain == PTYX_ERROR_DOMAIN_OUTPUT ||
      failure.operation == PTYX_OPERATION_OUTPUT) {
    return PtyOutputException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == PTYX_STATUS_CLOSED ||
      failure.status == PTYX_STATUS_STALE_HANDLE ||
      failure.status == PTYX_STATUS_WRONG_STATE ||
      failure.kind == PTYX_ERROR_CLOSED ||
      failure.kind == PTYX_ERROR_STALE_HANDLE ||
      failure.kind == PTYX_ERROR_WRONG_STATE) {
    return PtyClosedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  return switch (failure.operation) {
    PTYX_OPERATION_SPAWN => PtySpawnException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    PTYX_OPERATION_TERMINATE => PtySignalException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    PTYX_OPERATION_RESIZE => PtyResizeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    PTYX_OPERATION_METADATA => PtyMetadataException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    PTYX_OPERATION_CLOSE => PtyCloseException(
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
  NativeFailure failure, {
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
  PTYX_OPERATION_SPAWN => 'spawn',
  PTYX_OPERATION_WRITE => 'write',
  PTYX_OPERATION_OUTPUT => 'output',
  PTYX_OPERATION_RESIZE => 'resize',
  PTYX_OPERATION_TERMINATE => 'kill',
  PTYX_OPERATION_METADATA => 'metadata',
  PTYX_OPERATION_CLOSE => 'close',
  _ => 'controller',
};

void _validateSpawnOptions(
  PtySpawnOptions options,
  String workingDirectory,
  Map<String, String> effectiveEnvironment,
  List<String> environment,
) {
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
  if (effectiveEnvironment.length > 4096) {
    throw PtyInvalidArgumentException(
      'process spawn accepts at most 4096 environment entries',
      operation: 'spawn',
      context: '${effectiveEnvironment.length} entries',
    );
  }
  for (final entry in effectiveEnvironment.entries) {
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
  for (final entry in environment) {
    encodedPayloadBytes += utf8.encode(entry).length;
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
  if (size.rows <= 0 ||
      size.rows > 32767 ||
      size.columns <= 0 ||
      size.columns > 32767 ||
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
