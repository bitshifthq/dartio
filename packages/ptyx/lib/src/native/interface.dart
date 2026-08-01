part of 'native.dart';

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
/// final session = await PtySession.spawn(
///   const PtySpawnOptions(
///     executable: '/bin/sh',
///     arguments: ['-c', 'printf hello'],
///     initialSize: PtySize(rows: 24, columns: 80),
///   ),
/// );
///
/// try {
///   final outputDone = session.output.forEach(stdout.add);
///   final exitCode = await session.exitCode;
///   await outputDone;
///   stdout.writeln('exit: $exitCode');
/// } finally {
///   await session.close();
/// }
/// ```
abstract interface class PtySession {
  /// Starts a child process attached to a new pseudo terminal.
  ///
  /// Windows requires build 26100 or newer. Earlier builds report
  /// [PtyUnsupportedException] because their ConPTY shutdown behavior cannot
  /// satisfy the session cleanup contract.
  ///
  /// Throws [PtyInvalidArgumentException] for options outside the native
  /// contract, [PtyUnsupportedException] when the platform backend is
  /// unavailable, [PtySpawnException] when native process creation fails, or
  /// [PtyInfrastructureException] when controller setup cannot be completed.
  static Future<PtySession> spawn(PtySpawnOptions options) =>
      _NativeSession.spawn(options);

  /// Completes with the child process exit code when the child exits.
  ///
  /// The result is cached. Awaiting this future more than once returns the same
  /// value. A successful exit is usually `0`; signal exits and native process
  /// termination uses a platform-specific numeric representation.
  ///
  /// This future can complete before [output] delivers every buffered byte.
  /// Wait for [output] to close when trailing output matters.
  ///
  /// If the session is closed before the child exit can be observed, or if the
  /// exit status cannot be observed, this future completes with a
  /// [PtyException].
  Future<int> get exitCode;

  /// Completes with a typed direct-child termination status.
  ///
  /// Unix signal termination produces [PtySignaled]. Normal Unix exits and
  /// every Windows native exit code produce [PtyExited]. Like [exitCode], this
  /// can complete before trailing [output].
  Future<PtyExitStatus> get exitStatus;

  /// Platform-specific features available for this session.
  PtyCapabilities get capabilities;

  /// The most recently observed terminal input mode.
  ///
  /// Returns `null` when terminal modes are not available on the current
  /// platform. Check [capabilities] to distinguish that case. Throws
  /// [PtyModeException] when the native snapshot fails and
  /// [PtyClosedException] after [close].
  PtyTermMode? get mode;

  /// Terminal input mode changes observed from the pseudo terminal.
  ///
  /// On supported platforms, listening first emits the current mode and then
  /// emits when a program changes terminal input behavior, such as disabling
  /// echo for hidden input. On platforms without terminal-mode support it
  /// remains silent. It closes with the session and may emit a [PtyException]
  /// if supported mode polling fails.
  Stream<PtyTermMode> get modeChanges;

  /// Raw bytes received from the platform pseudo-terminal backend.
  ///
  /// A pseudo terminal has one output byte stream rather than separate standard
  /// output and standard error streams. This stream closes when the terminal
  /// reaches EOF or the session closes.
  ///
  /// On Windows, ConPTY emits UTF-8 text and virtual-terminal presentation
  /// updates. Those bytes need not reproduce the attached application's
  /// original write-call boundaries or intermediate screen states.
  ///
  /// The stream is single-subscription. Output-heavy children may block while
  /// this stream has no listener or while its subscription is paused. Cancel
  /// the subscription to discard unread and future output and allow the child
  /// to continue. A caller that never needs output can attach a listener with
  /// no data callback and immediately cancel its subscription:
  ///
  /// ```dart
  /// final output = session.output.listen(null);
  /// await output.cancel();
  /// ```
  Stream<Uint8List> get output;

  /// The child process identifier.
  ///
  /// Returns `null` when no stable process identifier is available.
  /// Throws [PtyMetadataException] when the native query fails and
  /// [PtyClosedException] after [close].
  int? get pid;

  /// The current pseudo terminal size.
  ///
  /// Throws [PtyMetadataException] when the native query fails and
  /// [PtyClosedException] after [close].
  PtySize get size;

  /// The pseudo terminal device name.
  ///
  /// Returns `null` when no terminal device name is available. Check
  /// [capabilities] to distinguish an unsupported terminal name. Throws
  /// [PtyMetadataException] when the native query fails and
  /// [PtyClosedException] after [close].
  String? get ttyName;

  /// Closes the session and releases native resources.
  ///
  /// If the child is still running, the session asks it to terminate before
  /// releasing native handles. Calling [close] more than once is allowed.
  ///
  /// After this future completes, operations that require a live session throw
  /// [PtyClosedException]. The [output] and [modeChanges] streams are closed as
  /// part of closing the session. Throws [PtyCloseException] if bounded native
  /// cleanup cannot be established and [PtyInputException] if accepted input
  /// cannot be delivered during cleanup. If an earlier
  /// [PtyInfrastructureException] caused cleanup uncertainty, that root error
  /// is preserved.
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
  ///
  /// Throws [PtySignalException] if native delivery fails.
  bool kill([ProcessSignal signal = .sigterm]);

  /// Changes the pseudo terminal size.
  ///
  /// Terminal programs commonly observe this as a window-size change.
  /// Throws [PtyInvalidArgumentException] for invalid dimensions,
  /// [PtyResizeException] when the native resize fails, and
  /// [PtyClosedException] after [close].
  void resize(PtySize size);

  /// Accepts all of [data] into bounded native storage in invocation order.
  ///
  /// Returning means the complete buffer was copied and accepted, not that the
  /// child consumed it. The caller may mutate [data] immediately afterward.
  ///
  /// One invocation accepts at most 1 MiB; larger application payloads should
  /// be divided into ordered chunks. Throws [PtyInvalidArgumentException] when
  /// [data] is empty, exceeds 1 MiB, or is larger than the configured input
  /// bound, [PtyBackpressureException] when the complete buffer cannot be
  /// accepted without waiting, [PtyInputException] after terminal input
  /// failure, and [PtyClosedException] after [close].
  ///
  /// ```dart
  /// session.write(Uint8List.fromList('status\n'.codeUnits));
  /// ```
  void write(Uint8List data);
}
