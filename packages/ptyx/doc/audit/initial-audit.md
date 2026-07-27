# Initial ptyx audit

This audit covers the base revision
`4c6120c38dec4ac36a7fb349c274ae3123fe4224`. It traces the public Dart API,
generated bindings, C ABI, Rust implementation, build hooks, tests, workflows,
and publication inputs. Findings are ordered by severity. The quality
requirements referenced below are in `QUALITY_STANDARD.md` and
`EXECUTION_SPEC.md`.

## Call and ownership trace

`PtySession.spawn` synchronously creates two Dart receive ports, initializes
the Dart DL API, converts Dart strings and options into arena-owned C layouts,
and calls `ptyx_spawn`. Rust copies those values, opens a portable-pty pair,
spawns the child, starts a child waiter, and then starts reader, exit-event,
and mode threads. A writer thread starts on the first non-empty write.

The returned Dart object stores the native allocation address as an integer.
Output is posted as copied or external typed data. The external-data peer owns
its Rust vector until the Dart finalizer drops it. Dart acknowledges output
through a second FFI call. Exit, error, and mode events use a separate native
port.

`close` sends termination signals from Dart, calls the synchronous
`ptyx_close`, closes both ports, and cancels subscriptions. Rust marks the
session closed, sends a force-kill request, stops and joins runtime threads,
and joins the child waiter. Errors in native close, kill, runtime shutdown, and
child-waiter join are discarded.

## Critical findings

### Concurrent close can double-free the native session

`NativeSession.close` tests `_closed` before its first await and sets `_closed`
only after up to two waits. Two concurrent calls can both pass the test and
both call `sessionFree`. `ptyx_close` reconstructs a `Box` from the same raw
pointer each time. This violates idempotent close, exact-once ownership, and
memory safety.

Required proof: a public race test that starts concurrent closes and an ABI
test that demonstrates stale handles cannot be freed or used.

### Raw ABI pointers permit stale-handle use, double free, and invalid dereference

`session_from_ptr`, `free_session`, `take_owned_buffer`, and
`free_owned_buffer` trust any non-null address and dereference or reconstruct a
`Box`. The ABI cannot distinguish a live handle from an arbitrary, freed, or
wrong-kind pointer. The same issue affects owned write buffers. This violates
the required stale-handle, invalid-input, use-after-free, and double-free
protection.

Required proof: generation-counted or registry-validated handles plus a
standalone C harness, fuzzing, and sanitizer coverage.

### Isolate or port loss leaks the session and terminal job

The Dart session has no native finalizer or isolate-loss protocol. A failed
output post stops the reader, but it does not close the writer, waiter, mode
observer, process, or native session allocation. If Dart loses the object or
isolate without calling `close`, no owner remains able to release the child
and native resources. This violates isolate ownership, port-loss handling,
failure containment, and deterministic cleanup.

Required proof: a native-owned session registry with port-loss shutdown,
isolate termination tests, failed-post fault injection, and leak checks.

### Accepted input can be silently discarded

Writes are reported as accepted when queued. Close and writer failure clear
the queued vectors without an operation-specific completion or flush failure.
There is no public capacity wait, flush, or input-failure channel. A write
racing with close can be accepted while `close` is awaiting exit, then removed
without proof it reached the PTY. This violates accepted-write ordering,
explicit backpressure, flush, and failure semantics.

Required proof: an input state machine, sequence-numbered flush barriers,
all-or-reject capacity accounting, and race tests.

## High-severity findings

### Spawn and cleanup block the Dart isolate

The public spawn factory calls the entire native spawn path synchronously.
Native close joins reader, writer, waiter, and mode threads through a
synchronous FFI call. Windows blocking reads have no demonstrated reliable
interruption before join. The Dart-side 50 ms termination waits do not bound
the subsequent native joins. This violates asynchronous spawn and close and
can wedge the owning isolate.

### Output acknowledgments do not prove Dart consumption

When a listener is active and not explicitly paused, Dart acknowledges a chunk
immediately after `StreamController.add`. The controller is asynchronous, so
the event may still be queued on the Dart side. A slow consumer that does not
manually pause can therefore release native backpressure before consuming the
bytes, leaving Dart buffering outside the documented bound. Native accounting
also allows an arbitrary acknowledgment count and uses saturating subtraction,
which can hide duplicate or excessive acknowledgments.

### Write failures are delivered on the output stream

`_handleNativeError` combines output and write error sources and adds both to
the output controller. This is the exact unrelated-channel defect named in the
execution specification. Wait and mode errors have separate channels, but
close errors are discarded and kill errors collapse to `false`.

### Process signaling and cleanup own only the direct child

Unix signaling calls `kill(pid, signal)`. It does not target the foreground
process group. Forced cleanup also targets only that PID, so descendants in
the terminal job can survive. PID reuse is reduced by consulting the cached
wait result, but the check and signal are not one atomic OS identity
operation. The implementation does not satisfy terminal-job ownership.

### Linux children inherit unrelated descriptors

The Linux monitor and exec child are created after the Dart process is
multithreaded. The exec child duplicates the slave PTY but never closes
unrelated inherited descriptors. The monitor also retains inherited
descriptors until exit. This can leak other sessions, ports, files, and
sockets into both monitor and child processes.

The non-Linux portable-pty path tries to enumerate `/dev/fd` from a
`pre_exec` callback. Directory iteration and allocation after fork are not an
acceptable async-signal-safe strategy in a multithreaded host.

### Per-session worker topology fails the scaling design

An idle session creates a child waiter, output reader, event waiter, and mode
poller. The writer adds a fifth thread after input. An exploratory run with
sufficient descriptors increased the process from 8 to 408 threads at 100
idle sessions. The retained normal-limit run failed while creating session 61
with `EMFILE`. Both outcomes contradict the required 100-session topology and
materially expose descriptor, memory, and scheduling cost.

### Output and input defaults permit excessive aggregate memory

Defaults allow 64 MiB of queued input, 64 MiB of outstanding external output,
4 MiB of unacknowledged output, plus native read and batch buffers per session.
At 100 sessions, the configured theoretical aggregate is several gigabytes
before Dart-side queues. Inflight reservation also checks only whether the
current count is below the maximum, then may add a full batch beyond it.

### Terminal mode polling is always active

Default native flags start a dedicated 50 ms mode polling thread for every
session. Dart uses a broadcast controller with no native activation on first
listen or deactivation on last cancel. This contradicts opt-in observation,
adaptive polling, and idle-CPU requirements.

### Windows runtime behavior is unqualified and partly unspecified

The only end-to-end session test file has `@TestOn('!windows')`, so the Windows
workflow runs no session behavior. portable-pty owns command-line quoting,
environment construction, ConPTY, pipe, process, and handle behavior without
a ptyx ownership audit or fault tests. Replace and clear environments do not
explicitly preserve Windows system entries. Blocking ConPTY pipes and
pseudoconsole shutdown are not modeled, including the documented requirement
to continue draining output during close.

### Android is advertised without runtime evidence

The pubspec advertises Android and CI cross-compiles armv7, arm64, and x64.
There are no device or emulator runtime tests. Compilation is not production
qualification, so the package claim is currently unsupported.

## Medium-severity findings

- ABI initialization checks only the major integer. It does not verify minor
  features, layout, calling convention, artifact target, or source identity.
- Error objects carry only text and three Dart exception classes. They omit a
  stable operation, complete category, native code, and safe context.
- Nullable PID, TTY name, and mode values stand in for missing capabilities.
  There is no explicit capability object.
- `kill` converts every native error into `false`, making an exited child
  indistinguishable from a failed signal attempt.
- `session::close` discards every cleanup error, so the ABI cannot report an
  incomplete cleanup result.
- Output cancellation still transfers every later byte through Dart before
  acknowledging it. It is explicit discard semantically, but not an efficient
  native drain policy.
- The mode stream polls fixed intervals and its documentation does not state
  that transitions can be missed.
- Size values are not validated at Dart construction. Native conversion
  rejects zero and values above `u16`, but the public type accepts them.
- Generated bindings are post-edited with unchecked string replacement. CI
  does not regenerate and compare them with the header.
- Build artifacts use the ambient stable Rust toolchain and are not shown
  reproducible. The prebuilt hash table is empty, and no artifact mismatch
  test exists beyond an ABI-major check.
- The native build matrix cross-compiles Linux arm64 and Android but does not
  run them. It omits Windows arm64 builds and all required x64/arm64 runtime
  combinations.
- Dependency advisories, licenses, and maintenance risks are not recorded.

## Verification and documentation gaps

The README is empty. There is no public contract, state machine, race table,
platform matrix, architecture record, security guide, build guide, benchmark
protocol, capability comparison, or usable example outside API comments.

The repository has no committed direct-native or competitor benchmark,
Ghostty-equivalent comparison, ABI harness, binding agreement test, property
test, lifecycle model, deterministic fault layer, fuzz target, sanitizer job,
leak test, fairness test, multi-gigabyte integrity test, or multi-day soak
harness. Existing tests cover common Unix behavior and several build-hook
helpers, but Windows session behavior is excluded and race and failure
coverage is sparse.

## Starting-observation disposition

Every starting observation in `EXECUTION_SPEC.md` is confirmed:

- README guidance is absent.
- Windows end-to-end session tests are excluded.
- Android and several non-host architectures receive compilation only.
- spawn is synchronous FFI.
- asynchronous write failures use the output error path.
- signaling targets only the direct child.
- modes use fixed polling.
- sessions create several dedicated workers.
- default input and output budgets are large per session.
- required competitors, native baseline, fuzzing, ABI, fault, and soak
  infrastructure are absent.
- lifecycle and cross-platform error semantics remain implicit.
