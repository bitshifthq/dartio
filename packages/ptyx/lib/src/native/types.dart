import '../api/api.dart';

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
