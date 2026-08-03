part of 'api.dart';

bool _sameArguments(List<String> left, List<String> right) =>
    left.length == right.length &&
    left.indexed.every((entry) => entry.$2 == right[entry.$1]);

bool _sameEnvironment(Map<String, String> left, Map<String, String> right) {
  if (left.length != right.length) return false;
  for (final entry in left.entries) {
    if (right[entry.key] != entry.value) return false;
  }
  return true;
}

/// How spawn options build the child process environment.
enum PtyEnvironmentMode {
  /// Inherits the parent environment and ignores [PtySpawnOptions.environment].
  inherit,

  /// Inherits the parent environment and overlays
  /// [PtySpawnOptions.environment].
  overlay,

  /// Starts with an empty environment and applies
  /// [PtySpawnOptions.environment].
  replace,

  /// Starts with an empty environment and ignores
  /// [PtySpawnOptions.environment].
  clear,
}

/// Configuration used to start a [PtySession].
///
/// [executable] is the program to run. [arguments] are passed to that program,
/// without the executable name. Use an absolute executable path when command
/// lookup must be predictable.
///
/// [initialSize] is required because terminal programs often read their size
/// during startup.
///
/// Example:
///
/// ```dart
/// const options = PtySpawnOptions(
///   executable: '/usr/bin/env',
///   arguments: ['TERM=xterm-256color', 'bash'],
///   environment: {'LANG': 'en_US.UTF-8'},
///   initialSize: PtySize(rows: 30, columns: 100),
/// );
/// ```
@immutable
final class PtySpawnOptions {
  /// The executable path or command name.
  ///
  /// Relative names use native process lookup rules.
  final String executable;

  /// Arguments passed to [executable].
  ///
  /// Do not include [executable] itself as the first argument.
  final List<String> arguments;

  /// Environment entries applied according to [environmentMode].
  ///
  /// Each map entry becomes a `KEY=VALUE` environment entry. Empty keys and NUL
  /// bytes are rejected. Some platforms may still provide entries required for
  /// process startup.
  final Map<String, String> environment;

  /// Controls how [environment] is combined with the parent environment.
  final PtyEnvironmentMode environmentMode;

  /// The working directory for the child process.
  ///
  /// A `null` value uses the parent's current working directory.
  final String? workingDirectory;

  /// The initial pseudo terminal size.
  ///
  /// [PtySession.resize] can change the size after the session starts.
  final PtySize initialSize;

  /// Maximum accepted input bytes retained by this session.
  ///
  /// Must be between 1 byte and 64 MiB, inclusive.
  final int maxBufferedInput;

  /// Maximum queued and in-flight output bytes retained by this session.
  ///
  /// Must be between 1 byte and 64 MiB, inclusive.
  final int maxBufferedOutput;

  /// Time allowed for graceful termination before forced cleanup.
  ///
  /// Must be between zero and one minute, inclusive. Unix uses this interval
  /// after requesting graceful termination. ConPTY has no equivalent portable
  /// request, so Windows begins Job Object termination immediately.
  final Duration gracefulCloseTimeout;

  /// Creates spawn options for [PtySession.spawn].
  const PtySpawnOptions({
    required this.executable,
    required this.initialSize,
    this.arguments = const [],
    this.environment = const {},
    this.environmentMode = .overlay,
    this.workingDirectory,
    this.maxBufferedInput = 1024 * 1024,
    this.maxBufferedOutput = 256 * 1024,
    this.gracefulCloseTimeout = const Duration(milliseconds: 250),
  });

  @override
  int get hashCode => Object.hash(
    executable,
    Object.hashAll(arguments),
    Object.hashAllUnordered(
      environment.entries.map((entry) => Object.hash(entry.key, entry.value)),
    ),
    environmentMode,
    workingDirectory,
    initialSize,
    maxBufferedInput,
    maxBufferedOutput,
    gracefulCloseTimeout,
  );

  @override
  bool operator ==(Object other) =>
      other is PtySpawnOptions &&
      executable == other.executable &&
      _sameArguments(arguments, other.arguments) &&
      _sameEnvironment(environment, other.environment) &&
      environmentMode == other.environmentMode &&
      workingDirectory == other.workingDirectory &&
      initialSize == other.initialSize &&
      maxBufferedInput == other.maxBufferedInput &&
      maxBufferedOutput == other.maxBufferedOutput &&
      gracefulCloseTimeout == other.gracefulCloseTimeout;
}
