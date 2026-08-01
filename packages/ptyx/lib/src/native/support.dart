part of 'native.dart';

PtyCapabilities _capabilitiesFromBits(int bits) => PtyCapabilities(
  signals: bits & PTYX_CAPABILITY_SIGNALS != 0,
  processGroups: bits & PTYX_CAPABILITY_PROCESS_GROUPS != 0,
  terminalModes: bits & PTYX_CAPABILITY_TERMINAL_MODES != 0,
  terminalName: bits & PTYX_CAPABILITY_TERMINAL_NAME != 0,
);

PtyException _exception(_NativeFailure failure, {String? operation}) {
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
    return _inputException(failure, operation: publicOperation);
  }
  if (failure.domain == ptyx_error_domain.PTYX_ERROR_DOMAIN_OUTPUT ||
      failure.operation == ptyx_operation.PTYX_OPERATION_OUTPUT) {
    return PtyOutputException(
      failure.message,
      operation: publicOperation,
      nativeCode: nativeCode,
    );
  }
  if (failure.status == ptyx_status.PTYX_STATUS_CLOSED ||
      failure.status == ptyx_status.PTYX_STATUS_STALE_HANDLE ||
      failure.status == ptyx_status.PTYX_STATUS_WRONG_STATE ||
      failure.kind == ptyx_error_kind.PTYX_ERROR_CLOSED ||
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

PtyInputException _inputException(
  _NativeFailure failure, {
  required String operation,
}) => PtyInputException(
  failure.message,
  operation: operation,
  nativeCode: failure.nativeCode == 0 ? null : failure.nativeCode,
);

PtyInputException _inputError(String operation, PtyInputException failure) =>
    PtyInputException(
      failure.message,
      operation: operation,
      nativeCode: failure.nativeCode,
      context: failure.context,
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

_NativeSpawnRequest _snapshotSpawnRequest(PtySpawnOptions options) {
  final effectiveEnvironment = _effectiveEnvironment(options);
  return (
    executable: options.executable,
    arguments: List.unmodifiable(options.arguments),
    environment: List.unmodifiable([
      for (final entry in effectiveEnvironment.entries)
        '${entry.key}=${entry.value}',
    ]),
    inheritEnvironment: options.environmentMode == .inherit,
    workingDirectory: options.workingDirectory ?? Directory.current.path,
    rows: options.initialSize.rows,
    columns: options.initialSize.columns,
    pixelWidth: options.initialSize.pixelWidth,
    pixelHeight: options.initialSize.pixelHeight,
    inputCapacity: options.maxBufferedInput,
    outputCapacity: options.maxBufferedOutput,
    gracefulCloseTimeout: options.gracefulCloseTimeout,
  );
}

Map<String, String> _effectiveEnvironment(PtySpawnOptions options) =>
    switch (options.environmentMode) {
      .inherit || .clear => const {},
      .overlay => {...Platform.environment, ...options.environment},
      .replace => options.environment,
    };
