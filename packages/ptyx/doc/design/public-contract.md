# Public contract

`ptyx` owns one local child process attached to one native pseudo-terminal and
the terminal job associated with it. The transport boundary is raw bytes.
Chunk boundaries have no semantic meaning.

## Session creation

Spawning is asynchronous:

```dart
final session = await PtySession.spawn(options);
```

The future completes only after options are validated, the child is attached
to the terminal, native ownership is registered, Dart ports are installed,
and input and output can make progress. A spawn failure completes only after
every resource acquired by the attempt has been reclaimed. No partially
constructed public session escapes.

Executable and argument values are passed directly to the platform process
facility. ptyx does not invoke a shell, parse commands, select an encoding, or
rewrite line endings.

## Session and sub-resource states

The public session has `open`, `closing`, and `closed` states. Spawn is not a
public session state because the future does not expose a session until spawn
commits.

The child, input, output, exit observation, mode observation, and native
resources progress independently:

- child: `running`, `exited`, or `exitObservationFailed`;
- input: `open`, `failed`, or `closed`;
- output: `awaitingListener`, `flowing`, `paused`, `discarding`, `ended`, or
  `failed`;
- mode observation: `inactive`, `observing`, `failed`, or `ended`;
- native ownership: `live`, `stopping`, or `released`.

Child exit does not imply output EOF. Output EOF does not imply that the direct
child status has been observed. Input may fail while readable trailing output
remains.

## Input

The session exposes one bounded input operation:

```dart
session.write(bytes);
```

`write` synchronously accepts the complete byte list into owned native storage
or rejects it without accepting any bytes. A byte list larger than the
configured maximum is invalid. Temporary queue exhaustion throws
`PtyBackpressureException` and leaves the session usable. The operation never
waits for the child, native writer, descriptor readiness, or queue capacity.

The acceptance linearization point is the successful queue reservation under
the native input lock. Returning does not mean that the operating system, slave
line discipline, or child consumed the bytes. Callers establish application
progress from terminal output or their application protocol.

Input queue capacity includes every accepted byte not yet passed to the PTY.
It is bounded by configuration and cannot be expanded by a large write.

Interrupted and temporarily unavailable native writes are retried internally.
An unrecoverable endpoint failure makes later writes throw the same sticky
`PtyInputException`, while output, exit observation, signaling, metadata, and
cleanup remain available. Safely buffered output is delivered before the input
failure becomes the terminal output event. `close` completes cleanup before
reporting a retained input failure. Infrastructure or ownership loss remains a
whole-session failure.

## Output

`output` is a single-subscription `Stream<Uint8List>`. Bytes are delivered
exactly once and in PTY read order.

- Before listen, native and Dart output remain within the configured combined
  budget and then stop reading the PTY.
- Pause retains bounded data and then stops reading the PTY.
- Resume continues delivery without loss or reordering.
- Cancel commits an explicit transition to native drain-and-discard.
- Normal child exit preserves trailing output until PTY EOF.
- Stream completion means no later output can be delivered.
- A read failure is emitted after every byte that can still be delivered
  safely, then the stream closes.

Native output credit is released only after a copied Dart event is delivered
to the subscription or explicitly discarded. Data in a Dart controller or its
bounded in-flight native-port messages remains charged to the same session
budget. Messages that arrive after a subscription pauses are retained in a
FIFO charged to that budget, then delivered in order after resume.

Cancellation is the only public discard operation. A caller that does not need
output attaches and cancels a subscription:

```dart
final output = session.output.listen(null);
await output.cancel();
```

This keeps discard ownership in the stream API instead of duplicating it on
the session.

Awaiting child exit without consuming or discarding output can legitimately
block the child on PTY backpressure.

## Exit

`exitStatus` completes once with a typed direct-child status or with
`PtyExitException` if status observation fails. `exitCode` is the compatible
numeric view. Unix signal termination and a normal numeric exit are distinct
status values. Windows exposes the full unsigned 32-bit native process exit
code as a Dart integer without fabricating a Unix signal.

Exit can complete before trailing output. Callers that require complete output
await stream completion as well.

## Signals and terminal job ownership

`capabilities.signals` states whether Unix signal semantics are available.
Windows does not advertise that capability; `kill` instead terminates the
owned ConPTY job using Windows process semantics.

`kill` returns `true` when a live child or job accepted termination and
`false` when the direct child has already exited. A native delivery failure
throws `PtySignalException`; it is not converted to `false`. Unix signals
target the owned process group. Forced cleanup uses the broker's retained
job identity and never signals a PID cached by Dart.

The reported exit status remains the direct child's status even when cleanup
owns a larger process group or Windows job.

## Resize, metadata, and modes

Resize validates cell and pixel ranges before entering native code. Success
means the native PTY accepted the new size. Unix sends the platform terminal
window-change notification. A closed session, unavailable capability, and
failed native resize remain distinct results.

Capabilities state whether Unix signals, Unix process groups, terminal modes,
and a terminal name are available. The backend choice is intentionally not
part of the public capability object. `pid` is a direct-child identifier when
the platform returns one. A missing terminal name or mode is interpreted with
its corresponding capability, so it is not ambiguous.

An on-demand mode query is a snapshot. Nullable fields mean that the platform
did not report that field. Mode changes are a broadcast observation stream
that starts native observation with the first listener and stops it with the
last. It reports only distinct states observed by ptyx and does not promise
lossless detection of arbitrarily short transitions.

## Close

Close is asynchronous and safe to call concurrently:

```dart
await session.close();
```

The first call atomically commits `open` to `closing`. Every caller receives
the same completion future. Close rejects new operations, settles already
accepted input, performs graceful terminal-job termination for the configured
deadline on Unix, escalates to forced termination when needed, stops I/O, reaps
the direct child exactly once, and releases ports, messages, buffers,
descriptors, handles, workers, and job ownership. ConPTY has no equivalent
portable graceful request, so Windows begins Job Object termination
immediately.

Close is an explicit shutdown request, so output not already delivered may be
drained and discarded during its cleanup phase. Normal child exit without
close retains trailing output.

Cleanup proceeds after individual failures. If cleanup itself cannot be
established, the shared future completes with `PtyCloseException` after every
safe action. If cleanup uncertainty is a consequence of an earlier typed
infrastructure failure, close preserves that root failure instead of replacing
it with a less specific cleanup exception.

## Isolate ownership

A session belongs to the isolate that spawned it and is not transferable.
Native state owns cleanup independently of the Dart wrapper. Dart
native finalization and failed operational posts commit the same idempotent
native shutdown path. The pinned native-finalizer callback is guaranteed at
normal isolate-group shutdown, performs no Dart API calls, and serializes route
removal with every native-to-Dart post. A separate supervisor receives staged
handles before publication and requests the same abandonment path when the
individual owner isolate exits. Late messages carry generation-checked
identities and cannot access a released session.

## Errors

Every operational exception includes:

- the public operation or infrastructure subsystem that first reported the
  failure;
- a stable error category;
- a safe message;
- the native OS code when the native boundary retained it;
- non-secret context needed to act on the failure.

The public error families are spawn, input, output, exit observation, signal,
resize, metadata, mode observation, state, unsupported capability, and close.
Delayed input failures are retained for later writes and close and become the
terminal output event after safely buffered bytes. Output read failures never
complete exit or input state. Cleanup failures never replace a previously
observed child status.

Children inherit the parent's operating-system privilege. ptyx is not a
sandbox.
