# ptyx

`ptyx` starts local processes attached to a native pseudo-terminal and exposes
its terminal input and output as bytes. It is intended for terminal
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
terminal output. Chunk boundaries have no semantic meaning. On Windows, ConPTY
produces UTF-8 text and virtual-terminal presentation updates rather than a
history of the child's write calls; `ptyx` preserves the bytes and order it
receives from that backend.

## Backpressure

Input and output memory are bounded per session. `write` accepts a complete
buffer in invocation order and copies it into native storage before returning.
Temporary saturation throws `PtyBackpressureException` without accepting any
bytes or failing the session:

```dart
session.write(Uint8List.fromList('status\n'.codeUnits));
```

For an interactive owner, listen to output before writing. Accepted buffers
may be reused or mutated immediately:

```dart
final outputDone = session.output.forEach(stdout.add);
await for (final bytes in stdin) {
  session.write(Uint8List.fromList(bytes));
}
await outputDone;
```

An output-heavy child can block normally when `output` has no listener or its
subscription is paused. Listen before awaiting `exitCode` when output matters.
If it does not matter, explicitly cancel a subscription:

```dart
final output = session.output.listen(null);
await output.cancel();
```

Cancellation drains and discards later native output so the child can
continue. It is the only public discard operation.

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
- Windows requires build 26100 or newer so pseudoconsole shutdown is
  nonblocking and all per-session resources can be reclaimed.
- ConPTY resize uses cell dimensions. Pixel dimensions remain cached metadata.

The implementation targets Linux, macOS, and Windows on x64 and arm64.
Production qualification is granted per target only after native end-to-end
CI on that exact OS and architecture; compilation alone is not qualification.
See the maintained [platform matrix](doc/platforms.md) for current evidence and
gaps. Android is not advertised until device or representative-emulator
qualification is retained.

## Errors

Operational and validated-input errors derive from `PtyException`:

- `PtyArgumentException` means the native boundary rejected a value. Pure Dart
  validation uses `ArgumentError`.
- `PtyClosedException` means the operation requires a live session.
- `PtyUnsupportedException` means the native capability does not exist.
- `PtyBackpressureException` means one write was rejected without failing the
  session because bounded native input storage was full.
- `PtyInputException` reports an unrecoverable terminal input failure.
- `PtyInfraException` reports controller or Unix broker loss.

Output read failures are delivered on `output`. Permanent input failures become
sticky for later writes, are delivered after safely buffered output, and are
retained by `close`. Spawn, resize, metadata, exit observation, and close
otherwise report through their own operation.

## Security

Children run with the same operating-system privileges as the Dart process.
`ptyx` is not a sandbox. No shell is inserted, but callers must still treat
the selected executable, arguments, environment, and child output according
to their own trust boundary. Input bytes and environment values are not
included in package diagnostics. See the full [security model](doc/security.md).

See the [lifecycle model](doc/design/lifecycle.md), the
[C ABI contract](doc/design/c-abi.md), the
[capability matrix](doc/capability-matrix.md), the
[selected architecture](doc/architecture/selected-architecture.md), and
[BENCHMARKS.md](BENCHMARKS.md) for the benchmark protocol. The current
scorecard is diagnostic and does not yet satisfy every production acceptance
workload listed there.
