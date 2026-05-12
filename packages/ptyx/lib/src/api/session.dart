part of 'api.dart';

/// A child process connected to a pseudo terminal.
///
/// A pseudo terminal gives the child one terminal device. Output arrives as raw
/// bytes through [output], and input is written with [write]. Terminal programs
/// can change input modes while they run; [mode] and [modeChanges] expose the
/// most recent mode observed by the session.
///
/// The session owns native resources. Close it when it is no longer needed,
/// even after [exitCode] completes.
///
/// Example:
///
/// ```dart
/// final session = PtySession.spawn(
///   const PtySpawnOptions(
///     executable: '/bin/sh',
///     arguments: ['-c', 'printf hello'],
///     initialSize: PtySize(rows: 24, columns: 80),
///   ),
/// );
///
/// final chunks = <int>[];
/// final output = session.output.listen(chunks.addAll);
///
/// final exitCode = await session.exitCode;
/// await output.cancel();
/// await session.close();
/// ```
abstract interface class PtySession {
  /// Starts a child process attached to a new pseudo terminal.
  ///
  /// Throws a [PtyException] if the process cannot be started.
  factory PtySession.spawn(PtySpawnOptions options) {
    return NativePtySession.spawn(options);
  }

  /// Completes with the child process exit code when the child exits.
  ///
  /// The result is cached. Awaiting this future more than once returns the same
  /// value. A successful exit is usually `0`; signal exits and native process
  /// failures use platform-specific values.
  ///
  /// This future can complete before [output] delivers every buffered byte.
  /// Wait for [output] to close when trailing output matters.
  ///
  /// If the session is closed before the child exit can be observed, or if the
  /// exit status cannot be observed, this future completes with a
  /// [PtyException].
  Future<int> get exitCode;

  /// The most recently observed terminal input mode.
  ///
  /// Returns `null` when terminal modes are not available on the current
  /// platform. Throws [PtyClosedException] after [close].
  PtyTermMode? get mode;

  /// Terminal input mode changes observed from the pseudo terminal.
  ///
  /// The stream emits when a program changes terminal input behavior, such as
  /// disabling echo for hidden input. It closes with the session and may emit a
  /// [PtyException] if mode polling fails.
  Stream<PtyTermMode> get modeChanges;

  /// Raw bytes produced by the child process through the pseudo terminal.
  ///
  /// A pseudo terminal has one output byte stream rather than separate standard
  /// output and standard error streams. This stream closes when the terminal
  /// reaches EOF or the session closes.
  ///
  /// The stream is single-subscription. Output-heavy children may block while
  /// this stream has no listener or while its subscription is paused. Cancel
  /// the subscription to discard unread output and allow the child to continue.
  Stream<Uint8List> get output;

  /// The child process identifier.
  ///
  /// Returns `null` when no stable process identifier is available.
  /// Throws [PtyClosedException] after [close].
  int? get pid;

  /// The current pseudo terminal size.
  ///
  /// Throws [PtyClosedException] after [close].
  PtySize get size;

  /// The pseudo terminal device name.
  ///
  /// Returns `null` when no terminal device name is available.
  /// Throws [PtyClosedException] after [close].
  String? get ttyName;

  /// Closes the session and releases native resources.
  ///
  /// If the child is still running, the session asks it to terminate before
  /// releasing native handles. Calling [close] more than once is allowed.
  ///
  /// After this future completes, operations that require a live session throw
  /// [PtyClosedException]. The [output] and [modeChanges] streams are closed as
  /// part of closing the session.
  Future<void> close();

  /// Sends [signal] to the child process.
  ///
  /// On platforms with signal support, [signal] is delivered to the child. On
  /// platforms without signals, the child is terminated in the supported native
  /// way and the concrete signal may be ignored.
  ///
  /// Returns `true` when a live child was signaled or terminated. Returns
  /// `false` when there is no live child, including after [exitCode] completes
  /// or after [close].
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]);

  /// Changes the pseudo terminal size.
  ///
  /// Terminal programs commonly observe this as a window-size change.
  /// Throws [PtyClosedException] after [close].
  void resize(PtySize size);

  /// Writes raw bytes to the child process input.
  ///
  /// Bytes are sent exactly as provided. Text callers choose the encoding and
  /// line endings expected by the child. Throws [PtyClosedException] after
  /// [close].
  void write(Uint8List data);
}
