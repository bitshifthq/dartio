part of '../api/api.dart';

const _guardianStartupTimeout = Duration(seconds: 30);
const _guardianUnarmedLease = Duration(seconds: 5);
const _guardianCreate = 0;
const _guardianCreated = 1;
const _guardianCreateFailed = 2;
const _guardianOwnerExit = 3;

Future<void> _guardNativeOwner(SendPort ready) async {
  final commands = ReceivePort();
  final unarmedLease = Timer(_guardianUnarmedLease, commands.close);
  ready.send(commands.sendPort);
  var adapter = PTYD_INVALID_ADAPTER;
  try {
    await for (final message in commands) {
      switch (message) {
        case [_guardianCreate, final int port, final SendPort reply]
            when adapter == PTYD_INVALID_ADAPTER:
          unarmedLease.cancel();
          try {
            final created = _NativeRuntime._createNative(port);
            adapter = created.adapter;
            reply.send([_guardianCreated, adapter, created.capabilities]);
          } on _NativeFailure catch (failure) {
            reply.send([
              _guardianCreateFailed,
              failure.status,
              failure.domain,
              failure.kind,
              failure.operation,
              failure.nativeCode,
              failure.flags,
              failure.message,
            ]);
            commands.close();
          }
        case [_guardianOwnerExit]:
          commands.close();
      }
    }
  } finally {
    unarmedLease.cancel();
    if (adapter != PTYD_INVALID_ADAPTER) {
      final remaining = using((arena) {
        final handle = arena<ptyd_adapter_t>()..value = adapter;
        ptyd_runtime_detach(handle, nullptr);
        return handle.value;
      });
      if (remaining != PTYD_INVALID_ADAPTER) {
        ptyd_runtime_finalize(Pointer<Void>.fromAddress(remaining));
      }
    }
  }
}

final class _NativeFailure implements Exception {
  final int status;
  final int domain;
  final int kind;
  final int operation;
  final int nativeCode;
  final int flags;
  final String message;

  const _NativeFailure({
    required this.status,
    required this.domain,
    required this.kind,
    required this.operation,
    required this.nativeCode,
    required this.flags,
    required this.message,
  });
}

typedef _NativeSpawnRequest = ({
  String executable,
  List<String> arguments,
  List<String> environment,
  bool inheritEnvironment,
  String workingDirectory,
  int rows,
  int columns,
  int pixelWidth,
  int pixelHeight,
  int inputCapacity,
  int outputCapacity,
  Duration gracefulCloseTimeout,
});

typedef _NativeSnapshot = ({
  int? pid,
  int rows,
  int columns,
  int pixelWidth,
  int pixelHeight,
  int? modes,
  Uint8List? terminalName,
});

final class _NativeRuntime implements Finalizable {
  static final _adapterFinalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_runtime_finalize),
  );
  static final _sessionFinalizer = NativeFinalizer(
    Native.addressOf<NativeFinalizerFunction>(ptyd_session_finalize),
  );
  static final _allocationFinalizer = NativeFinalizer(calloc.nativeFree);

  static final Future<_NativeRuntime> instance = _create();

  final RawReceivePort _port;
  final int _adapter;
  final int capabilityBits;
  final Pointer<ptyx_error_t> _writeError;
  final Map<int, _PendingSpawn> _pendingSpawns = {};
  final Map<int, WeakReference<_NativeSession>> _sessions = {};
  var _terminalDeliveryTurns = 0;
  _NativeRuntime._(this._port, this._adapter, this.capabilityBits)
    : _writeError = calloc<ptyx_error_t>() {
    _writeError.ref.struct_size = sizeOf<ptyx_error_t>();
    _allocationFinalizer.attach(this, _writeError.cast(), detach: this);
    _adapterFinalizer.attach(
      this,
      Pointer<Void>.fromAddress(_adapter),
      detach: this,
    );
  }

  static Future<_NativeRuntime> _create() async {
    if (ptyx_abi_version() != PTYX_ABI_VERSION) {
      throw StateError(
        'ptyx C ABI mismatch: expected $PTYX_ABI_VERSION, '
        'found ${ptyx_abi_version()}',
      );
    }
    if (ptyd_initialize(NativeApi.initializeApiDLData) !=
        ptyx_status.PTYX_STATUS_OK) {
      throw StateError('ptyx Dart adapter initialization failed');
    }

    late final _NativeRuntime controller;
    final port = RawReceivePort();
    final ready = ReceivePort();
    final created = ReceivePort();
    SendPort? guardian;
    try {
      await Isolate.spawn(_guardNativeOwner, ready.sendPort);
      guardian = await ready.first.timeout(_guardianStartupTimeout) as SendPort;
      Isolate.current.addOnExitListener(
        guardian,
        response: const [_guardianOwnerExit],
      );
      guardian.send([
        _guardianCreate,
        port.sendPort.nativePort,
        created.sendPort,
      ]);
      final response =
          await created.first.timeout(_guardianStartupTimeout) as List<Object?>;
      switch (response) {
        case [_guardianCreated, final int adapter, final int capabilities]:
          controller = _NativeRuntime._(port, adapter, capabilities);
        case [
          _guardianCreateFailed,
          final int status,
          final int domain,
          final int kind,
          final int operation,
          final int nativeCode,
          final int flags,
          final String message,
        ]:
          throw _NativeFailure(
            status: status,
            domain: domain,
            kind: kind,
            operation: operation,
            nativeCode: nativeCode,
            flags: flags,
            message: message,
          );
        default:
          throw StateError('ptyx owner guardian returned an invalid response');
      }
      port.handler = controller._onMessage;
      port.keepIsolateAlive = false;
      return controller;
    } on Object {
      if (guardian case final owner?) {
        Isolate.current.removeOnExitListener(owner);
        owner.send(const [_guardianOwnerExit]);
      }
      port.close();
      rethrow;
    } finally {
      ready.close();
      created.close();
    }
  }

  static ({int adapter, int capabilities}) _createNative(int port) {
    return using((arena) {
      final runtime = arena<ptyx_runtime_t>();
      final error = _newError(arena);
      final createStatus = ptyx_runtime_create(nullptr, runtime, error);
      if (createStatus != ptyx_status.PTYX_STATUS_OK) {
        throw _failure(createStatus, error);
      }

      final capabilities = arena<Uint32>();
      final capabilityStatus = ptyx_runtime_capabilities(
        runtime.value,
        capabilities,
        error,
      );
      if (capabilityStatus != ptyx_status.PTYX_STATUS_OK) {
        ptyx_runtime_shutdown(runtime.value, error);
        ptyx_runtime_release(runtime, error);
        throw _failure(capabilityStatus, error);
      }

      final adapter = arena<ptyd_adapter_t>();
      final attachStatus = ptyd_runtime_attach(
        runtime.value,
        port,
        adapter,
        error,
      );
      if (attachStatus != ptyx_status.PTYX_STATUS_OK) {
        ptyx_runtime_shutdown(runtime.value, error);
        ptyx_runtime_release(runtime, error);
        throw _failure(attachStatus, error);
      }
      return (adapter: adapter.value, capabilities: capabilities.value);
    });
  }

  void startSpawn(
    _NativeSpawnRequest request, {
    required _NativeSession Function(int handle) onReady,
    required void Function(_NativeFailure failure) onFailure,
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
        if (status != ptyx_status.PTYX_STATUS_OK) {
          throw _failure(status, error);
        }
        _pendingSpawns[session.value] = (
          onReady: onReady,
          onFailure: onFailure,
        );
      });
    } finally {
      _updateLiveness();
    }
  }

  void attachFinalizer(_NativeSession target, int handle) {
    _sessionFinalizer.attach(
      target,
      Pointer<Void>.fromAddress(handle),
      detach: target,
      externalSize: 64 * 1024,
    );
  }

  void detachFinalizer(_NativeSession target) {
    _sessionFinalizer.detach(target);
  }

  void write(int session, Uint8List data) {
    final status = ptyx_session_write(
      session,
      data.address,
      data.length,
      _writeError,
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw _failure(status, _writeError);
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
      if (status != ptyx_status.PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
    });
  }

  bool terminate(int session, int signal) {
    return using((arena) {
      final delivered = arena<Uint32>();
      final error = _newError(arena);
      final status = ptyx_session_terminate(session, signal, delivered, error);
      if (status != ptyx_status.PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
      return delivered.value != 0;
    });
  }

  _NativeSnapshot snapshot(int session) {
    return using((arena) {
      final snapshot = arena<ptyx_session_snapshot_t>();
      snapshot.ref.struct_size = sizeOf<ptyx_session_snapshot_t>();
      final error = _newError(arena);
      var status = ptyx_session_snapshot(session, snapshot, error);
      if (status == ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL &&
          snapshot.ref.tty_name_required > 0) {
        final name = arena<Uint8>(snapshot.ref.tty_name_required);
        snapshot.ref
          ..tty_name = name
          ..tty_name_capacity = snapshot.ref.tty_name_required;
        status = ptyx_session_snapshot(session, snapshot, error);
      }
      if (status != ptyx_status.PTYX_STATUS_OK) {
        throw _failure(status, error);
      }
      final flags = snapshot.ref.flags;
      final nameLength = snapshot.ref.tty_name_required;
      return (
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
    if (status != ptyx_status.PTYX_STATUS_OK) {
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
      if (status != ptyx_status.PTYX_STATUS_OK) {
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
    final failure = errorKind == ptyx_error_kind.PTYX_ERROR_NONE
        ? null
        : _failureFromValues(
            domain: errorDomain,
            kind: errorKind,
            operation: errorOperation,
            nativeCode: errorNativeCode,
            flags: errorFlags,
          );
    if (session == PTYX_INVALID_SESSION &&
        kind == ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED) {
      final terminalFailure =
          failure ??
          _failureFromValues(
            domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
            kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
            operation: ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN,
            nativeCode: 0,
            flags: 0,
          );
      final pending = _pendingSpawns.values.toList(growable: false);
      final sessions = [
        for (final reference in _sessions.values)
          if (reference.target case final _NativeSession target) target,
      ];
      if (pending.isNotEmpty || sessions.isNotEmpty) {
        _retainTerminalDeliveryTurn();
      }
      _pendingSpawns.clear();
      _sessions.clear();
      for (final spawn in pending) {
        spawn.onFailure(terminalFailure);
      }
      for (final target in sessions) {
        target._nativeInfrastructureFailed(terminalFailure);
      }
      _updateLiveness();
      return;
    }

    if (kind == ptyx_event_kind.PTYX_EVENT_SPAWN_READY) {
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
    if (kind == ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED) {
      final pending = _pendingSpawns.remove(session);
      if (pending != null) {
        _retainTerminalDeliveryTurn();
        pending.onFailure(
          failure ??
              _failureFromValues(
                domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
                kind: ptyx_error_kind.PTYX_ERROR_NATIVE_FAILURE,
                operation: ptyx_operation.PTYX_OPERATION_SPAWN,
                nativeCode: 0,
                flags: 0,
              ),
        );
      }
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
      case ptyx_event_kind.PTYX_EVENT_OUTPUT:
        if (data case final Uint8List bytes) {
          target._nativeOutput(bytes, token);
        } else {
          _ackOrRelease(session, token);
        }
      case ptyx_event_kind.PTYX_EVENT_INPUT_FAILED:
        target._nativeInputFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED:
        target._nativeOutputFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED:
        target._nativeInfrastructureFailed(failure!);
      case ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE:
        target._nativeOutputDone();
      case ptyx_event_kind.PTYX_EVENT_EXIT:
        target._nativeExit(value);
      case ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE:
        _retainTerminalDeliveryTurn();
        _sessions.remove(session);
        target._nativeCloseComplete(flags, failure);
        _updateLiveness();
      case ptyx_event_kind.PTYX_EVENT_MODE_CHANGED:
        target._nativeModeChanged(value);
      case ptyx_event_kind.PTYX_EVENT_MODE_FAILED:
        target._nativeModeFailed(failure!);
    }
  }

  void _ackOrRelease(int session, int token) {
    try {
      acknowledge(token);
    } on _NativeFailure {
      releaseSession(session);
    }
  }

  void _updateLiveness() {
    final active =
        _pendingSpawns.isNotEmpty ||
        _sessions.isNotEmpty ||
        _terminalDeliveryTurns != 0;
    _port.keepIsolateAlive = active;
  }

  void _retainTerminalDeliveryTurn() {
    // A terminal native message can synchronously queue the final output and
    // complete several Dart futures. Keep the port alive through the resulting
    // microtasks and the following event turn so a CLI cannot exit before
    // those consumers observe them.
    _terminalDeliveryTurns++;
    Timer.run(() {
      Timer.run(() {
        _terminalDeliveryTurns--;
        _updateLiveness();
      });
    });
  }

  static Pointer<ptyx_error_t> _newError(Arena arena) {
    final error = arena<ptyx_error_t>();
    error.ref.struct_size = sizeOf<ptyx_error_t>();
    return error;
  }

  static _NativeFailure _failure(int status, Pointer<ptyx_error_t> error) {
    final value = error.ref;
    return _NativeFailure(
      status: status,
      domain: value.domain,
      kind: value.kind,
      operation: value.operation,
      nativeCode: value.native_code,
      flags: value.flags,
      message: _formatError(error),
    );
  }

  static _NativeFailure _acknowledgementFailure(int status) => _NativeFailure(
    status: status,
    domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
    kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
    operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
    nativeCode: 0,
    flags: 0,
    message: 'native output acknowledgement failed',
  );

  static _NativeFailure _failureFromValues({
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
      return _NativeFailure(
        status: ptyx_status.PTYX_STATUS_INTERNAL,
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
      if (query != ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL &&
          query != ptyx_status.PTYX_STATUS_OK) {
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
      if (status != ptyx_status.PTYX_STATUS_OK) {
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

typedef _PendingSpawn = ({
  _NativeSession Function(int handle) onReady,
  void Function(_NativeFailure failure) onFailure,
});
