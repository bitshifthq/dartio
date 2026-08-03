import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';

PtyCapabilities capabilitiesFromBits(int bits) => PtyCapabilities(
  signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
  processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
  terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
  terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
);

PtyException exceptionFromNative(
  int status,
  Pointer<ptyx_error_t> error, {
  required String operation,
}) {
  final value = error.ref;
  return _exception(
    status: status,
    domain: value.domain,
    kind: value.kind,
    nativeCode: value.native_code,
    operation: operation,
    message: _formatError(error),
  );
}

PtyException exceptionFromStatus(int status, {required String operation}) =>
    _exception(
      status: status,
      domain: status == ptyx_status.PTYX_STATUS_INTERNAL
          ? ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME
          : ptyx_error_domain.PTYX_ERROR_DOMAIN_NONE,
      kind: ptyx_error_kind.PTYX_ERROR_NONE,
      nativeCode: 0,
      operation: operation,
      message: 'native PTY operation failed',
    );

PtyException exceptionFromEvent({
  required int domain,
  required int kind,
  required int nativeCode,
  required String operation,
}) => _exception(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: domain,
  kind: kind,
  nativeCode: nativeCode,
  operation: operation,
  message: 'native PTY operation failed',
);

PtyException exceptionFromValues({
  required int status,
  required int domain,
  required int kind,
  required int nativeCode,
  required String operation,
  required String message,
}) => _exception(
  status: status,
  domain: domain,
  kind: kind,
  nativeCode: nativeCode,
  operation: operation,
  message: message,
);

PtyException exceptionFromCategory({
  required PtyErrorCategory category,
  required String operation,
  required String message,
  int? nativeCode,
}) => switch (category) {
  .invalidArgument => PtyArgumentException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  .closed => PtyClosedException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  .unsupported => PtyUnsupportedException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  .input => PtyInputException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  .backpressure => PtyBackpressureException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  .infrastructure => PtyInfraException(
    message,
    operation: operation,
    nativeCode: nativeCode,
  ),
  _ => PtyException(
    message,
    operation: operation,
    category: category,
    nativeCode: nativeCode,
  ),
};

PtyException syntheticError({
  required String operation,
  int kind = ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST,
  String message = 'native PTY operation failed',
}) => _exception(
  status: ptyx_status.PTYX_STATUS_INTERNAL,
  domain: ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME,
  kind: kind,
  nativeCode: 0,
  operation: operation,
  message: message,
);

PtyTermMode modeFromBits(int bits) => PtyTermMode(
  canonical: bits & PTYX_MODE_CANONICAL != 0,
  echo: bits & PTYX_MODE_ECHO != 0,
  signals: bits & PTYX_MODE_SIGNALS != 0,
);

PtyException _exception({
  required int status,
  required int domain,
  required int kind,
  required int nativeCode,
  required String operation,
  required String message,
}) {
  final code = nativeCode == 0 ? null : nativeCode;
  if (status == ptyx_status.PTYX_STATUS_BACKPRESSURE ||
      kind == ptyx_error_kind.PTYX_ERROR_QUEUE_FULL) {
    return PtyBackpressureException(
      message,
      operation: operation,
      nativeCode: code,
    );
  }
  if (status == ptyx_status.PTYX_STATUS_INVALID_ARGUMENT ||
      kind == ptyx_error_kind.PTYX_ERROR_INVALID_ARGUMENT) {
    return PtyArgumentException(
      message,
      operation: operation,
      nativeCode: code,
    );
  }
  if (status == ptyx_status.PTYX_STATUS_UNSUPPORTED ||
      kind == ptyx_error_kind.PTYX_ERROR_UNSUPPORTED) {
    return PtyUnsupportedException(
      message,
      operation: operation,
      nativeCode: code,
    );
  }
  if (status == ptyx_status.PTYX_STATUS_CLOSED ||
      status == ptyx_status.PTYX_STATUS_STALE_HANDLE ||
      status == ptyx_status.PTYX_STATUS_WRONG_STATE) {
    return PtyClosedException(message, operation: operation, nativeCode: code);
  }
  if (domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_RUNTIME ||
      kind == ptyx_error_kind.PTYX_ERROR_INFRASTRUCTURE_LOST) {
    return PtyInfraException(message, operation: operation, nativeCode: code);
  }
  if (domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_INPUT ||
      operation == 'write') {
    return PtyInputException(message, operation: operation, nativeCode: code);
  }
  if (kind == ptyx_error_kind.PTYX_ERROR_CLOSED ||
      kind == ptyx_error_kind.PTYX_ERROR_STALE_HANDLE ||
      kind == ptyx_error_kind.PTYX_ERROR_WRONG_STATE) {
    return PtyClosedException(message, operation: operation, nativeCode: code);
  }
  return PtyException(
    message,
    operation: operation,
    category: _category(domain: domain, operation: operation),
    nativeCode: code,
  );
}

PtyErrorCategory _category({required int domain, required String operation}) {
  if (operation == 'close' || operation.startsWith('output.cancel')) {
    return .cleanup;
  }
  if (operation == 'output') {
    return .output;
  }
  if (operation == 'spawn' || operation == 'kill' || operation == 'exit') {
    return .process;
  }
  if (operation == 'resize' ||
      operation == 'mode' ||
      operation.startsWith('modeChanges.') ||
      operation == 'size' ||
      operation == 'pid' ||
      operation == 'ttyName') {
    return .terminal;
  }
  if (domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT) return .output;
  if (domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_PROCESS) return .process;
  return .unknown;
}

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
