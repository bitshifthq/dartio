import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'types.dart';

PtyxFailure ptyxFailureFromNative(int status, Pointer<ptyx_error_t> error) {
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

PtyxFailure ptyxAcknowledgementFailure(int status) => PtyxFailure(
  status: status,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
  operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
  nativeCode: 0,
  flags: 0,
  message: 'native output acknowledgement failed',
);

PtyCapabilities ptyxCapabilitiesFromBits(int bits) => PtyCapabilities(
  signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
  processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
  terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
  terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
);

PtyTermMode ptyxModeFromBits(int bits) => PtyTermMode(
  canonical: bits & PTYX_MODE_CANONICAL != 0,
  echo: bits & PTYX_MODE_ECHO != 0,
  signals: bits & PTYX_MODE_SIGNALS != 0,
);

PtyException ptyxException(PtyxFailure failure, {String? operation}) {
  final publicOperation = operation ?? _operationName(failure.operation);
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
      failure.operation == ptyx_operation.PTYX_OPERATION_WRITE) {
    return ptyxInputError(failure, operation: publicOperation);
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT ||
      failure.operation == ptyx_operation.PTYX_OPERATION_OUTPUT) {
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
  return switch (failure.operation) {
    ptyx_operation.PTYX_OPERATION_SPAWN => PtySpawnException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_TERMINATE => PtySignalException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_EXIT => PtyExitException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_RESIZE => PtyResizeException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_METADATA => PtyMetadataException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    ),
    ptyx_operation.PTYX_OPERATION_CLOSE => PtyCloseException(
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

PtyxFailure ptyxFailureFromEvent({
  required int domain,
  required int kind,
  required int operation,
  required int nativeCode,
  required int flags,
}) => PtyxFailure(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: domain,
  kind: kind,
  operation: operation,
  nativeCode: nativeCode,
  flags: flags,
  message: 'native PTY operation failed',
);

PtyxFailure ptyxRuntimeShutdownFailure() => ptyxInfrastructureFailure(
  operation: ptyx_operation.PTYX_OPERATION_RUNTIME_SHUTDOWN,
);

PtyxFailure ptyxOutputInfrastructureFailure({
  String message = 'native PTY operation failed',
}) => ptyxInfrastructureFailure(
  operation: ptyx_operation.PTYX_OPERATION_OUTPUT,
  message: message,
);

PtyxFailure ptyxSpawnFailure() =>
    ptyxNativeFailure(operation: ptyx_operation.PTYX_OPERATION_SPAWN);

PtyInputException ptyxInputError(
  PtyxFailure failure, {
  required String operation,
}) => PtyInputException(
  failure.message,
  operation: operation,
  nativeCode: failure.nativeCode == 0 ? null : failure.nativeCode,
);

PtyInputException ptyxInputErrorForOperation(
  String operation,
  PtyInputException failure,
) => PtyInputException(
  failure.message,
  operation: operation,
  nativeCode: failure.nativeCode,
  context: failure.context,
);

PtyxFailure ptyxInfrastructureFailure({
  required int operation,
  String message = 'native PTY operation failed',
}) => PtyxFailure(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
  operation: operation,
  nativeCode: 0,
  flags: 0,
  message: message,
);

PtyxFailure ptyxNativeFailure({
  required int operation,
  String message = 'native PTY operation failed',
}) => PtyxFailure(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: ptyx_error_kind.PTYX_ERROR_NATIVE_FAILURE,
  operation: operation,
  nativeCode: 0,
  flags: 0,
  message: message,
);

String _operationName(int operation) => switch (operation) {
  ptyx_operation.PTYX_OPERATION_SPAWN => 'spawn',
  ptyx_operation.PTYX_OPERATION_WRITE => 'write',
  ptyx_operation.PTYX_OPERATION_OUTPUT => 'output',
  ptyx_operation.PTYX_OPERATION_RESIZE => 'resize',
  ptyx_operation.PTYX_OPERATION_TERMINATE => 'kill',
  ptyx_operation.PTYX_OPERATION_EXIT => 'exit',
  ptyx_operation.PTYX_OPERATION_METADATA => 'metadata',
  ptyx_operation.PTYX_OPERATION_CLOSE => 'close',
  _ => 'controller',
};

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
