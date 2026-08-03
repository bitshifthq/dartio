import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';
import 'types.dart';

Pointer<ptyx_error_t>? _errorStorage;

Pointer<ptyx_error_t> _error() => _errorStorage ??= _allocateError();

int abiVersion() => ptyx_abi_version();

int initializeApi(int apiData) => ptyd_initialize(Pointer.fromAddress(apiData));

int runtimeCreate() {
  return using((arena) {
    final runtime = arena<ptyx_runtime_t>();
    final status = ptyx_runtime_create(nullptr, runtime, _error());
    _check(status, _error());
    return runtime.value;
  });
}

int runtimeAttach(int runtime, int port) {
  return using((arena) {
    final adapter = arena<ptyd_adapter_t>();
    final status = ptyd_runtime_attach(runtime, port, adapter, _error());
    _check(status, _error());
    return adapter.value;
  });
}

PtyCapabilities runtimeCapabilities(int adapter) {
  return using((arena) {
    final bits = arena<Uint32>();
    final status = ptyd_runtime_capabilities(adapter, bits, _error());
    _check(status, _error());
    return capabilitiesFromBits(bits.value);
  });
}

void runtimeShutdown(int runtime) =>
    _call((error) => ptyx_runtime_shutdown(runtime, error));

void runtimeRelease(int runtime) {
  using((arena) {
    final handle = arena<ptyx_runtime_t>()..value = runtime;
    final status = ptyx_runtime_release(handle, _error());
    _check(status, _error());
  });
}

void runtimeDetach(int adapter) {
  using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    final status = ptyd_runtime_detach(handle, _error());
    _check(status, _error());
  });
}

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
      _error(),
    );
    _check(status, _error());
    return session.value;
  });
}

void sessionWrite(int session, Uint8List data) {
  final status = ptyx_session_write(
    session,
    data.address,
    data.length,
    _error(),
  );
  _check(status, _error());
}

void sessionCancelOutput(int session) =>
    _call((error) => ptyx_session_cancel_output(session, error));

void sessionResize(int session, PtySize size) {
  using((arena) {
    final nativeSize = ptyx_size.$allocate(
      arena,
      rows: size.rows,
      columns: size.columns,
      pixel_width: size.pixelWidth,
      pixel_height: size.pixelHeight,
    );
    final status = ptyx_session_resize(session, nativeSize, _error());
    _check(status, _error());
  });
}

bool sessionTerminate(int session, int signal) {
  return using((arena) {
    final delivered = arena<Uint32>();
    final status = ptyx_session_terminate(session, signal, delivered, _error());
    _check(status, _error());
    return delivered.value != 0;
  });
}

PtySize sessionSize(int session) {
  return using((arena) {
    final size = arena<ptyx_size_t>();
    final status = ptyx_session_get_size(session, size, _error());
    _check(status, _error());
    return PtySize(
      rows: size.ref.rows,
      columns: size.ref.columns,
      pixelWidth: size.ref.pixel_width,
      pixelHeight: size.ref.pixel_height,
    );
  });
}

int? sessionPid(int session) {
  return using((arena) {
    final pid = arena<Int64>();
    final status = ptyx_session_get_child_pid(session, pid, _error());
    _check(status, _error());
    return pid.value < 0 ? null : pid.value;
  });
}

PtyTermMode sessionMode(int session) {
  return using((arena) {
    final mode = arena<Uint32>();
    final status = ptyx_session_get_term_mode(session, mode, _error());
    _check(status, _error());
    return modeFromBits(mode.value);
  });
}

String? sessionTtyName(int session) {
  return using((arena) {
    final required = arena<Uint64>();
    var status = ptyx_session_get_tty_name(
      session,
      nullptr,
      0,
      required,
      _error(),
    );
    if (status == ptyx_status.PTYX_STATUS_OK && required.value == 0) {
      return null;
    }
    if (status != ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL) {
      _check(status, _error());
    }
    final bytes = arena<Uint8>(required.value);
    status = ptyx_session_get_tty_name(
      session,
      bytes,
      required.value,
      required,
      _error(),
    );
    _check(status, _error());
    return utf8.decode(bytes.asTypedList(required.value));
  });
}

void sessionObserveMode(int session, {required bool enabled}) => _call(
  (error) => ptyx_session_observe_mode(session, enabled ? 1 : 0, error),
);

void sessionClose(int session) =>
    _call((error) => ptyx_session_close(session, error));

void runtimeAbort(int adapter) {
  using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    final error = _error();
    final status = ptyd_runtime_abort(handle, error);
    _check(status, error);
  });
}

void eventAcknowledge(int adapter, int token) =>
    _call((error) => ptyd_event_ack(adapter, token, error));

void sessionRelease(int adapter, int session) {
  using((arena) {
    final handle = arena<ptyx_session_t>()..value = session;
    final status = ptyd_session_release(adapter, handle, _error());
    if (status != ptyx_status.PTYX_STATUS_STALE_HANDLE) {
      _check(status, _error());
    }
  });
}

void _call(int Function(Pointer<ptyx_error_t>) operation) {
  final error = _error();
  final status = operation(error);
  _check(status, error);
}

void _check(int status, Pointer<ptyx_error_t> error) {
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw failureFromNative(status, error);
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
