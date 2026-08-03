import 'dart:ffi';
import 'dart:typed_data';

typedef PtyxFinalizable = Finalizable;

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
