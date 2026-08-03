import 'dart:ffi';

import '../api/api.dart';

typedef PtyxFinalizable = Finalizable;

final class NativeFailure implements Exception {
  final int status;
  final int domain;
  final int kind;
  final int nativeCode;
  final String message;

  const NativeFailure({
    required this.status,
    required this.domain,
    required this.kind,
    required this.nativeCode,
    required this.message,
  });
}

final class SpawnRequest {
  final String executable;
  final List<String> arguments;
  final List<String> environment;
  final bool inheritEnvironment;
  final String workingDirectory;
  final PtySize size;
  final int inputCapacity;
  final int outputCapacity;
  final Duration gracefulCloseTimeout;

  const SpawnRequest({
    required this.executable,
    required this.arguments,
    required this.environment,
    required this.inheritEnvironment,
    required this.workingDirectory,
    required this.size,
    required this.inputCapacity,
    required this.outputCapacity,
    required this.gracefulCloseTimeout,
  });
}
