part of 'api.dart';

/// Base exception thrown by ptyx operations.
class PtyException implements Exception {
  /// A human-readable error message.
  final String message;

  /// Creates a PTY exception with [message].
  const PtyException(this.message);

  String get _name => 'PtyException';

  @override
  String toString() => '$_name: $message';
}

/// Thrown when an operation requires an open session.
///
/// Closing a session is idempotent. Operations such as [PtySession.write],
/// [PtySession.resize], and metadata getters require native handles released by
/// [PtySession.close].
class PtyClosedException extends PtyException {
  /// Creates a closed-session exception with [message].
  const PtyClosedException(super.message);

  @override
  String get _name => 'PtyClosedException';
}

/// Thrown when the current platform does not support an operation.
///
/// Optional platform capabilities may also be represented by nullable values,
/// such as [PtySession.pid], [PtySession.ttyName], and [PtySession.mode].
class PtyUnsupportedException extends PtyException {
  /// Creates an unsupported-operation exception with [message].
  const PtyUnsupportedException(super.message);

  @override
  String get _name => 'PtyUnsupportedException';
}
