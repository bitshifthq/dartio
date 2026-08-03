import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';
import 'types.dart';

Pointer<ptyx_error_t>? _errorStorage;

Pointer<ptyx_error_t> nativeError() => _errorStorage ??= _allocateError();

Pointer<ptyx_error_t> newError(Arena arena) {
  final error = arena<ptyx_error_t>();
  error.ref.struct_size = sizeOf<ptyx_error_t>();
  return error;
}

int abiVersion() => ptyx_abi_version();

int initializeApi(Pointer<Void> apiData) => ptyd_initialize(apiData);

int runtimeCreate(
  Pointer<ptyx_runtime_options_t>? options,
  Pointer<ptyx_runtime_t> runtime,
  Pointer<ptyx_error_t> error,
) => ptyx_runtime_create(options ?? nullptr, runtime, error);

int runtimeCapabilities(
  int runtime,
  Pointer<Uint32> capabilities,
  Pointer<ptyx_error_t> error,
) => ptyx_runtime_capabilities(runtime, capabilities, error);

int runtimeAttach(
  int runtime,
  int port,
  Pointer<ptyd_adapter_t> adapter,
  Pointer<ptyx_error_t> error,
) => ptyd_runtime_attach(runtime, port, adapter, error);

int runtimeShutdown(int runtime, Pointer<ptyx_error_t> error) =>
    ptyx_runtime_shutdown(runtime, error);

int runtimeRelease(
  Pointer<ptyx_runtime_t> runtime,
  Pointer<ptyx_error_t> error,
) => ptyx_runtime_release(runtime, error);

int runtimeDetach(
  Pointer<ptyd_adapter_t> adapter,
  Pointer<ptyx_error_t> error,
) => ptyd_runtime_detach(adapter, error);

void runtimeFinalize(Pointer<Void> token) => ptyd_runtime_finalize(token);

void sessionFinalize(Pointer<Void> token) => ptyd_session_finalize(token);

int spawnStart(int adapter, PtyxSpawnRequest request) {
  return using((arena) {
    final options = arena<ptyx_spawn_options_t>();
    options.ref
      ..struct_size = sizeOf<ptyx_spawn_options_t>()
      ..flags = request.inheritEnvironment ? PTYX_SPAWN_INHERIT_ENVIRONMENT : 0
      ..argument_count = request.arguments.length
      ..environment_count = request.environment.length
      ..input_capacity = request.inputCapacity
      ..output_capacity = request.outputCapacity
      ..graceful_close_timeout_us = request.gracefulCloseTimeout.inMicroseconds;
    _setView(arena, options.ref.executable, utf8.encode(request.executable));
    _setView(
      arena,
      options.ref.working_directory,
      utf8.encode(request.workingDirectory),
    );
    options.ref.arguments = _views(arena, request.arguments);
    options.ref.environment = _views(arena, request.environment);
    options.ref.size = ptyx_size
        .$allocate(
          arena,
          rows: request.rows,
          columns: request.columns,
          pixel_width: request.pixelWidth,
          pixel_height: request.pixelHeight,
        )
        .ref;
    final session = arena<ptyx_session_t>();
    final status = ptyd_session_spawn_start(
      adapter,
      options,
      session,
      nativeError(),
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxFailureFromNative(status, nativeError());
    }
    return session.value;
  });
}

void sessionWrite(int session, Uint8List data) {
  final status = ptyx_session_write(
    session,
    data.address,
    data.length,
    nativeError(),
  );
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw ptyxFailureFromNative(status, nativeError());
  }
}

void sessionCancelOutput(int session) =>
    _sessionCall((error) => ptyx_session_cancel_output(session, error));

void sessionResize(int session, PtySize size) {
  using((arena) {
    final nativeSize = ptyx_size.$allocate(
      arena,
      rows: size.rows,
      columns: size.columns,
      pixel_width: size.pixelWidth,
      pixel_height: size.pixelHeight,
    );
    final status = ptyx_session_resize(session, nativeSize, nativeError());
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxFailureFromNative(status, nativeError());
    }
  });
}

bool sessionTerminate(int session, int signal) {
  return using((arena) {
    final delivered = arena<Uint32>();
    final status = ptyx_session_terminate(
      session,
      signal,
      delivered,
      nativeError(),
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxFailureFromNative(status, nativeError());
    }
    return delivered.value != 0;
  });
}

PtyxSnapshot sessionSnapshot(int session) {
  return using((arena) {
    final snapshot = arena<ptyx_session_snapshot_t>();
    snapshot.ref.struct_size = sizeOf<ptyx_session_snapshot_t>();
    var status = ptyx_session_snapshot(session, snapshot, nativeError());
    if (status == ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL &&
        snapshot.ref.tty_name_required > 0) {
      final name = arena<Uint8>(snapshot.ref.tty_name_required);
      snapshot.ref
        ..tty_name = name
        ..tty_name_capacity = snapshot.ref.tty_name_required;
      status = ptyx_session_snapshot(session, snapshot, nativeError());
    }
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw ptyxFailureFromNative(status, nativeError());
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

void sessionObserveMode(int session, {required bool enabled}) => _sessionCall(
  (error) => ptyx_session_observe_mode(session, enabled ? 1 : 0, error),
);

void sessionClose(int session) =>
    _sessionCall((error) => ptyx_session_close(session, error));

int runtimeAbort(int adapter, Pointer<ptyx_error_t> error) {
  return using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    return ptyd_runtime_abort(handle, error);
  });
}

int eventAcknowledge(int adapter, int token, Pointer<ptyx_error_t> error) =>
    ptyd_event_ack(adapter, token, error);

int sessionRelease(
  int adapter,
  Pointer<ptyx_session_t> session,
  Pointer<ptyx_error_t> error,
) => ptyd_session_release(adapter, session, error);

void _sessionCall(int Function(Pointer<ptyx_error_t>) operation) {
  final status = operation(nativeError());
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw ptyxFailureFromNative(status, nativeError());
  }
}

void _setView(Arena arena, ptyx_bytes_view_t view, List<int> bytes) {
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

Pointer<ptyx_bytes_view_t> _views(Arena arena, List<String> values) {
  if (values.isEmpty) return nullptr;
  final views = arena<ptyx_bytes_view_t>(values.length);
  for (var index = 0; index < values.length; index++) {
    _setView(arena, views[index], utf8.encode(values[index]));
  }
  return views;
}

Pointer<ptyx_error_t> _allocateError() {
  final error = calloc<ptyx_error_t>();
  error.ref.struct_size = sizeOf<ptyx_error_t>();
  return error;
}
