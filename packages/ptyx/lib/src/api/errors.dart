part of 'api.dart';

/// Stable failure domains exposed by ptyx operations.
enum PtyErrorCategory {
  /// Caller input is invalid.
  invalidArgument,

  /// The session no longer accepts the operation.
  closed,

  /// The current platform or backend does not provide the operation.
  unsupported,

  /// Accepted terminal input failed permanently.
  input,

  /// Bounded terminal input storage cannot accept the complete write now.
  backpressure,

  /// Terminal output failed.
  output,

  /// A child-process operation failed.
  process,

  /// A terminal query or mutation failed.
  terminal,

  /// Session cleanup could not be established.
  cleanup,

  /// The native controller, reactor, or broker failed.
  infrastructure,

  /// No more specific domain is available.
  unknown,
}

/// Base exception thrown by ptyx operations.
base class PtyException implements Exception {
  /// Human-readable failure detail.
  final String message;

  /// The public operation or infrastructure subsystem that reported failure.
  final String operation;

  /// Stable machine-readable failure domain.
  final PtyErrorCategory category;

  /// Native or operating-system error code, when available.
  final int? nativeCode;

  /// Non-secret context useful for diagnosing the failed operation.
  final String? context;

  const PtyException(
    this.message, {
    this.operation = 'unknown',
    this.category = .unknown,
    this.nativeCode,
    this.context,
  });

  String get _name => 'PtyException';

  @override
  String toString() {
    final native = nativeCode == null ? '' : ' (native $nativeCode)';
    final detail = context == null ? '' : ': $context';
    return '$_name[$operation/${category.name}]$native: $message$detail';
  }
}

/// Thrown when native validation rejects an argument.
final class PtyArgumentException extends PtyException {
  const PtyArgumentException(
    super.message, {
    super.operation = 'validation',
    super.nativeCode,
    super.context,
  }) : super(category: .invalidArgument);

  @override
  String get _name => 'PtyArgumentException';
}

/// Thrown when an operation requires a live session.
final class PtyClosedException extends PtyException {
  const PtyClosedException(
    super.message, {
    super.operation = 'state',
    super.nativeCode,
    super.context,
  }) : super(category: .closed);

  @override
  String get _name => 'PtyClosedException';
}

/// Thrown when bounded native input admission is temporarily unavailable.
final class PtyBackpressureException extends PtyException {
  const PtyBackpressureException(
    super.message, {
    super.operation = 'write',
    super.nativeCode,
    super.context,
  }) : super(category: .backpressure);

  @override
  String get _name => 'PtyBackpressureException';
}

/// Thrown when the current platform does not support an operation.
final class PtyUnsupportedException extends PtyException {
  const PtyUnsupportedException(
    super.message, {
    super.operation = 'capability',
    super.nativeCode,
    super.context,
  }) : super(category: .unsupported);

  @override
  String get _name => 'PtyUnsupportedException';
}

/// Thrown when accepted terminal input cannot be delivered.
final class PtyInputException extends PtyException {
  const PtyInputException(
    super.message, {
    super.operation = 'input',
    super.nativeCode,
    super.context,
  }) : super(category: .input);

  @override
  String get _name => 'PtyInputException';
}

/// Thrown when the native controller or broker becomes unavailable.
final class PtyInfraException extends PtyException {
  const PtyInfraException(
    super.message, {
    super.operation = 'controller',
    super.nativeCode,
    super.context,
  }) : super(category: .infrastructure);

  @override
  String get _name => 'PtyInfraException';
}
