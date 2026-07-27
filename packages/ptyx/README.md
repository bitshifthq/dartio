# ptyx

`ptyx` starts local processes attached to a native pseudo-terminal and exposes
their terminal input and output as exact bytes. It is intended for terminal
applications, IDEs, TTY-aware automation, and services that own many concurrent
terminal sessions.

The package does not emulate a terminal, decode text, parse shell commands, or
provide SSH. Arguments are passed directly to the executable.

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

  final outputDone = session.output
      .map((chunk) => String.fromCharCodes(chunk))
      .forEach(stdout.write);

  final exitCode = await session.exitCode;
  await outputDone;
  await session.close();
  stdout.writeln('exit: $exitCode');
}
```

Output is a single-subscription byte stream containing the child's combined
terminal output. Chunk boundaries have no semantic meaning.

## Backpressure

Input and output memory are bounded per session. `tryWrite` accepts a complete
buffer or rejects it without accepting any bytes. `write` waits for capacity,
and `flush` completes after all earlier accepted bytes have reached the PTY
master:

```dart
await session.write(Uint8List.fromList('status\n'.codeUnits));
await session.flush();
```

An output-heavy child can block normally when `output` has no listener or its
subscription is paused. Listen before awaiting `exitCode` when output matters.
If it does not matter, call `discardOutput()`; canceling an output subscription
has the same drain-and-discard effect.

`inputDone` completes normally after deliberate shutdown and with a
`PtyInputException` if already accepted input cannot be written.

## Lifecycle

`spawn` is asynchronous and returns only after Dart can route every native
event for the session. `exitCode` reports the direct child's status, but may
complete before trailing output is delivered. Await the output stream when
trailing bytes matter.

Always call `close`, including after normal process exit. Close is idempotent,
first requests graceful termination, then forces cleanup after
`gracefulCloseTimeout`. Operations requiring live native state throw
`PtyClosedException` after close.

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

The supported production matrix is Linux, macOS, and Windows on x64 and arm64.
Platform support is qualified by native end-to-end CI, not by cross-compilation
alone. Android is not advertised until device or representative-emulator
qualification is retained.

## Errors

All package errors derive from `PtyException`:

- `PtyClosedException` means the operation requires a live session.
- `PtyUnsupportedException` means the native capability does not exist.
- `PtyInputException` reports terminal input failure or an impossible capacity
  request.
- `PtyInfrastructureException` reports controller or Unix broker loss.

Output read failures are delivered on `output`; input failures use `inputDone`;
spawn, resize, metadata, exit observation, and close report through their own
operation.

See the [lifecycle model](doc/design/lifecycle.md), the
[selected architecture](doc/architecture/selected-architecture.md), and
[BENCHMARKS.md](BENCHMARKS.md) for the reproducible scorecard.
