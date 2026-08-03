import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'errors.dart';
import 'types.dart';

int abiVersion() => ptyx_abi_version();

void initializeApi(int apiData) {
  final status = ptyd_initialize(Pointer.fromAddress(apiData));
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw exceptionFromStatus(status, operation: 'initialize');
  }
}

int runtimeCreate() {
  return _call(
    'runtime',
    (error, check) => using((arena) {
      final runtime = arena<ptyx_runtime_t>();
      final status = ptyx_runtime_create(nullptr, runtime, error);
      check(status);
      return runtime.value;
    }),
  );
}

int runtimeAttach(int runtime, int port) {
  return _call(
    'controller',
    (error, check) => using((arena) {
      final adapter = arena<ptyd_adapter_t>();
      final status = ptyd_runtime_attach(runtime, port, adapter, error);
      check(status);
      return adapter.value;
    }),
  );
}

PtyCapabilities runtimeCapabilities(int adapter) {
  return _call(
    'capabilities',
    (error, check) => using((arena) {
      final bits = arena<Uint32>();
      final status = ptyd_runtime_capabilities(adapter, bits, error);
      check(status);
      return capabilitiesFromBits(bits.value);
    }),
  );
}

void runtimeShutdown(int runtime) => _call(
  'close',
  (error, check) => check(ptyx_runtime_shutdown(runtime, error)),
);

void runtimeRelease(int runtime) => _call(
  'close',
  (error, check) => using((arena) {
    final handle = arena<ptyx_runtime_t>()..value = runtime;
    check(ptyx_runtime_release(handle, error));
  }),
);

void runtimeDetach(int adapter) => _call(
  'close',
  (error, check) => using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    check(ptyd_runtime_detach(handle, error));
  }),
);

int spawnStart(int adapter, SpawnRequest request) {
  return _call('spawn', (error, check) {
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
      check(status);
      return session.value;
    });
  });
}

void sessionWrite(int session, Uint8List data) {
  using((arena) {
    final error = arena<ptyx_error_t>()
      ..ref.struct_size = sizeOf<ptyx_error_t>();
    final status = ptyx_session_write(
      session,
      data.address,
      data.length,
      error,
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw exceptionFromNative(status, error, operation: 'write');
    }
  });
}

void sessionCancelOutput(int session) => _call(
  'output.cancel',
  (error, check) => check(ptyx_session_cancel_output(session, error)),
);

void sessionResize(int session, PtySize size) =>
    _call('resize', (error, check) {
      using((arena) {
        final nativeSize = ptyx_size.$allocate(
          arena,
          rows: size.rows,
          columns: size.columns,
          pixel_width: size.pixelWidth,
          pixel_height: size.pixelHeight,
        );
        check(ptyx_session_resize(session, nativeSize, error));
      });
    });

bool sessionTerminate(int session, int signal) {
  return _call('kill', (error, check) {
    return using((arena) {
      final delivered = arena<Uint32>();
      check(ptyx_session_terminate(session, signal, delivered, error));
      return delivered.value != 0;
    });
  });
}

PtySize sessionSize(int session) {
  return _call('size', (error, check) {
    return using((arena) {
      final size = arena<ptyx_size_t>();
      check(ptyx_session_get_size(session, size, error));
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
  return _call('pid', (error, check) {
    return using((arena) {
      final pid = arena<Int64>();
      check(ptyx_session_get_child_pid(session, pid, error));
      return pid.value < 0 ? null : pid.value;
    });
  });
}

PtyTermMode sessionMode(int session) {
  return _call('mode', (error, check) {
    return using((arena) {
      final mode = arena<Uint32>();
      check(ptyx_session_get_term_mode(session, mode, error));
      return modeFromBits(mode.value);
    });
  });
}

String? sessionTtyName(int session) {
  return _call('ttyName', (error, check) {
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
        check(status);
      }
      final bytes = arena<Uint8>(required.value);
      status = ptyx_session_get_tty_name(
        session,
        bytes,
        required.value,
        required,
        error,
      );
      check(status);
      return utf8.decode(bytes.asTypedList(required.value));
    });
  });
}

void sessionObserveMode(int session, {required bool enabled}) => _call(
  'modeChanges.observe',
  (error, check) =>
      check(ptyx_session_observe_mode(session, enabled ? 1 : 0, error)),
);

void sessionClose(int session) =>
    _call('close', (error, check) => check(ptyx_session_close(session, error)));

void runtimeAbort(int adapter) => _call('controller', (error, check) {
  using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    check(ptyd_runtime_abort(handle, error));
  });
});

void eventAcknowledge(int adapter, int token) => _call(
  'output.ack',
  (error, check) => check(ptyd_event_ack(adapter, token, error)),
);

void sessionRelease(int adapter, int session) {
  _call(
    'close',
    (error, check) => using((arena) {
      final handle = arena<ptyx_session_t>()..value = session;
      final status = ptyd_session_release(adapter, handle, error);
      if (status != ptyx_status.PTYX_STATUS_STALE_HANDLE) {
        check(status);
      }
    }),
  );
}

T _call<T>(
  String operation,
  T Function(Pointer<ptyx_error_t>, void Function(int status)) body,
) {
  return using((arena) {
    final error = arena<ptyx_error_t>()
      ..ref.struct_size = sizeOf<ptyx_error_t>();
    void check(int status) {
      if (status != ptyx_status.PTYX_STATUS_OK) {
        throw exceptionFromNative(status, error, operation: operation);
      }
    }

    return body(error, check);
  });
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
