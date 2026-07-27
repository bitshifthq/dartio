part of 'api.dart';

/// Base exception thrown by ptyx operations.
class PtyException implements Exception {
  final String message;

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
  const PtyClosedException(super.message);

  @override
  String get _name => 'PtyClosedException';
}

/// Thrown when the current platform does not support an operation.
///
/// Optional platform capabilities may also be represented by nullable values,
/// such as [PtySession.pid], [PtySession.ttyName], and [PtySession.mode].
class PtyUnsupportedException extends PtyException {
  const PtyUnsupportedException(super.message);

  @override
  String get _name => 'PtyUnsupportedException';
}

/// Thrown when accepted input fails or input capacity can never be provided.
class PtyInputException extends PtyException {
  const PtyInputException(super.message);

  @override
  String get _name => 'PtyInputException';
}

/// Thrown when the native controller or Unix broker becomes unavailable.
class PtyInfrastructureException extends PtyException {
  const PtyInfrastructureException(super.message);

  @override
  String get _name => 'PtyInfrastructureException';
}
