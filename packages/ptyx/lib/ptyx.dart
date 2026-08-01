/// Pseudo-terminal sessions for interactive local processes.
///
/// Use [PtySession.spawn] to start a child process connected to a pseudo
/// terminal. A session exposes raw terminal output, byte-oriented input, and
/// process lifecycle controls.
///
/// Windows requires build 26100 or newer. Earlier ConPTY implementations
/// cannot satisfy the package's resource-cleanup contract.
library;

export 'src/api/api.dart'
    show
        PtyBackpressureException,
        PtyCapabilities,
        PtyCloseException,
        PtyClosedException,
        PtyEnvironmentMode,
        PtyErrorCategory,
        PtyException,
        PtyExitException,
        PtyExitStatus,
        PtyExited,
        PtyInfrastructureException,
        PtyInputException,
        PtyInvalidArgumentException,
        PtyMetadataException,
        PtyModeException,
        PtyOutputException,
        PtyResizeException,
        PtySignalException,
        PtySignaled,
        PtySize,
        PtySpawnException,
        PtySpawnOptions,
        PtyTermMode,
        PtyUnsupportedException;

export 'src/native/native.dart' show PtySession;
