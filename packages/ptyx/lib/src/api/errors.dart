part of 'api.dart';

/// Stable error categories exposed by ptyx operations.
enum PtyErrorCategory {
  /// Invalid caller input.
  invalidArgument,

  /// An operation requires a live session.
  closed,

  /// The platform does not provide the requested capability.
  unsupported,

  /// Native process creation failed.
  spawn,

  /// Accepted terminal input failed.
  input,

  /// Terminal output failed.
  output,

  /// Direct-child exit observation failed.
  exit,

  /// Native signal or termination delivery failed.
  signal,

  /// Native terminal resize failed.
  resize,

  /// Native metadata access failed.
  metadata,

  /// Terminal-mode observation failed.
  mode,

  /// Session cleanup could not be established.
  cleanup,

  /// The native controller, reactor, or broker failed.
  infrastructure,

  /// An uncategorized package failure.
  unknown,
}

/// Base exception thrown by ptyx operations.
class PtyException implements Exception {
  /// Human-readable failure detail.
  final String message;

  /// The public operation or infrastructure subsystem that reported failure.
  final String operation;

  /// Stable machine-readable error category.
  final PtyErrorCategory category;

  /// Native status or operating-system error code when retained by the
  /// native boundary.
  final int? nativeCode;

  /// Non-secret context useful for diagnosing the failed operation.
  final String? context;

  const PtyException(
    this.message, {
    this.operation = 'unknown',
    this.category = PtyErrorCategory.unknown,
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

/// Thrown when a runtime-validated ptyx argument is invalid.
class PtyInvalidArgumentException extends PtyException {
  const PtyInvalidArgumentException(
    super.message, {
    super.operation = 'validation',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.invalidArgument);

  @override
  String get _name => 'PtyInvalidArgumentException';
}

/// Thrown when an operation requires an open session.
///
/// Closing a session is idempotent. Operations such as [PtySession.write],
/// [PtySession.resize], and metadata getters require native handles released by
/// [PtySession.close].
class PtyClosedException extends PtyException {
  const PtyClosedException(
    super.message, {
    super.operation = 'state',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.closed);

  @override
  String get _name => 'PtyClosedException';
}

/// Thrown when the current platform does not support an operation.
///
/// Optional platform capabilities may also be represented by nullable values,
/// such as [PtySession.pid], [PtySession.ttyName], and [PtySession.mode].
class PtyUnsupportedException extends PtyException {
  const PtyUnsupportedException(
    super.message, {
    super.operation = 'capability',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.unsupported);

  @override
  String get _name => 'PtyUnsupportedException';
}

/// Thrown when accepted input fails or input capacity can never be provided.
class PtyInputException extends PtyException {
  const PtyInputException(
    super.message, {
    super.operation = 'input',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.input);

  @override
  String get _name => 'PtyInputException';
}

/// Thrown when the native controller or Unix broker becomes unavailable.
class PtyInfrastructureException extends PtyException {
  const PtyInfrastructureException(
    super.message, {
    super.operation = 'controller',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.infrastructure);

  @override
  String get _name => 'PtyInfrastructureException';
}

/// Thrown when native process creation fails.
class PtySpawnException extends PtyException {
  const PtySpawnException(
    super.message, {
    super.operation = 'spawn',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.spawn);

  @override
  String get _name => 'PtySpawnException';
}

/// Thrown on a terminal output read failure.
class PtyOutputException extends PtyException {
  const PtyOutputException(
    super.message, {
    super.operation = 'output',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.output);

  @override
  String get _name => 'PtyOutputException';
}

/// Thrown when direct-child status cannot be observed.
class PtyExitException extends PtyException {
  const PtyExitException(
    super.message, {
    super.operation = 'exit',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.exit);

  @override
  String get _name => 'PtyExitException';
}

/// Thrown when native signal or termination delivery fails.
class PtySignalException extends PtyException {
  const PtySignalException(
    super.message, {
    super.operation = 'signal',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.signal);

  @override
  String get _name => 'PtySignalException';
}

/// Thrown when native terminal resize fails.
class PtyResizeException extends PtyException {
  const PtyResizeException(
    super.message, {
    super.operation = 'resize',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.resize);

  @override
  String get _name => 'PtyResizeException';
}

/// Thrown when native terminal metadata cannot be read safely.
class PtyMetadataException extends PtyException {
  const PtyMetadataException(
    super.message, {
    super.operation = 'metadata',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.metadata);

  @override
  String get _name => 'PtyMetadataException';
}

/// Thrown when terminal-mode observation fails.
class PtyModeException extends PtyException {
  const PtyModeException(
    super.message, {
    super.operation = 'mode',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.mode);

  @override
  String get _name => 'PtyModeException';
}

/// Thrown when bounded native cleanup cannot be established.
class PtyCloseException extends PtyException {
  const PtyCloseException(
    super.message, {
    super.operation = 'close',
    super.nativeCode,
    super.context,
  }) : super(category: PtyErrorCategory.cleanup);

  @override
  String get _name => 'PtyCloseException';
}
