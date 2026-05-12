/// Pseudo-terminal sessions for interactive local processes.
///
/// Use [PtySession.spawn] to start a child process connected to a pseudo
/// terminal. A session exposes raw terminal output, byte-oriented input, and
/// process lifecycle controls.
library;

export 'src/api/api.dart';
