import 'dart:convert';
import 'dart:ffi';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';

typedef PtyxFinalizable = Finalizable;

int ptyxPort(SendPort port) => port.nativePort;

final class PtyxFailure implements Exception {
  final int status;
  final int domain;
  final int kind;
  final int operation;
  final int nativeCode;
  final int flags;
  final String message;

  const PtyxFailure({
    required this.status,
    required this.domain,
    required this.kind,
    required this.operation,
    required this.nativeCode,
    required this.flags,
    required this.message,
  });
}

typedef PtyxSpawnRequest = ({
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

typedef PtyxSnapshot = ({
  int? pid,
  int rows,
  int columns,
  int pixelWidth,
  int pixelHeight,
  int? modes,
  Uint8List? terminalName,
});

const ptyxStatusBackpressure = ptyx_status.PTYX_STATUS_BACKPRESSURE;
const ptyxStatusInvalidArgument = ptyx_status.PTYX_STATUS_INVALID_ARGUMENT;
const ptyxStatusUnsupported = ptyx_status.PTYX_STATUS_UNSUPPORTED;
const ptyxStatusClosed = ptyx_status.PTYX_STATUS_CLOSED;
const ptyxStatusStaleHandle = ptyx_status.PTYX_STATUS_STALE_HANDLE;
const ptyxStatusWrongState = ptyx_status.PTYX_STATUS_WRONG_STATE;
const ptyxStatusInternal = ptyx_status.PTYX_STATUS_INTERNAL;
const ptyxKindNone = ptyx_error_kind.PTYX_ERROR_NONE;
const ptyxKindQueueFull = ptyx_error_kind.PTYX_ERROR_QUEUE_FULL;
const ptyxKindInvalidArgument = ptyx_error_kind.PTYX_ERROR_INVALID_ARGUMENT;
const ptyxKindUnsupported = ptyx_error_kind.PTYX_ERROR_UNSUPPORTED;
const ptyxKindInfrastructureLost =
    ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST;
const ptyxKindClosed = ptyx_error_kind.PTYX_ERROR_CLOSED;
const ptyxKindStaleHandle = ptyx_error_kind.PTYX_ERROR_STALE_HANDLE;
const ptyxKindWrongState = ptyx_error_kind.PTYX_ERROR_WRONG_STATE;
const ptyxKindNativeFailure = ptyx_error_kind.PTYX_ERROR_NATIVE_FAILURE;
const ptyxDomainRuntime = ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME;
const ptyxDomainInput = ptyx_error_domain.PTYX_ERROR_DOMAIN_INPUT;
const ptyxDomainOutput = ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT;
const ptyxOperationSpawn = ptyx_operation.PTYX_OPERATION_SPAWN;
const ptyxOperationWrite = ptyx_operation.PTYX_OPERATION_WRITE;
const ptyxOperationOutput = ptyx_operation.PTYX_OPERATION_OUTPUT;
const ptyxOperationResize = ptyx_operation.PTYX_OPERATION_RESIZE;
const ptyxOperationTerminate = ptyx_operation.PTYX_OPERATION_TERMINATE;
const ptyxOperationExit = ptyx_operation.PTYX_OPERATION_EXIT;
const ptyxOperationMetadata = ptyx_operation.PTYX_OPERATION_METADATA;
const ptyxOperationClose = ptyx_operation.PTYX_OPERATION_CLOSE;
const ptyxOperationRuntimeShutdown =
    ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN;
const ptyxEventSpawnReady = ptyx_event_kind.PTYX_EVENT_SPAWN_READY;
const ptyxEventSpawnFailed = ptyx_event_kind.PTYX_EVENT_SPAWN_FAILED;
const ptyxEventOutput = ptyx_event_kind.PTYX_EVENT_OUTPUT;
const ptyxEventInputFailed = ptyx_event_kind.PTYX_EVENT_INPUT_FAILED;
const ptyxEventOutputFailed = ptyx_event_kind.PTYX_EVENT_OUTPUT_FAILED;
const ptyxEventInfrastructureFailed =
    ptyx_event_kind.PTYX_EVENT_INFRASTRUCTURE_FAILED;
const ptyxEventOutputDone = ptyx_event_kind.PTYX_EVENT_OUTPUT_DONE;
const ptyxEventExit = ptyx_event_kind.PTYX_EVENT_EXIT;
const ptyxEventExitFailed = ptyx_event_kind.PTYX_EVENT_EXIT_FAILED;
const ptyxEventCloseComplete = ptyx_event_kind.PTYX_EVENT_CLOSE_COMPLETE;
const ptyxEventModeChanged = ptyx_event_kind.PTYX_EVENT_MODE_CHANGED;
const ptyxEventModeFailed = ptyx_event_kind.PTYX_EVENT_MODE_FAILED;
const ptyxInvalidSession = PTYX_INVALID_SESSION;
const ptyxInvalidEventToken = PTYX_INVALID_EVENT_TOKEN;
const ptyxInvalidAdapter = PTYD_INVALID_ADAPTER;
const ptyxModeCanonical = PTYX_MODE_CANONICAL;
const ptyxModeEcho = PTYX_MODE_ECHO;
const ptyxModeSignals = PTYX_MODE_SIGNALS;
const ptyxCapabilitySignals = PTYX_CAPABILITY_SIGNALS;
const ptyxCapabilityProcessGroups = PTYX_CAPABILITY_PROCESS_GROUPS;
const ptyxCapabilityTerminalModes = PTYX_CAPABILITY_TERMINAL_MODES;
const ptyxCapabilityTerminalName = PTYX_CAPABILITY_TERMINAL_NAME;

final _runtimeFinalizer = NativeFinalizer(
  Native.addressOf<NativeFinalizerFunction>(ptyd_runtime_finalize),
);
final _sessionFinalizer = NativeFinalizer(
  Native.addressOf<NativeFinalizerFunction>(ptyd_session_finalize),
);
final _allocationFinalizer = NativeFinalizer(calloc.nativeFree);
Pointer<ptyx_error_t>? _errorStorage;
Pointer<ptyx_error_t> get _nativeError => _errorStorage ??= _allocateError();

void ptyxInitialize() {
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
}

({int adapter, int capabilities}) ptyxCreate(int port) {
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

void ptyxAttachRuntimeFinalizer(PtyxFinalizable target, int adapter) {
  _allocationFinalizer.attach(target, _nativeError.cast(), detach: target);
  _runtimeFinalizer.attach(
    target,
    Pointer<Void>.fromAddress(adapter),
    detach: target,
  );
}

void ptyxAttachSessionFinalizer(PtyxFinalizable target, int handle) {
  _sessionFinalizer.attach(
    target,
    Pointer<Void>.fromAddress(handle),
    detach: target,
    externalSize: 64 * 1024,
  );
}

void ptyxDetachSessionFinalizer(PtyxFinalizable target) {
  _sessionFinalizer.detach(target);
}

int ptyxStartSpawn(int adapter, PtyxSpawnRequest request) {
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
      _nativeError,
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw _failure(status, _nativeError);
    }
    return session.value;
  });
}

void ptyxWrite(int session, Uint8List data) {
  final status = ptyx_session_write(
    session,
    data.address,
    data.length,
    _nativeError,
  );
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw _failure(status, _nativeError);
  }
}

void ptyxCancelOutput(int session) =>
    _sessionCall((error) => ptyx_session_cancel_output(session, error));

void ptyxResize(int session, PtySize size) {
  using((arena) {
    final nativeSize = ptyx_size.$allocate(
      arena,
      rows: size.rows,
      columns: size.columns,
      pixel_width: size.pixelWidth,
      pixel_height: size.pixelHeight,
    );
    final status = ptyx_session_resize(session, nativeSize, _nativeError);
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw _failure(status, _nativeError);
    }
  });
}

bool ptyxTerminate(int session, int signal) {
  return using((arena) {
    final delivered = arena<Uint32>();
    final status = ptyx_session_terminate(
      session,
      signal,
      delivered,
      _nativeError,
    );
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw _failure(status, _nativeError);
    }
    return delivered.value != 0;
  });
}

PtyxSnapshot ptyxSnapshot(int session) {
  return using((arena) {
    final snapshot = arena<ptyx_session_snapshot_t>();
    snapshot.ref.struct_size = sizeOf<ptyx_session_snapshot_t>();
    var status = ptyx_session_snapshot(session, snapshot, _nativeError);
    if (status == ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL &&
        snapshot.ref.tty_name_required > 0) {
      final name = arena<Uint8>(snapshot.ref.tty_name_required);
      snapshot.ref
        ..tty_name = name
        ..tty_name_capacity = snapshot.ref.tty_name_required;
      status = ptyx_session_snapshot(session, snapshot, _nativeError);
    }
    if (status != ptyx_status.PTYX_STATUS_OK) {
      throw _failure(status, _nativeError);
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

void ptyxObserveMode(int session, {required bool enabled}) => _sessionCall(
  (error) => ptyx_session_observe_mode(session, enabled ? 1 : 0, error),
);

void ptyxClose(int session) =>
    _sessionCall((error) => ptyx_session_close(session, error));

PtyxFailure? ptyxAbort(int adapter) {
  final failure = using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    final status = ptyd_runtime_abort(handle, _nativeError);
    return status == ptyx_status.PTYX_STATUS_OK
        ? null
        : _failure(status, _nativeError);
  });
  return failure;
}

void ptyxAcknowledge(int adapter, int token) {
  final status = ptyd_event_ack(adapter, token, nullptr);
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw _acknowledgementFailure(status);
  }
}

PtyxFailure? ptyxReleaseSession(int adapter, int handle) {
  if (handle == PTYX_INVALID_SESSION) {
    return null;
  }
  PtyxFailure? failure;
  try {
    using((arena) {
      final session = arena<ptyx_session_t>()..value = handle;
      final status = ptyd_session_release(adapter, session, _nativeError);
      if (status != ptyx_status.PTYX_STATUS_OK &&
          status != ptyx_status.PTYX_STATUS_STALE_HANDLE) {
        failure = _failure(status, _nativeError);
      }
    });
  } finally {
    if (failure != null) {
      ptyd_session_finalize(Pointer<Void>.fromAddress(handle));
    }
  }
  return failure;
}

void ptyxOwnerCleanup(int adapter) {
  if (adapter == PTYD_INVALID_ADAPTER) {
    return;
  }
  final remaining = using((arena) {
    final handle = arena<ptyd_adapter_t>()..value = adapter;
    ptyd_runtime_detach(handle, nullptr);
    return handle.value;
  });
  if (remaining != PTYD_INVALID_ADAPTER) {
    ptyd_runtime_finalize(Pointer<Void>.fromAddress(remaining));
  }
}

PtyxFailure ptyxFailureFromValues({
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
    return _failure(ptyx_status.PTYX_STATUS_INTERNAL, error);
  });
}

void _sessionCall(int Function(Pointer<ptyx_error_t>) operation) {
  final status = operation(_nativeError);
  if (status != ptyx_status.PTYX_STATUS_OK) {
    throw _failure(status, _nativeError);
  }
}

Pointer<ptyx_error_t> _allocateError() {
  final error = calloc<ptyx_error_t>();
  error.ref.struct_size = sizeOf<ptyx_error_t>();
  return error;
}

Pointer<ptyx_error_t> _newError(Arena arena) {
  final error = arena<ptyx_error_t>();
  error.ref.struct_size = sizeOf<ptyx_error_t>();
  return error;
}

PtyxFailure _failure(int status, Pointer<ptyx_error_t> error) {
  final value = error.ref;
  return PtyxFailure(
    status: status,
    domain: value.domain,
    kind: value.kind,
    operation: value.operation,
    nativeCode: value.native_code,
    flags: value.flags,
    message: _formatError(error),
  );
}

PtyxFailure _acknowledgementFailure(int status) => PtyxFailure(
  status: status,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
  operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
  nativeCode: 0,
  flags: 0,
  message: 'native output acknowledgement failed',
);

String _formatError(Pointer<ptyx_error_t> error) {
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
  if (values.isEmpty) {
    return nullptr;
  }
  final views = arena<ptyx_bytes_view_t>(values.length);
  for (var index = 0; index < values.length; index++) {
    _setView(arena, views[index], utf8.encode(values[index]));
  }
  return views;
}
