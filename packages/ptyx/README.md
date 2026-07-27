# ptyx

`ptyx` starts local processes attached to a native pseudo-terminal and exposes
their terminal input and output as exact bytes. It is intended for terminal
applications, IDEs, TTY-aware automation, and services that own many concurrent
terminal sessions.

The package does not emulate a terminal, decode text, parse shell commands, or
provide SSH. Arguments are passed directly to the executable.

## Installation

```sh
dart pub add ptyx
```

Version `0.0.1` has no prebuilt artifacts and always builds from source. It
requires Dart 3.11 or newer, a stable Rust toolchain, and a C11 compiler. See
[building from source](doc/building.md) for target-specific requirements.

## Quick start

```dart
import 'dart:io';
import 'dart:typed_data';

import 'package:ptyx/ptyx.dart';

Future<void> main() async {
  final session = await PtySession.spawn(
    PtySpawnOptions(
      executable: Platform.isWindows ? 'cmd.exe' : '/bin/sh',
      arguments: Platform.isWindows
          ? const ['/d', '/s', '/c', 'echo hello']
          : const ['-c', 'printf "hello\\n"'],
      initialSize: const PtySize(rows: 24, columns: 80),
    ),
  );

  try {
    final outputDone = session.output.forEach(stdout.add);
    final exitCode = await session.exitCode;
    await outputDone;
    stdout.writeln('exit: $exitCode');
  } finally {
    await session.close();
  }
}
```

Output is a single-subscription byte stream containing the child's combined
terminal output. Chunk boundaries have no semantic meaning.

## Backpressure

Input and output memory are bounded per session. `write` accepts a complete
buffer in invocation order and waits asynchronously when input capacity is
temporarily exhausted. It throws `PtyInputException` after permanent input
failure. `flush` completes after all earlier accepted bytes have reached the
PTY master:

```dart
await session.write(Uint8List.fromList('status\n'.codeUnits));
await session.flush();
```

For an interactive owner, listen to output before writing and keep bytes
unmodified:

```dart
final outputDone = session.output.forEach(stdout.add);
await for (final bytes in stdin) {
  await session.write(Uint8List.fromList(bytes));
  await session.flush();
}
await outputDone;
```

An output-heavy child can block normally when `output` has no listener or its
subscription is paused. Listen before awaiting `exitCode` when output matters.
If it does not matter, call `discardOutput()`; canceling an output subscription
has the same drain-and-discard effect.

`inputDone` completes normally after deliberate shutdown and with a
`PtyInputException` if already accepted input cannot be written.

Resize is synchronous bounded metadata work:

```dart
session.resize(const PtySize(rows: 40, columns: 120));
```

## Lifecycle

`spawn` is asynchronous and returns only after Dart can route every native
event for the session. `exitCode` reports the direct child's status, but may
complete before trailing output is delivered. Await the output stream when
trailing bytes matter.

Always call `close`, including after normal process exit. Close is idempotent,
first requests graceful termination on Unix, then forces cleanup after
`gracefulCloseTimeout`. ConPTY has no equivalent portable graceful request, so
Windows begins Job Object termination immediately. Operations requiring live
native state throw `PtyClosedException` after close.

The session owns the terminal job. On Unix this is the controlling-terminal
process group; on Windows this is a kill-on-close job associated with ConPTY.
A same-privilege Unix descendant can deliberately escape its process group and
is then outside the portable cleanup guarantee.

## Environment and executable lookup

`PtyEnvironmentMode` chooses whether the child inherits, overlays, replaces, or
clears the parent environment. Environment keys and values must not contain
NUL. Use an absolute executable path when lookup must be deterministic.

`arguments` excludes the executable itself. No shell is inserted. If shell
syntax is desired, invoke the shell explicitly as in the quick-start example.

## Platform capabilities

Inspect `session.capabilities` instead of inferring behavior from the operating
system:

- Linux and macOS provide Unix process-group signals, terminal modes, and a TTY
  device name.
- Windows uses ConPTY. Unix signal and terminal-mode semantics are unavailable;
  `kill` terminates the owned job using Windows process semantics.
- ConPTY resize uses cell dimensions. Pixel dimensions remain cached metadata.

The implementation targets Linux, macOS, and Windows on x64 and arm64.
Production qualification is granted per target only after native end-to-end
CI on that exact OS and architecture; compilation alone is not qualification.
See the maintained [platform matrix](doc/platforms.md) for current evidence and
gaps. Android is not advertised until device or representative-emulator
qualification is retained.

## Errors

Operational and validated-input errors derive from `PtyException`:

- `PtyInvalidArgumentException` means a value is outside the native contract.
- `PtyClosedException` means the operation requires a live session.
- `PtyUnsupportedException` means the native capability does not exist.
- `PtyInputException` reports terminal input failure or an impossible capacity
  transition after bytes were accepted.
- `PtyInfrastructureException` reports controller or Unix broker loss.

Output read failures are delivered on `output`; input failures use `inputDone`;
spawn, resize, metadata, exit observation, and close report through their own
operation.

## Security

Children run with the same operating-system privileges as the Dart process.
`ptyx` is not a sandbox. No shell is inserted, but callers must still treat
the selected executable, arguments, environment, and child output according
to their own trust boundary. Input bytes and environment values are not
included in package diagnostics. See the full [security model](doc/security.md).

See the [lifecycle model](doc/design/lifecycle.md), the
[capability matrix](doc/capability-matrix.md), the
[selected architecture](doc/architecture/selected-architecture.md), and
[BENCHMARKS.md](BENCHMARKS.md) for the benchmark protocol. The current
scorecard is diagnostic and does not yet satisfy every production acceptance
workload listed there.
