import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'types.dart';

PtyCapabilities capabilitiesFromBits(int bits) => PtyCapabilities(
  signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
  processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
  terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
  terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
);

PtyException exceptionFromFailure(
  NativeFailure failure, {
  String operation = 'controller',
}) {
  final publicOperation = operation;
  final nativeCode = failure.nativeCode == 0 ? null : failure.nativeCode;
  if (publicOperation == 'mode' || publicOperation.startsWith('modeChanges.')) {
    return PtyModeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_BACKPRESSURE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_QUEUE_FULL) {
    return PtyBackpressureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_INVALID_ARGUMENT ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_INVALID_ARGUMENT) {
    return PtyInvalidArgumentException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_UNSUPPORTED ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_UNSUPPORTED) {
    return PtyUnsupportedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_CLOSED ||
      failure.status == ptyx_status.PTYX_STATUS_STALE_HANDLE ||
      failure.status == ptyx_status.PTYX_STATUS_WRONG_STATE) {
    return PtyClosedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST) {
    return PtyInfrastructureException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_INPUT ||
      publicOperation == 'write') {
    return inputException(failure: failure, operation: publicOperation);
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT ||
      publicOperation == 'output') {
    return PtyOutputException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.kind == ptyx_error_kind.PTYX_ERROR_CLOSED ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_STALE_HANDLE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_WRONG_STATE) {
    return PtyClosedException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  return switch (publicOperation) {
    'spawn' => PtySpawnException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    'kill' => PtySignalException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    'exit' => PtyExitException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    'resize' => PtyResizeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    'size' || 'pid' || 'ttyName' => PtyMetadataException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    'close' => PtyCloseException(
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

NativeFailure failureFromEvent({
  required int domain,
  required int kind,
  required int nativeCode,
}) => NativeFailure(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: domain,
  kind: kind,
  nativeCode: nativeCode,
  message: 'native PTY operation failed',
);

NativeFailure failureFromNative(int status, Pointer<ptyx_error_t> error) {
  final value = error.ref;
  return NativeFailure(
    status: status,
    domain: value.domain,
    kind: value.kind,
    nativeCode: value.native_code,
    message: _formatError(error),
  );
}

NativeFailure syntheticFailure({
  required int kind,
  String message = 'native PTY operation failed',
}) => NativeFailure(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: kind,
  nativeCode: 0,
  message: message,
);

PtyInputException inputException({
  required String operation,
  NativeFailure? failure,
  PtyInputException? previous,
}) {
  if (previous case final error?) {
    return PtyInputException(
      error.message,
      operation: operation,
      nativeCode: error.nativeCode,
      context: error.context,
    );
  }
  final error = failure!;
  return PtyInputException(
    error.message,
    operation: operation,
    nativeCode: error.nativeCode == 0 ? null : error.nativeCode,
  );
}

PtyTermMode modeFromBits(int bits) => PtyTermMode(
  canonical: bits & PTYX_MODE_CANONICAL != 0,
  echo: bits & PTYX_MODE_ECHO != 0,
  signals: bits & PTYX_MODE_SIGNALS != 0,
);

String _formatError(Pointer<ptyx_error_t> error) {
  return using((arena) {
    final required = arena<Uint64>();
    final query = ptyx_error_format(error, nullptr, 0, required);
    if (query != ptyx_status.PTYX_STATUS_BUFFER_TOO_SMALL &&
        query != ptyx_status.PTYX_STATUS_OK) {
      return 'native PTY operation failed';
    }
    if (required.value == 0) return 'native PTY operation failed';
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
