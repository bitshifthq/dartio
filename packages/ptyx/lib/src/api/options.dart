part of 'api.dart';

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
///
/// final session = PtySession.spawn(options);
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

  /// Creates spawn options for [PtySession.spawn].
  const PtySpawnOptions({
    required this.executable,
    required this.initialSize,
    this.arguments = const [],
    this.environment = const {},
    this.environmentMode = .overlay,
    this.workingDirectory,
  });
}
