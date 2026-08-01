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
  /// The result is cached and may complete before buffered [output] finishes.
  /// If the status cannot be observed, the future completes with a
  /// [PtyException].
  Future<int> get exitCode;

  /// Completes with a typed direct-child termination status.
  ///
  /// Unix signal termination produces [PtySignaled]. Normal exits and Windows
  /// native exit codes produce [PtyExited].
  Future<PtyExitStatus> get exitStatus;

  /// Platform-specific features available for this session. Nullable values
  /// and unsupported operations are described by the corresponding flags.
  PtyCapabilities get capabilities;

  /// The most recently observed terminal input mode.
  ///
  /// Returns `null` when terminal modes are unavailable. Throws a typed
  /// [PtyException] when a supported native query fails.
  PtyTermMode? get mode;

  /// Terminal input mode changes observed from the pseudo terminal.
  ///
  /// On supported platforms, listening emits the current mode and subsequent
  /// changes. It remains silent when terminal modes are unavailable.
  Stream<PtyTermMode> get modeChanges;

  /// Raw bytes received from the platform pseudo-terminal backend.
  ///
  /// The stream is single-subscription and applies native backpressure while
  /// paused or unwatched. Cancel it when output should be discarded.
  Stream<Uint8List> get output;

  /// The child process identifier, when the platform provides one. Throws a
  /// typed [PtyException] when the native query fails.
  int? get pid;

  /// The current pseudo terminal size. Throws a typed [PtyException] when the
  /// native query fails.
  PtySize get size;

  /// The pseudo terminal device name, when the platform provides one. Throws a
  /// typed [PtyException] when the native query fails.
  String? get ttyName;

  /// Closes the session and releases native resources.
  ///
  /// Closing is idempotent. If the child is still running, the native backend
  /// terminates it and waits for bounded cleanup. The output and mode streams
  /// close as part of the operation.
  Future<void> close();

  /// Sends [signal] to the child process.
  ///
  /// Returns `false` when no live child remains. Throws [PtySignalException]
  /// when native delivery fails.
  bool kill([ProcessSignal signal = .sigterm]);

  /// Changes the pseudo terminal size.
  ///
  /// Throws [PtyResizeException] for a native resize failure or
  /// [PtyClosedException] after [close].
  void resize(PtySize size);

  /// Accepts all of [data] into bounded native storage in invocation order.
  ///
  /// Returning means the bytes were copied and accepted, not consumed by the
  /// child. The caller may mutate [data] immediately afterward. A rejected
  /// write never partially enters the native input queue.
  void write(Uint8List data);
}
