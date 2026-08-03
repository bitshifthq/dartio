import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';
import 'types.dart';

int abiVersion() => ptyx_abi_version();

int initializeApi(int apiData) => ptyd_initialize(Pointer.fromAddress(apiData));

int runtimeCreate() {
  return _call((error) {
    return using((arena) {
      final runtime = arena<ptyx_runtime_t>();
      final status = ptyx_runtime_create(nullptr, runtime, error);
      _checkStatus(status, error);
      return runtime.value;
    });
  });
}

int runtimeAttach(int runtime, int port) {
  return _call((error) {
    return using((arena) {
      final adapter = arena<ptyd_adapter_t>();
      final status = ptyd_runtime_attach(runtime, port, adapter, error);
      _checkStatus(status, error);
      return adapter.value;
    });
  });
}

PtyCapabilities runtimeCapabilities(int adapter) {
  return _call((error) {
    return using((arena) {
      final bits = arena<Uint32>();
      final status = ptyd_runtime_capabilities(adapter, bits, error);
      _checkStatus(status, error);
      return capabilitiesFromBits(bits.value);
    });
  });
}

void runtimeShutdown(int runtime) => _call<void>(
  (error) => _checkStatus(ptyx_runtime_shutdown(runtime, error), error),
);

void runtimeRelease(int runtime) => _call<void>((error) {
  using((arena) {
    final handle = arena<ptyx_runtime_t>()..value = runtime;
    _checkStatus(ptyx_runtime_release(handle, error), error);
  });
});

void runtimeDetach(int adapter) => _call<void>((error) {
  using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    _checkStatus(ptyd_runtime_detach(handle, error), error);
  });
});

int spawnStart(int adapter, SpawnRequest request) {
  return _call((error) {
    return using((arena) {
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
            rows: request.size.rows,
            columns: request.size.columns,
            pixel_width: request.size.pixelWidth,
            pixel_height: request.size.pixelHeight,
          )
          .ref;
      final session = arena<ptyx_session_t>();
      final status = ptyd_session_spawn_start(adapter, options, session, error);
      _checkStatus(status, error);
      return session.value;
    });
  });
}

void sessionWrite(int session, Uint8List data) => _call<void>((error) {
  final status = ptyx_session_write(session, data.address, data.length, error);
  _checkStatus(status, error);
});

void sessionCancelOutput(int session) => _call<void>(
  (error) => _checkStatus(ptyx_session_cancel_output(session, error), error),
);

void sessionResize(int session, PtySize size) => _call<void>((error) {
  using((arena) {
    final nativeSize = ptyx_size.$allocate(
      arena,
      rows: size.rows,
      columns: size.columns,
      pixel_width: size.pixelWidth,
      pixel_height: size.pixelHeight,
    );
    _checkStatus(ptyx_session_resize(session, nativeSize, error), error);
  });
});

bool sessionTerminate(int session, int signal) {
  return _call((error) {
    return using((arena) {
      final delivered = arena<Uint32>();
      _checkStatus(
        ptyx_session_terminate(session, signal, delivered, error),
        error,
      );
      return delivered.value != 0;
    });
  });
}

PtySize sessionSize(int session) {
  return _call((error) {
    return using((arena) {
      final size = arena<ptyx_size_t>();
      _checkStatus(ptyx_session_get_size(session, size, error), error);
      return PtySize(
        rows: size.ref.rows,
        columns: size.ref.columns,
        pixelWidth: size.ref.pixel_width,
        pixelHeight: size.ref.pixel_height,
      );
    });
  });
}

int? sessionPid(int session) {
  return _call((error) {
    return using((arena) {
      final pid = arena<Int64>();
      _checkStatus(ptyx_session_get_child_pid(session, pid, error), error);
      return pid.value < 0 ? null : pid.value;
    });
  });
}

PtyTermMode sessionMode(int session) {
  return _call((error) {
    return using((arena) {
      final mode = arena<Uint32>();
      _checkStatus(ptyx_session_get_term_mode(session, mode, error), error);
      return modeFromBits(mode.value);
    });
  });
}

String? sessionTtyName(int session) {
  return _call((error) {
    return using((arena) {
      final required = arena<Uint64>();
      var status = ptyx_session_get_tty_name(
        session,
        nullptr,
        0,
        required,
        error,
      );
      if (status == ptyx_status.PTYX_STATUS_OK && required.value == 0) {
        return null;
      }
      if (status != ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL) {
        _checkStatus(status, error);
      }
      final bytes = arena<Uint8>(required.value);
      status = ptyx_session_get_tty_name(
        session,
        bytes,
        required.value,
        required,
        error,
      );
      _checkStatus(status, error);
      return utf8.decode(bytes.asTypedList(required.value));
    });
  });
}

void sessionObserveMode(int session, {required bool enabled}) => _call<void>(
  (error) => _checkStatus(
    ptyx_session_observe_mode(session, enabled ? 1 : 0, error),
    error,
  ),
);

void sessionClose(int session) => _call<void>(
  (error) => _checkStatus(ptyx_session_close(session, error), error),
);

void runtimeAbort(int adapter) => _call<void>((error) {
  using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    _checkStatus(ptyd_runtime_abort(handle, error), error);
  });
});

void eventAcknowledge(int adapter, int token) => _call<void>(
  (error) => _checkStatus(ptyd_event_ack(adapter, token, error), error),
);

void sessionRelease(int adapter, int session) => _call<void>((error) {
  using((arena) {
    final handle = arena<ptyx_session_t>()..value = session;
    final status = ptyd_session_release(adapter, handle, error);
    if (status != ptyx_status.PTYX_STATUS_STALE_HANDLE) {
      _checkStatus(status, error);
    }
  });
});

T _call<T>(T Function(Pointer<ptyx_error_t>) operation) {
  return using((arena) {
    final error = arena<ptyx_error_t>()
      ..ref.struct_size = sizeOf<ptyx_error_t>();
    return operation(error);
  });
}

void _checkStatus(int status, Pointer<ptyx_error_t> error) {
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
