import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../ffi/ptyx.g.dart';

const _guardianStartupTimeout = Duration(seconds: 30);
const _guardianOwnerExit = 0;
const _guardianStop = 1;

Future<void> _guardNativeOwner(SendPort ready) async {
  final commands = ReceivePort();
  ready.send(commands.sendPort);
  await for (final message in commands) {
    switch (message) {
      case [_guardianOwnerExit, final int adapter]
          when adapter != PTYD_INVALID_ADAPTER:
        ptyd_runtime_finalize(Pointer<Void>.fromAddress(adapter));
        commands.close();
      case [_guardianStop]:
        commands.close();
    }
  }
}

final class NativeFailure implements Exception {
  const NativeFailure({
    required this.status,
    required this.domain,
    required this.kind,
    required this.operation,
    required this.nativeCode,
    required this.flags,
    required this.message,
  });

  final int status;
  final int domain;
  final int kind;
  final int operation;
  final int nativeCode;
  final int flags;
  final String message;
}

final class NativeSpawnRequest {
  const NativeSpawnRequest({
    required this.executable,
    required this.arguments,
    required this.environment,
    required this.inheritEnvironment,
    required this.workingDirectory,
    required this.rows,
    required this.columns,
    required this.pixelWidth,
    required this.pixelHeight,
    required this.inputCapacity,
    required this.outputCapacity,
    required this.gracefulCloseTimeout,
  });

  final String executable;
  final List<String> arguments;
  final List<String> environment;
  final bool inheritEnvironment;
  final String workingDirectory;
  final int rows;
  final int columns;
  final int pixelWidth;
  final int pixelHeight;
  final int inputCapacity;
  final int outputCapacity;
  final Duration gracefulCloseTimeout;
}

final class NativeSnapshot {
  const NativeSnapshot({
    required this.pid,
    required this.rows,
    required this.columns,
    required this.pixelWidth,
    required this.pixelHeight,
    required this.modes,
    required this.terminalName,
  });

  final int? pid;
  final int rows;
  final int columns;
  final int pixelWidth;
  final int pixelHeight;
  final int? modes;
  final Uint8List? terminalName;
}

abstract interface class NativeEventTarget {
  void nativeOutput(Uint8List bytes, int token);

  void nativeInputFailed(NativeFailure failure);

  void nativeOutputFailed(NativeFailure failure);

  void nativeInfrastructureFailed(NativeFailure failure);

  void nativeOutputDone();

  void nativeExit(int status);

  void nativeCloseComplete(int flags, NativeFailure? failure);

  void nativeModeChanged(int modes);
}

final class NativeController implements Finalizable {
  NativeController._(this._port, this._adapter, this.capabilityBits) {
    _adapterFinalizer.attach(
      this,
      Pointer<Void>.fromAddress(_adapter),
      detach: this,
    );
  }

  static final _adapterFinalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_runtime_finalize),
  );

  static final instance = NativeController._create();

  factory NativeController._create() {
    if (ptyx_abi_version() != PTYX_ABI_VERSION) {
      throw StateError(
        'ptyx C ABI mismatch: expected $PTYX_ABI_VERSION, '
        'found ${ptyx_abi_version()}',
      );
    }
    if (ptyd_initialize(NativeApi.initializeApiDLData) != PTYX_STATUS_OK) {
      throw StateError('ptyx Dart adapter initialization failed');
    }

    late final NativeController controller;
    final port = RawReceivePort();
    try {
      controller = using((arena) {
        final runtime = arena<ptyx_runtime_t>();
        final error = _newError(arena);
        final createStatus = ptyx_runtime_create(nullptr, runtime, error);
        if (createStatus != PTYX_STATUS_OK) {
          throw _failure(createStatus, error);
        }

        final capabilities = arena<Uint32>();
        final capabilityStatus = ptyx_runtime_capabilities(
          runtime.value,
          capabilities,
          error,
        );
        if (capabilityStatus != PTYX_STATUS_OK) {
          ptyx_runtime_shutdown(runtime.value, error);
          ptyx_runtime_release(runtime, error);
          throw _failure(capabilityStatus, error);
        }

        final adapter = arena<ptyd_adapter_t>();
        final attachStatus = ptyd_runtime_attach(
          runtime.value,
          port.sendPort.nativePort,
          adapter,
          error,
        );
        if (attachStatus != PTYX_STATUS_OK) {
          ptyx_runtime_shutdown(runtime.value, error);
          ptyx_runtime_release(runtime, error);
          throw _failure(attachStatus, error);
        }
        return NativeController._(port, adapter.value, capabilities.value);
      });
      port.handler = controller._onMessage;
      port.keepIsolateAlive = false;
      return controller;
    } on Object {
      port.close();
      rethrow;
    }
  }

  final RawReceivePort _port;
  final int _adapter;
  final int capabilityBits;
  final Map<int, _PendingSpawn> _pendingSpawns = {};
  final Map<int, WeakReference<NativeEventTarget>> _sessions = {};
  Future<void>? _guardianStart;
  SendPort? _guardian;
  late final _sessionFinalizer = Finalizer<int>(releaseSession);

  Future<void> ensureOwnerGuardian() {
    return _guardianStart ??= _startOwnerGuardian();
  }

  Future<void> _startOwnerGuardian() async {
    final ready = ReceivePort();
    try {
      await Isolate.spawn(_guardNativeOwner, ready.sendPort);
      final guardian =
          await ready.first.timeout(_guardianStartupTimeout) as SendPort;
      _guardian = guardian;
      Isolate.current.addOnExitListener(
        guardian,
        response: [_guardianOwnerExit, _adapter],
      );
    } on Object {
      _guardianStart = null;
      rethrow;
    } finally {
      ready.close();
    }
  }

  void startSpawn(
    NativeSpawnRequest request, {
    required NativeEventTarget Function(int handle) onReady,
    required void Function(NativeFailure failure) onFailure,
  }) {
    try {
      using((arena) {
        final options = arena<ptyx_spawn_options_t>();
        options.ref
          ..struct_size = sizeOf<ptyx_spawn_options_t>()
          ..flags = request.inheritEnvironment
              ? PTYX_SPAWN_INHERIT_ENVIRONMENT
              : 0
          ..argument_count = request.arguments.length
          ..environment_count = request.environment.length
          ..input_capacity = request.inputCapacity
          ..output_capacity = request.outputCapacity
          ..graceful_close_timeout_us =
              request.gracefulCloseTimeout.inMicroseconds;
        _setView(
          arena,
          options.ref.executable,
          utf8.encode(request.executable),
        );
        _setView(
          arena,
          options.ref.working_directory,
          utf8.encode(request.workingDirectory),
        );
        options.ref.arguments = _views(arena, request.arguments);
        options.ref.environment = _views(arena, request.environment);
        options.ref.size
          ..rows = request.rows
          ..columns = request.columns
          ..pixel_width = request.pixelWidth
          ..pixel_height = request.pixelHeight;

        final session = arena<ptyx_session_t>();
        final error = _newError(arena);
        final status = ptyd_session_spawn_start(
          _adapter,
          options,
          session,
          error,
        );
        if (status != PTYX_STATUS_OK) {
          throw _failure(status, error);
        }
        _pendingSpawns[session.value] = _PendingSpawn(onReady, onFailure);
      });
    } finally {
      _updateLiveness();
    }
  }

  void attachFinalizer(NativeEventTarget target, int handle) {
    _sessionFinalizer.attach(target, handle, detach: target);
  }

  void detachFinalizer(NativeEventTarget target) {
    _sessionFinalizer.detach(target);
  }

  void write(int session, Uint8List data) {
    final status = ptyx_session_write(
      session,
      data.address,
      data.length,
      nullptr,
    );
    if (status != PTYX_STATUS_OK) {
      throw _writeFailure(status);
    }
  }

  void cancelOutput(int session) {
    _sessionCall((error) => ptyx_session_cancel_output(session, error));
  }

  void resize(
    int session, {
    required int rows,
    required int columns,
    required int pixelWidth,
    required int pixelHeight,
  }) {
    using((arena) {
      final size = arena<ptyx_size_t>();
      size.ref
        ..rows = rows
        ..columns = columns
        ..pixel_width = pixelWidth
        ..pixel_height = pixelHeight;
      final error = _newError(arena);
      final status = ptyx_session_resize(session, size, error);
      if (status != PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
    });
  }

  bool terminate(int session, int signal) {
    return using((arena) {
      final delivered = arena<Uint32>();
      final error = _newError(arena);
      final status = ptyx_session_terminate(session, signal, delivered, error);
      if (status != PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
      return delivered.value != 0;
    });
  }

  NativeSnapshot snapshot(int session) {
    return using((arena) {
      final snapshot = arena<ptyx_session_snapshot_t>();
      snapshot.ref.struct_size = sizeOf<ptyx_session_snapshot_t>();
      final error = _newError(arena);
      var status = ptyx_session_snapshot(session, snapshot, error);
      if (status == PTYX_STATUS_BUFFER_TOO_SMALL &&
          snapshot.ref.tty_name_required > 0) {
        final name = arena<Uint8>(snapshot.ref.tty_name_required);
        snapshot.ref
          ..tty_name = name
          ..tty_name_capacity = snapshot.ref.tty_name_required;
        status = ptyx_session_snapshot(session, snapshot, error);
      }
      if (status != PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
      final flags = snapshot.ref.flags;
      final nameLength = snapshot.ref.tty_name_required;
      return NativeSnapshot(
        pid: snapshot.ref.pid < 0 ? null : snapshot.ref.pid,
        rows: snapshot.ref.size.rows,
        columns: snapshot.ref.size.columns,
        pixelWidth: snapshot.ref.size.pixel_width,
        pixelHeight: snapshot.ref.size.pixel_height,
        modes: flags & PTYX_SNAPSHOT_HAS_MODE == 0 ? null : snapshot.ref.modes,
        terminalName: flags & PTYX_SNAPSHOT_HAS_TTY_NAME == 0 || nameLength == 0
            ? null
            : Uint8List.fromList(snapshot.ref.tty_name.asTypedList(nameLength)),
      );
    });
  }

  void observeMode(int session, {required bool enabled}) {
    _sessionCall(
      (error) => ptyx_session_observe_mode(session, enabled ? 1 : 0, error),
    );
  }

  void closeSession(int session) {
    _sessionCall((error) => ptyx_session_close(session, error));
  }

  void acknowledge(int token) {
    final status = ptyd_event_ack(_adapter, token, nullptr);
    if (status != PTYX_STATUS_OK) {
      throw _acknowledgementFailure(status);
    }
  }

  void releaseSession(int handle) {
    if (handle == PTYX_INVALID_SESSION) {
      return;
    }
    try {
      using((arena) {
        final session = arena<ptyx_session_t>()..value = handle;
        final error = _newError(arena);
        ptyd_session_release(_adapter, session, error);
      });
    } finally {
      _pendingSpawns.remove(handle);
      _sessions.remove(handle);
      _updateLiveness();
    }
  }

  void _sessionCall(int Function(Pointer<ptyx_error_t> error) operation) {
    using((arena) {
      final error = _newError(arena);
      final status = operation(error);
      if (status != PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
    });
  }

  void _onMessage(Object? message) {
    if (message is! List<Object?> || message.length != 11) {
      return;
    }
    if (message.take(10).any((value) => value is! int)) {
      return;
    }
    final kind = message[0]! as int;
    final session = message[1]! as int;
    final token = message[2]! as int;
    final flags = message[3]! as int;
    final value = message[4]! as int;
    final errorDomain = message[5]! as int;
    final errorKind = message[6]! as int;
    final errorOperation = message[7]! as int;
    final errorNativeCode = message[8]! as int;
    final errorFlags = message[9]! as int;
    final data = message[10];
    final failure = errorKind == PTYX_ERROR_NONE
        ? null
        : _failureFromValues(
            domain: errorDomain,
            kind: errorKind,
            operation: errorOperation,
            nativeCode: errorNativeCode,
            flags: errorFlags,
          );
    if (session == PTYX_INVALID_SESSION &&
        kind == PTYX_EVENT_INFRASTRUCTURE_FAILED) {
      final terminalFailure =
          failure ??
          _failureFromValues(
            domain: PTYX_ERROR_DOMAIN_RUNTIME,
            kind: PTYX_ERROR_INFRASTRUCTURE_LOST,
            operation: PTYX_OPERATION_RUNTIME_SHUTDOWN,
            nativeCode: 0,
            flags: 0,
          );
      final pending = _pendingSpawns.values.toList(growable: false);
      final sessions = [
        for (final reference in _sessions.values)
          if (reference.target case final NativeEventTarget target) target,
      ];
      _pendingSpawns.clear();
      _sessions.clear();
      for (final spawn in pending) {
        spawn.onFailure(terminalFailure);
      }
      for (final target in sessions) {
        target.nativeInfrastructureFailed(terminalFailure);
      }
      _updateLiveness();
      return;
    }

    if (kind == PTYX_EVENT_SPAWN_READY) {
      final pending = _pendingSpawns.remove(session);
      if (pending == null) {
        releaseSession(session);
        return;
      }
      final target = pending.onReady(session);
      _sessions[session] = WeakReference(target);
      _updateLiveness();
      return;
    }
    if (kind == PTYX_EVENT_SPAWN_FAILED) {
      final pending = _pendingSpawns.remove(session);
      pending?.onFailure(
        failure ??
            _failureFromValues(
              domain: PTYX_ERROR_DOMAIN_RUNTIME,
              kind: PTYX_ERROR_NATIVE_FAILURE,
              operation: PTYX_OPERATION_SPAWN,
              nativeCode: 0,
              flags: 0,
            ),
      );
      _updateLiveness();
      return;
    }

    final target = _sessions[session]?.target;
    if (target == null) {
      if (token != PTYX_INVALID_EVENT_TOKEN) {
        _ackOrRelease(session, token);
      }
      releaseSession(session);
      return;
    }
    switch (kind) {
      case PTYX_EVENT_OUTPUT:
        if (data case final Uint8List bytes) {
          target.nativeOutput(bytes, token);
        } else {
          _ackOrRelease(session, token);
        }
      case PTYX_EVENT_INPUT_FAILED:
        target.nativeInputFailed(failure!);
      case PTYX_EVENT_OUTPUT_FAILED:
        target.nativeOutputFailed(failure!);
      case PTYX_EVENT_INFRASTRUCTURE_FAILED:
        target.nativeInfrastructureFailed(failure!);
      case PTYX_EVENT_OUTPUT_DONE:
        target.nativeOutputDone();
      case PTYX_EVENT_EXIT:
        target.nativeExit(value);
      case PTYX_EVENT_CLOSE_COMPLETE:
        _sessions.remove(session);
        target.nativeCloseComplete(flags, failure);
        _updateLiveness();
      case PTYX_EVENT_MODE_CHANGED:
        target.nativeModeChanged(value);
    }
  }

  void _ackOrRelease(int session, int token) {
    try {
      acknowledge(token);
    } on NativeFailure {
      releaseSession(session);
    }
  }

  void _updateLiveness() {
    final active = _pendingSpawns.isNotEmpty || _sessions.isNotEmpty;
    _port.keepIsolateAlive = active;
    if (!active) {
      final guardian = _guardian;
      if (guardian != null) {
        Isolate.current.removeOnExitListener(guardian);
        guardian.send(const [_guardianStop]);
        _guardian = null;
        _guardianStart = null;
      }
    }
  }

  static Pointer<ptyx_error_t> _newError(Arena arena) {
    final error = arena<ptyx_error_t>();
    error.ref.struct_size = sizeOf<ptyx_error_t>();
    return error;
  }

  static NativeFailure _failure(int status, Pointer<ptyx_error_t> error) {
    final value = error.ref;
    return NativeFailure(
      status: status,
      domain: value.domain,
      kind: value.kind,
      operation: value.operation,
      nativeCode: value.native_code,
      flags: value.flags,
      message: _formatError(error),
    );
  }

  static NativeFailure _writeFailure(int status) {
    final (domain, kind, message) = switch (status) {
      PTYX_STATUS_BACKPRESSURE => (
        PTYX_ERROR_DOMAIN_INPUT,
        PTYX_ERROR_QUEUE_FULL,
        'bounded native input storage is full',
      ),
      PTYX_STATUS_INVALID_ARGUMENT => (
        PTYX_ERROR_DOMAIN_ARGUMENT,
        PTYX_ERROR_INVALID_ARGUMENT,
        'native write arguments are invalid',
      ),
      PTYX_STATUS_CLOSED ||
      PTYX_STATUS_STALE_HANDLE ||
      PTYX_STATUS_WRONG_STATE => (
        PTYX_ERROR_DOMAIN_INPUT,
        PTYX_ERROR_CLOSED,
        'session can no longer accept input',
      ),
      _ => (
        PTYX_ERROR_DOMAIN_RUNTIME,
        PTYX_ERROR_INFRASTRUCTURE_LOST,
        'native runtime infrastructure was lost',
      ),
    };
    return NativeFailure(
      status: status,
      domain: domain,
      kind: kind,
      operation: PTYX_OPERATION_WRITE,
      nativeCode: 0,
      flags: 0,
      message: message,
    );
  }

  static NativeFailure _acknowledgementFailure(int status) => NativeFailure(
    status: status,
    domain: PTYX_ERROR_DOMAIN_RUNTIME,
    kind: PTYX_ERROR_INFRASTRUCTURE_LOST,
    operation: PTYX_OPERATION_OUTPUT,
    nativeCode: 0,
    flags: 0,
    message: 'native output acknowledgement failed',
  );

  static NativeFailure _failureFromValues({
    required int domain,
    required int kind,
    required int operation,
    required int nativeCode,
    required int flags,
  }) {
    return using((arena) {
      final error = _newError(arena);
      error.ref
        ..domain = domain
        ..kind = kind
        ..operation = operation
        ..native_code = nativeCode
        ..flags = flags;
      return NativeFailure(
        status: PTYX_STATUS_INTERNAL,
        domain: domain,
        kind: kind,
        operation: operation,
        nativeCode: nativeCode,
        flags: flags,
        message: _formatError(error),
      );
    });
  }

  static String _formatError(Pointer<ptyx_error_t> error) {
    return using((arena) {
      final required = arena<Uint64>();
      final query = ptyx_error_format(error, nullptr, 0, required);
      if (query != PTYX_STATUS_BUFFER_TOO_SMALL && query != PTYX_STATUS_OK) {
        return 'native PTY operation failed';
      }
      if (required.value == 0) {
        return 'native PTY operation failed';
      }
      final bytes = arena<Uint8>(required.value + 1);
      final status = ptyx_error_format(
        error,
        bytes,
        required.value + 1,
        required,
      );
      if (status != PTYX_STATUS_OK) {
        return 'native PTY operation failed';
      }
      return utf8.decode(bytes.asTypedList(required.value));
    });
  }

  static void _setView(Arena arena, ptyx_bytes_view_t view, List<int> bytes) {
    if (bytes.isEmpty) {
      view
        ..data = nullptr
        ..length = 0;
      return;
    }
    final data = arena<Uint8>(bytes.length);
    data.asTypedList(bytes.length).setAll(0, bytes);
    view
      ..data = data
      ..length = bytes.length;
  }

  static Pointer<ptyx_bytes_view_t> _views(Arena arena, List<String> values) {
    if (values.isEmpty) {
      return nullptr;
    }
    final views = arena<ptyx_bytes_view_t>(values.length);
    for (var index = 0; index < values.length; index++) {
      _setView(arena, views[index], utf8.encode(values[index]));
    }
    return views;
  }
}

final class _PendingSpawn {
  const _PendingSpawn(this.onReady, this.onFailure);

  final NativeEventTarget Function(int handle) onReady;
  final void Function(NativeFailure failure) onFailure;
}
