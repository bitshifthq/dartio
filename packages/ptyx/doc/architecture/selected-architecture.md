# Selected architecture

## Decision

`ptyx` uses direct Rust platform backends with shared readiness-driven I/O.
Unix process creation and reaping are isolated in a persistent helper process.
Windows uses direct ConPTY and Job Object ownership.

This is Candidate B with a mandatory Unix broker. The broker is not an
optional hardening layer. A child created directly inside a Dart process can
be reaped by the host runtime, and an embedded multithreaded process cannot
make a descriptor snapshot followed by `fork` race-free. Both failures were
observed in representative prototypes.

Candidate A is rejected because `portable-pty` hides or performs work across
the process-creation and Windows ownership boundaries that this contract must
control. Candidate C is rejected because the Zig prototype failed its first
PTY lifecycle gate and showed no material whole-package advantage in the
equivalent language comparison.

## Runtime topology

### Dart boundary

- Dart owns public state, typed errors, stream semantics, and API validation.
- A session is published in stages. Native routing exists before the session
  becomes active and before any notification can escape.
- Capacity and flush waits use generation-tagged waiter tokens. Registration
  atomically returns ready, armed, or failed; it never performs a
  check-then-listen sequence.
- Concurrent waits are retained independently in bounded tables. A capacity
  request larger than the session's input bound fails immediately rather than
  occupying a waiter slot.
- Output is posted as copied typed data with at most one message in flight per
  session. Native queued bytes, the posted message, and a synchronously
  paused listener all count against the same output bound.
- Delivery returns credit with a bounded nonblocking command. Interactive
  messages bypass bulk coalescing; bulk output uses a size target and bounded
  deadline so native-port traffic cannot devolve into one message per PTY
  read. Pause and resume never rearm delivery while a posted message remains
  uncredited.
- Output cancellation changes the session to drain-and-discard. It does not
  recursively close the stream or silently terminate the process.
- Loss of a Dart port is a terminal typed failure. It starts native cleanup,
  fails all waiters, and never leaves a session waiting for another Dart call.
- A failed operational post commits the affected session to native
  abandonment. Dart posts and native-finalizer route removal share a native
  lock, so an isolate-group shutdown waits for any in-flight post and prevents
  a later post from starting.

### Native controller

- One process-wide controller owns a bounded command queue, a generation
  registry, Dart notification routing, and platform reactor state.
- The native module is pinned before controller threads start. The controller
  intentionally lives for the host process, so library unloading cannot race
  a reactor, notifier, broker-controller, closer, or TLS destructor.
- Dart object collection uses `NativeFinalizer`, whose SDK contract guarantees
  its callback no later than normal isolate-group shutdown. The pinned callback
  only removes native routing and enqueues idempotent abandonment; it never
  calls a Dart API. Active-session receive ports keep a standalone owner alive.
  A separate per-owner supervisor receives staged handles before publication
  and abandons them if that individual isolate exits. The native notifier
  performs no periodic liveness posts.
- A staged spawn has no Dart notification route. Activation installs the route
  only after the finalizer is attached, under the same lock used by
  native-to-Dart posts and finalization.
- Controller initialization is transactional. The registry, reactor,
  notifier, broker or IOCP owner, and failure route either become observable
  together or are all torn down before initialization reports failure.
- Reactor, notifier, broker-channel, or IOCP-owner death atomically fails every
  affected sub-resource with a typed infrastructure error and starts cleanup;
  no synchronous request waits on a dead owner.
- Linux uses `epoll`. macOS uses a shared `poll` reactor because the measured
  `kqueue` candidate missed the input-throughput gate. A separate
  direct-parent prototype also failed exit ownership when the Dart host
  reaped its children; the mandatory broker corrects that ownership boundary.
  The broker channel has its own bounded controller because it owns a framed
  request/response transaction. Windows uses IOCP for overlapped controller
  pipe ends.
- Work is scheduled with command, byte, and syscall quanta. A busy session
  cannot drain the complete command queue or monopolize the readiness batch.
- Input and output use chunk queues with explicit byte accounting. Unix reads
  into uninitialized scratch storage, treats only the successful `read`
  prefix as initialized, and copies that exact prefix into its owned queue;
  it never zero-fills unread capacity. Filter changes occur only on state
  transitions.
- Child exit, PTY EOF, close, and terminal write failure resolve every
  accepted input sequence and every capacity or flush waiter with completion
  or typed failure. Once input fails, later writes cannot be accepted.
- Handles are never reused after generation exhaustion. A retired slot remains
  retired instead of wrapping a stale generation back into validity.
- A session is destroyed only after its child identity is no longer owned,
  accepted input has completed or failed, output has reached its terminal
  state, and platform resources have been reclaimed.

### Unix spawn and reap broker

The controller launches one single-threaded broker. macOS uses
`posix_spawn`; Linux uses an audited raw `fork`/`exec` launch to preserve the
pre-created control socket. The broker, rather than the Dart host, is the
parent of every PTY child.

- The broker resets its signal mask and dispositions, including `SIGCHLD`,
  before it accepts requests.
- It atomically opens PTY descriptors with close-on-exec, forks only from its
  single thread, establishes the session and controlling terminal, and runs a
  fixed async-signal-safe child syscall sequence before `exec`.
- It observes direct-child status with non-reaping `waitid`, retains the zombie
  leader to prevent process-group ID reuse, and performs exact `waitpid` reap
  only after descendant cleanup. The controller sends signal requests through
  the broker and never signals a cached PID after broker loss.
- PTY masters move to the controller with `SCM_RIGHTS`. A versioned, bounded,
  nonblocking protocol carries generation IDs, spawn results, signal
  acknowledgements, exit status, and cleanup state.
- Linux receives transferred masters with `MSG_CMSG_CLOEXEC`, making
  close-on-exec atomic. Darwin does not expose that receive flag; the
  controller applies `FD_CLOEXEC` while parsing the returned control message,
  before publishing the descriptor, and every ptyx `posix_spawn` uses
  `POSIX_SPAWN_CLOEXEC_DEFAULT`. A foreign native component that performs
  inheriting process creation concurrently with that short Darwin receive
  interval remains an external integration hazard.
- The handshake identifies protocol version, target architecture, helper
  build identity, and controller ABI. Controller and helper protocol constants
  are checked together by protocol tests; consolidating them into generated
  definitions remains build-system work.
- Requests and responses use fixed, bounded frames with validated payload
  lengths. Close, release, abort, and signal acknowledgements are completed by
  the broker-controller thread and do not block the PTY readiness reactor.
- Controller EOF makes the broker terminate and reap all jobs. Broker EOF
  makes the controller issue a terminal-bound signal (`SIGQUIT` on Linux,
  `SIGKILL` on macOS), resolve the foreground group through the still-owned
  terminal, and force that group down before closing each PTY master and
  reporting any remaining cleanup uncertainty. It does not signal a cached
  group unless `tcgetsid` still binds that identity to the owned terminal.

For ordinary Dart command-line applications, the library may materialize an
integrity-checked embedded broker to an owner-only, version-and-hash-qualified
path. Materialization uses an interprocess lock and revalidates the final
owner, mode, type, and content after winning or observing a concurrent
installation. Applications using a hardened runtime, sandbox, or platform
signing policy must provide a signed broker path in their application bundle.
The package must reject an unusable helper during initialization rather than
falling back to in-process `fork`.

### Linux ownership details

The Linux broker opens the PTY pair with `openpty`, sets close-on-exec before
fork, and retains the master in the broker until descriptor transfer succeeds.
The single-threaded child branch performs `setsid`, `TIOCSCTTY`,
foreground-process-group setup, `dup2`, closure of known descriptors, signal
reset, and `execve`.

The controller observes only PTY masters and its wake descriptor through
`epoll`; it never waits for or signals a PTY child. The broker blocks
`SIGCHLD`, consumes it through `signalfd`, and periodically re-observes every
known direct child so rapid exits do not depend on one signal edge. It records
status with `waitid(..., WNOWAIT)` and retains the zombie leader until release.
This remains safe because no other thread or host runtime is a parent of those
children. A generation-tagged live-job entry is removed only after group
cleanup and exact reap.

Controller loss closes the broker channel, terminates each owned process
group, reaps every child, and exits. Broker loss makes the controller drop
all PTY masters after a terminal-bound forced signal and produce a typed
infrastructure failure; it never signals cached numeric identities. Linux
helper materialization obeys the same owner/mode/hash/ABI checks as macOS. A
no-exec cache or temporary filesystem requires an explicit executable helper
path; it never triggers in-process fork fallback.

Linux x64 requires a retained runtime integration run before release. Linux
arm64 must cross-build during development and run on a representative arm64
runner before that architecture is advertised.

### Windows ConPTY

Windows does not use the Unix broker.

- ConPTY-facing pipe ends are synchronous; controller-facing ends are
  overlapped and associated with IOCP.
- `HPCON`, process, thread, Job Object, attribute list, pipe handles, pinned
  `OVERLAPPED` operations, and their completion state have separate owners.
- `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` and
  `PROC_THREAD_ATTRIBUTE_JOB_LIST` place the child into the pseudoconsole and
  kill-on-close job as one spawn transaction.
- Pipe names are cryptographically unpredictable, use
  `FILE_FLAG_FIRST_PIPE_INSTANCE`, and receive a restrictive DACL.
- Environment keys are compared with ordinal, case-insensitive UTF-16
  semantics. Required entries such as `SystemRoot` are preserved.
- Cancellation retains every `OVERLAPPED` allocation until its terminal IOCP
  completion. A legitimate process exit code of 259 remains exit code 259.
- The runtime floor is Windows 10 version 1809, build 17763, where ConPTY was
  introduced. Blocking `ClosePseudoConsole` behavior on older implementations
  is isolated on a bounded closer pool with admission limited to 128 live or
  quarantined sessions.
- Windows client and Server SKUs are qualified separately. Compilation alone
  is not runtime qualification.

## Failure and close ordering

Normal close is idempotent and follows this order:

1. stop accepting public writes;
2. resolve every accepted input sequence by completion or typed failure;
3. request job termination from the platform owner;
4. continue reading until the platform output boundary reaches EOF;
5. observe and publish exact exit status independently from output completion;
6. cancel or drain platform I/O while retaining its memory and handles;
7. release PTY, process, job, broker-routing, and registry ownership;
8. complete cleanup.

On Windows, step 3 atomically terminates the Job Object and cancels pending
writes. The controller then initiates bounded `ClosePseudoConsole` while the
overlapped output pipe remains owned and continues draining it to broken pipe.
Only after terminal IOCP completions have been consumed may it free pinned
`OVERLAPPED` storage or close the controller pipe, process, thread, job, and
IOCP handles. Windows therefore does not wait for output EOF before initiating
HPCON close.

Graceful termination has a caller-configured deadline. Forced termination,
broker acknowledgement, exit observation, PTY EOF, and platform cancellation
each have an explicit bounded phase. When a phase expires, the controller
continues every independent safe cleanup action and completes close with a
typed `PtyCloseException` describing which ownership result is uncertain.
Timeout never authorizes signalling a cached Unix PID after broker loss or
freeing a Windows `OVERLAPPED` allocation before terminal completion. Such
resources remain owned by a process-level quarantine until the platform owner
confirms completion or process shutdown reclaims them.

Port loss, reactor loss, broker loss, spawn rollback, and forced close enter
the same state machine at an explicit failure edge. None is implemented as a
separate best-effort destructor.

## Selection evidence

The decision is based on the retained baseline and candidate evidence plus the
corrective prototypes described in
`../evidence/candidate-investigation.md`.

The decisive results are:

- direct ownership matched the same-host direct input boundary and met the
  latency and idle-scaling gates;
- the representative broker-controller-Dart slice demonstrated bounded
  queues, copied typed-data delivery, bounded multi-message output credit, no
  per-session I/O workers, race-free capacity waits, concurrent close,
  pause/cancel/no-listener bounds, and typed cleanup after a real broker kill;
- the pre-correction Candidate B prototype recorded five post-warmup 128 MiB
  output runs at 89.847 to 90.968 MiB/s, five 32 MiB input runs at 5.485 to
  5.540 MiB/s, and 400 one-byte round trips at p50/p95/p99 of 134/193/288
  microseconds. Correctness fixes were verified on a later artifact, so these
  numbers remain directional selection evidence rather than a cleared
  production performance gate;
- 1/4/16-session fairness runs showed progress for every session. The
  saturated 16-session noisy throughput spread was approximately five
  percent; its 64.2 ms quiet p99 remains a production regression target;
- the Unix broker demonstrated controlling-terminal behavior, failure
  rollback, exact broker-owned reaping, descriptor reclamation, controller-EOF
  cleanup, and ordinary Dart JIT/AOT helper materialization;
- stress testing the direct-parent integrated slice proved that host
  `SIGCHLD` behavior can steal exit status, making broker-owned parenting a
  selection requirement;
- the Windows slice cross-compiled for x64 and arm64 and established the
  required ownership model. The production implementation subsequently
  bounded older `ClosePseudoConsole` behavior with admission and quarantine;
  exact-target runtime qualification remains a release gate.

Independent correctness and platform re-reviews passed this architecture for
implementation. The corrected vertical slice passed its correctness checks,
but it was not the exact artifact used for the retained performance samples.
The review is scoped to selection; production benchmarks, competitor
comparisons, soak, sanitizers, and target qualification remain release gates.

Selection does not claim that the corrective prototypes are production code.
Their remaining findings are requirements on the implementation above.
