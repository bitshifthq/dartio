part of 'api.dart';

/// The observed termination status of a session's direct child.
@immutable
sealed class PtyExitStatus {
  const PtyExitStatus();

  /// The raw status exposed by [PtySession.exitCode].
  int get rawCode;
}

/// A direct child that exited with a native numeric status.
final class PtyExited extends PtyExitStatus {
  /// The native status.
  final int code;

  /// Creates a normal or Windows-native exit status.
  const PtyExited(this.code);

  @override
  int get hashCode => code.hashCode;

  @override
  int get rawCode => code;

  @override
  bool operator ==(Object other) => other is PtyExited && other.code == code;

  @override
  String toString() => 'PtyExited($code)';
}

/// A Unix direct child that was terminated by a signal.
final class PtySignaled extends PtyExitStatus {
  /// The positive Unix signal number.
  final int signal;

  /// Creates a Unix signal-termination status.
  const PtySignaled(this.signal);

  @override
  int get hashCode => signal.hashCode;

  @override
  int get rawCode => -signal;

  @override
  bool operator ==(Object other) =>
      other is PtySignaled && other.signal == signal;

  @override
  String toString() => 'PtySignaled($signal)';
}
