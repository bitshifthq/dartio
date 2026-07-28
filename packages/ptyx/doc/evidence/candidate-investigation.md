# Architecture candidate investigation

This investigation uses the rejection gates in
`docs/architecture/candidate-gates.md`. Temporary prototypes are not package
implementations and are not included in the source distribution.

## Candidate A: Rust retaining portable-pty

The prototype at `/private/tmp/ptyx-candidate-a-20260727` used
`portable-pty` 0.9.0 for PTY allocation and process creation, then used the
raw Unix master descriptor with one `kqueue` reactor. Its measurement boundary
ended before Dart FFI, native-port messages, Dart queues, and output credit.

| Workload | Result |
|---|---|
| 100 failed spawns | descriptor count returned from 4 to 4 |
| 400 one-byte echoes, three runs | p99 180, 151, and 153 microseconds |
| 128 MiB exact output, three runs | 18.931, 19.189, and 21.151 MiB/s |
| 32 MiB exact input, three runs | 4.294, 4.314, and 4.335 MiB/s |
| Profiled 32 MiB input | 81,347 context switches and 3.896 MiB/s |
| Paused output | exactly 1 MiB retained during a 750 ms pause, then exact resume |
| 100 idle sessions | one reactor thread, 101 additional descriptors, zero descriptor delta after cleanup |
| Parent footprint with 100 sessions | 2,876 KiB physical and peak footprint |

The slice proved that one readiness reactor can replace per-session Unix I/O
workers while retaining strict native bounds. It did not prove the equivalent
Dart boundary, did not reach the direct-baseline throughput gate, and produced
more input context switches than the base.

More importantly, keeping `portable-pty` as the process abstraction fails
non-negotiable ownership gates:

- its Unix `pre_exec` path performs `/dev/fd` filesystem iteration and
  allocation after fork;
- its Windows implementation hides pipe and pseudoconsole ownership needed
  for cancellation and bounded teardown;
- it does not own a Windows job for descendant cleanup;
- version 0.9.0 contains inverted `TerminateProcess` result handling and its
  callers suppress the error.

Repairing those boundaries requires direct platform process backends, which
is Candidate B. Candidate A is therefore rejected before implementation
review.

Commands:

```text
cargo build --release --offline
target/release/ptyx-candidate-a --bench-spawn-failure 100
target/release/ptyx-candidate-a --bench-interactive 400
target/release/ptyx-candidate-a --bench-output 134217728
target/release/ptyx-candidate-a --bench-input 33554432
target/release/ptyx-candidate-a --bench-pause 33554432 750
target/release/ptyx-candidate-a --bench-idle 100
/usr/bin/time -l target/release/ptyx-candidate-a --bench-input 33554432
```

## Candidate B: direct Rust platform backends

The first prototype at `/private/tmp/ptyx-candidate-b-bL4HY0` used direct
Darwin PTY allocation, `posix_spawn`, nonblocking I/O, and `kqueue`.

Focused results:

- 1,000 nonexistent executable attempts returned the descriptor count from
  3 to 3;
- an intentionally inheritable sentinel descriptor was not visible in the
  child;
- 100 idle sessions used 103 controller descriptors and no per-session
  controller threads;
- exact 128 MiB output runs ranged from 59.536 to 79.262 MiB/s in the
  investigator's first set and from 76.535 to 78.822 MiB/s in a confirming
  set;
- exact 32 MiB input runs ranged from 5.064 to 5.208 MiB/s;
- foreground-group termination reached the child and preserved exit 42.

The spawn inspection also found a decisive defect in that subdesign. The
child had a new session and process group, but `tcgetpgrp(stdin)` failed with
`ENOTTY` and opening `/dev/tty` failed. A foreground process group reported
through the master was therefore insufficient evidence of a controlling
terminal.

The next slice replaced that subdesign with a raw fork/exec transaction. All
argv storage and a descriptor-close snapshot are materialized before fork.
The child uses a fixed syscall sequence to reset signals, create the session
and controlling terminal, duplicate streams, close the snapshotted
descriptors, and exec. This proved controlling-terminal behavior, but the
snapshot is not a production repair: another host thread can open an
inheritable descriptor after the snapshot and before fork. Independent review
therefore rejected this spawn boundary for an embedded multithreaded Dart
process.

The repaired Dart FFI slice then demonstrated:

| Workload | Retained result after one warmup |
|---|---|
| 400 one-byte echoes | p99 68, 65, and 62 microseconds |
| 128 MiB exact output | 89.254, 85.767, and 90.385 MiB/s |
| 32 MiB exact input | 5.589, 5.684, and 5.576 MiB/s |
| Paused output | native reading stopped at 0 queued bytes; exact 8 MiB after resume |
| Trailing output | exact `TRAILING` after child exit |
| Concurrent close | eight isolate callers succeeded; later stale close rejected |
| 100 idle sessions | 12 to 112 descriptors, 10 to 10 threads, 820 KiB RSS increase |
| Cleanup | descriptors returned to 12 |
| 1,000 missing executables | all failed, 8.42 seconds total, descriptors remained 12 |

The same-host direct-native input boundary produced 5.596 MiB/s, so the
corrected Dart FFI slice reached approximately 100 percent of it. A repeated
base input run produced 4.754 MiB/s.

The predeclared total-context-switch improvement did not occur. The corrected
slice recorded 72,434 switches, the repeated base recorded 72,263, and the
minimal direct-native boundary recorded 84,022. This proves that total
parent-plus-child PTY scheduling is not a valid requirement for choosing the
controller architecture on this host: even the minimal direct boundary cannot
meet it. The production benchmark therefore retains total switches as a
reported metric but uses controller-originated syscalls and wakeups, isolated
by profile, as the actionable regression target. This exception changes no
byte, boundedness, cleanup, latency, or throughput gate.

The direct-backend architecture remains the only candidate direction capable
of explicit ownership for Unix jobs and descriptors and for Windows ConPTY
pipes, `HPCON`, process, thread, job, attribute-list, overlapped-I/O, and
cancellation state. The temporary caller-pumped slice is not production code,
and listing Windows resources is not Windows feasibility evidence. Candidate
B is not selected by composing its results with Candidate A: all three
independent reviews required a representative integrated slice and concrete
platform corrections first.

## Independent candidate reviews

Three read-only reviews evaluated Candidate B after Candidates A and C were
rejected by their feasibility gates.

### Correctness, ownership, and lifecycle

Verdict: corrections required before selection.

Critical findings were an abandoned child when post-exec parent setup fails,
the possibility of signalling a reused process group after exit observation,
and accepted input being discarded without either flush completion or typed
failure. High findings covered the descriptor-snapshot race, ambiguous
exec-error-pipe failure, unbounded and lossy cleanup, handle-generation
retirement, missing panic containment, and the absence of an assembled
reactor/Dart-credit/platform slice.

The correction gate includes injected post-exec/pre-publication failure,
close-versus-reap and write/flush races, descriptor churn during spawn,
accepted-input failure completion, port loss, trailing output, and bounded
forced cleanup.

### Performance design and benchmark fairness

Verdict: corrections required before selection.

The review found that the caller-pumped direct slice and the pre-Dart
portable-pty reactor are not an integrated or equivalent-boundary comparison.
Neither proves real Dart stream credit, event-driven input capacity,
simultaneous bidirectional traffic, per-session fairness, or controller
efficiency. Several retained runs also crossed the predeclared five-percent
investigation threshold without complete raw reproduction metadata.

The correction gate is one shared `kqueue` reactor over the direct ownership
core, with explicit command wakeup, event-driven exit, bounded queues,
per-session byte and syscall quanta, and a real Dart subscription/write path.
Equivalent direct and integrated runs must cover latency, input, output,
bidirectional traffic, full-cap pause and exact resume, event-driven capacity
recovery, 1/4/16-session noisy-neighbor fairness, 100 idle sessions, resource
use, and retained internal counters.

### Platform feasibility and maintainability

Verdict: corrections required before selection.

The descriptor-snapshot race is critical, and the prototype has no Linux or
Windows implementation. A persistent single-threaded Unix spawn/reap broker
is the leading correction because it can exclusively own fork, children,
reaping, and descriptor transfer. It is conditional on a versioned bounded
protocol, failure-atomic `SCM_RIGHTS`, controller/broker-loss cleanup, and a
proven executable build, integrity, signing, location, and deployment path.
The current native-asset hook models only one dynamic library and cannot be
assumed to distribute an executable helper.

Windows requires a real ConPTY slice: synchronous ConPTY-facing named-pipe
ends, overlapped controller-facing ends on IOCP, lifetime-safe cancellation,
atomic `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` and
`PROC_THREAD_ATTRIBUTE_JOB_LIST`, direct process and job ownership, trailing
output, and bounded pre-Windows-11-24H2 shutdown. Windows arm64 must at least
cross-build and remains a runtime qualification gap until it runs on real
hardware.

The three verdicts keep architecture selection open. Corrective prototypes and
focused independent re-review are required before implementation begins.

## Candidate B corrective prototypes

The corrective work was split into three focused slices. Each slice was used
to prove or reject an ownership boundary; none is production package code.

### Persistent Unix broker

The prototype at `/private/tmp/ptyx-candidate-b-broker-0B35LG` is a
single-threaded broker launched with `posix_spawn`. It implements a bounded,
versioned control protocol, transfers PTY masters with `SCM_RIGHTS`, owns
`fork`, controlling-terminal setup, `exec`, signalling, and exact reaping, and
uses generation-tagged job identities.

Independent reruns produced:

- 40 controlling-terminal sessions under descriptor churn with no inherited
  descriptor or final descriptor leak;
- rollback for missing executables and injected post-exec setup failure;
- no signal after reap;
- typed failure for accepted input;
- no interference with an unrelated host `waitpid`;
- 100 idle sessions with controller descriptors returning from 104 to 4,
  broker descriptors remaining 5 to 5, and zero remaining jobs;
- child reclamation after controller EOF; and
- detected broker death with best-effort controller cleanup.

An embedded-helper materializer reproduced under Dart JIT and AOT. It used a
536,844-byte broker with SHA-256
`cd588e9bcd69bad73f344e79139b89b83c4ebd4e86d846a170aff6f712fb5c96`,
an owner-only version-and-hash path, no-follow/create-new semantics, content
verification, synchronization, and atomic replacement.

Review found five requirements still missing from the prototype:

- reset the `SIGCHLD` disposition as well as the signal mask;
- use incremental nonblocking framing instead of `MSG_WAITALL` request and
  response stalls;
- never signal a cached PID or process group after broker loss;
- serialize helper materialization across concurrent processes and qualify
  hardened-runtime, sandbox, and signing behavior; and
- open broker descriptors atomically close-on-exec and close only the
  broker's known descriptors rather than scanning a full descriptor table.

Those findings are incorporated into the selected architecture. Ordinary CLI
materialization is evidence, not proof for a signed or sandboxed application;
those applications require an explicitly bundled and signed helper.

### Integrated macOS reactor and Dart stream

The prototype at `/private/tmp/ptyx-candidate-b-integrated-saI2fI` began with
a single `kqueue` thread and was corrected into a broker-integrated shared
`poll` reactor after measured input throughput rejected the `kqueue`
implementation. It includes a wake descriptor, bounded native queues,
per-event byte and syscall quanta, copied typed-data Dart native-port
messages, one-message output credit, and a real Dart `Stream`.

The first integrated result was not selectable:

- one-byte p50/p95/p99 was 158/201/280 microseconds versus direct
  12/22/62 microseconds;
- output was 82.366 MiB/s versus direct 94.986 MiB/s, or 86.71 percent;
- input was 4.643 MiB/s;
- 32 MiB input caused 65,612 write syscalls and 32,295 capacity
  notifications; and
- Dart contained deterministic check-then-listen capacity and flush races.

The correction replaced broadcast waits with tokenized register-or-ready
capacity and flush waiters, staged native publication, explicit output-done
and typed wait-failure events, permanent input failure state, no signalling
after recorded exit, destruction only after recorded reap, port-post failure
injection, and nonrecursive output cancellation. It also made exec-status and
post-exec controller failures kill and reap the child. Native contract tests
reached 9 passing cases and Clippy was warning-free; the focused Dart
capacity, early-output, and port-loss cases passed in isolation.

Stress testing then exposed the decisive remaining defect. Eight concurrent
exact-output tests all received exact output and output completion, but six
never received exit status and waited in teardown. The fixture processes were
no longer live. The Dart host had reaped children that the integrated slice
created directly, so `kqueue` could not provide status to the package. An
activation-time `waitpid(WNOHANG)` check narrowed the registration race but
did not fix host reaping.

This result rejects direct child parenting inside the Dart process. The
production reactor consumes broker-owned exit messages and owns only the PTY
master. It does not use host-global child waiting or claim that process
readiness alone gives it exclusive reap ownership.

The corrected representative vertical slice then connected the external
broker, controller, PTY reactor, C ABI, Dart native ports, and Dart stream.
The broker remained the only parent and reaper; its exit messages replaced
host-global child waiting. The slice added chunk queues, cached readiness
state, bounded 1,024-command and 4,096-notice queues, a 64-command quantum,
64 KiB byte quanta, generation retirement, staged publication, and terminal
infrastructure failure.

The initial integer-notice plus synchronous pull/credit boundary passed
single-session throughput but amplified quiet latency under noisy neighbors.
The final boundary posts copied typed data directly, allows one message in
flight per session, and returns credit with a bounded nonblocking command.
Messages up to 256 bytes bypass bulk coalescing; bulk output targets 64 KiB
with a 10 ms ceiling.

Retained final measurements are in
`benchmark/results/candidate-b-integrated-macos-x64-2026-07-27.json`:

- five post-warmup 128 MiB output runs: 89.847, 90.968, 90.847, 90.137,
  and 90.353 MiB/s;
- five 32 MiB input runs: 5.540, 5.528, 5.520, 5.502, and 5.485 MiB/s,
  versus four post-warmup direct samples from 5.511 to 5.587 MiB/s;
- 400 one-byte echoes: p50/p95/p99 134/193/288 microseconds;
- four-session quiet p99 6.317 ms with each of three saturated sessions
  reaching 1.573 to 1.626 MiB/s; and
- sixteen-session quiet p99 64.213 ms with all fifteen saturated sessions
  progressing between 0.346 and 0.363 MiB/s.

The saturated quiet tail is retained as a production regression target. It is
not a starvation result: every session completed and noisy throughput spread
was approximately five percent.

The focused correctness review then found four selection blockers in the
vertical slice: concurrent capacity or flush waiters could replace one
another, a request larger than the input bound could wait forever, broker exit
or PTY EOF did not permanently fail queued input, and pause/resume could post
a second output chunk before the first received credit.

The corrected revision stores bounded sets of capacity and flush waiters and
notifies or fails every token. It rejects impossible capacity requests
immediately. Broker exit, PTY EOF, close, and write failure all enter one
permanent input-failure transition that clears accepted input, fails all
waiters, and rejects later writes. The production path later replaced
single-flight output with several budget-charged messages and a Dart FIFO for
messages already posted when pause commits. Credit, not subscription resume,
rearms native reads, so the pipeline remains bounded and lossless.

Fifteen native tests and eleven Dart tests passed. The native suite includes
exact concurrent-waiter registration, all-ready and all-failed delivery,
oversize waits, terminal input failure, and pause/resume without credit. The
Dart suite covers exact early output, real native-port post failure,
no-listener bounds, stable native reads while paused, repeated slow-consumer
pause/resume, cancellation credit, race-free capacity recovery, impossible
capacity failure, concurrent flush failure on terminal exit, twenty
concurrent close callers, and a real broker `SIGKILL` that completes output
and exit with a typed infrastructure error. Clippy and Dart analysis were
warning-free. The raw result preserves both the benchmarked revision hashes
and the corrected verification revision hashes. The exact reviewed broker
hash is
`cd588e9bcd69bad73f344e79139b89b83c4ebd4e86d846a170aff6f712fb5c96`;
integrated source and binary hashes are retained with the raw results.

The focused correctness re-review rebuilt the corrected artifacts, matched
their recorded hashes, reran all 26 native and Dart tests, strict Clippy, and
fatal Dart analysis, and passed Candidate B for architecture selection. It
confirmed that every prior blocker is closed. Incremental broker framing,
helper distribution and signing, initialization failure containment, extreme
command-queue saturation, and target runtime qualification remain production
requirements rather than selection evidence.

### Windows ConPTY ownership slice

The prototype at `/private/tmp/ptyx-candidate-b-windows-KoHhML` modeled
separate synchronous ConPTY-facing named-pipe ends, overlapped
controller-facing ends on IOCP, separately owned `HPCON`, process, thread,
job, attribute list, and pinned `OVERLAPPED` state, and atomic pseudoconsole
plus job-list attributes.

Five host model tests passed. Host Clippy, x64 MSVC cross-Clippy, arm64 MSVC
cross-Clippy, and both cross-builds passed. No Windows runtime test ran.

Review correctly rejected the model as runtime proof. It also found:

- a detached cleanup thread could retain all session resources indefinitely
  on older Windows;
- predictable pipe names lacked first-instance enforcement and a restrictive
  DACL;
- the fault model did not inject failures into real handle acquisition;
- environment sorting used lossy Unicode and did not preserve `SystemRoot`;
- legitimate exit code 259 was treated as `STILL_ACTIVE`; and
- the slice had no C ABI or Dart port-loss evidence.

That prototype therefore required Windows build 26100 or newer. The selected
production implementation replaced the detached cleanup design with a bounded
closer pool, 128-session admission, and process-lifetime quarantine. Retained
Server 2022 testing later confirmed that pre-26100 ConPTY still leaked one
process handle per completed session, so production keeps the build 26100
floor. It also uses random first-instance secured pipes, exact UTF-16
environment handling, and exact exit-code observation. Windows x64 and arm64
runtime qualification remain explicit missing evidence; cross-compilation is
not a substitute.

## Candidate B selection

Candidate B is selected as direct Rust platform backends with a mandatory
Unix spawn/reap broker and shared controller reactors. The complete decision,
topology, ownership rules, platform floor, and failure ordering are in
`docs/architecture/selected-architecture.md`.

The selection resolves the independent reviews as follows:

- descriptor inheritance, `fork` safety, child reaping, PID reuse, and
  host-wait interference move behind the Unix broker;
- Dart publication, capacity, flush, output completion, and port loss use
  staged routing and tokenized terminal states;
- reactor performance findings become explicit chunking, coalescing,
  transition-caching, bounded-queue, and fairness gates;
- Windows handle ownership is retained, while compile-only evidence and older
  blocking close behavior are not represented as runtime support; and
- helper packaging is split between integrity-checked ordinary CLI fallback
  and an explicit signed-helper requirement for hardened applications.

Correctness, performance, and platform-focused re-reviews each passed
Candidate B for implementation. None represented the prototypes as
production-ready. Their remaining hardening and qualification findings are
requirements on the selected implementation and release evidence.

No review finding is dismissed because another prototype happened to pass.
Where the corrective slice could not close a finding, the selected design
changes the boundary or retains the item as a production validation gate.

Commands:

```text
cargo build --release --offline
target/release/ptyx-candidate-b inspect
target/release/ptyx-candidate-b failure
target/release/ptyx-candidate-b signal
target/release/ptyx-candidate-b output 134217728
target/release/ptyx-candidate-b input 33554432
target/release/ptyx-candidate-b idle100
```

## Candidate C: Zig 0.16

The prototype at `/private/tmp/ptyx-candidate-c-KoQzEY` contained a 19 KiB
Unix core, generation handles, bounded rings, a C ABI, and a C lifecycle
harness. The release x64 macOS dynamic library compiled to 24 KiB, and a
focused ring test passed.

The first real ABI lifecycle run did not pass:

```text
./abi_harness
# stage abi
# stage echo spawn
# no READY output after 30 seconds
```

The first macOS arm64 cross-build also failed because the translated target
headers could not find `util.h`. No Dart-boundary workload, Windows backend,
sanitizer run, fuzz target, or cleanup measurement completed. Zig 0.16 API
churn also required replacing removed mutex APIs before the full exports could
compile.

A smaller binary is not a whole-package advantage. Candidate C is rejected
because its first PTY lifecycle gate failed and it adds unproven build,
cross-platform, diagnostic, and maintenance paths without measured
Dart-boundary benefit.

An independent, smaller language-comparison harness under
`benchmark/candidates` did complete for both direct Rust and Zig. Both
libraries implemented the same synchronous C ABI and the same direct PTY
operations. The Dart harness included FFI calls, copied 64 KiB input and
output buffers, exact verification, registry lookup, child wait, and cleanup.
It excluded the production asynchronous reactor and bounded user-space
backpressure equally on both sides.

| Metric | Direct Rust | Direct Zig |
|---|---:|---:|
| One-byte p99 range, five runs | 32 to 56 microseconds | 37 to 59 microseconds |
| 32 MiB output range | 79.48 to 104.85 MiB/s | 76.51 to 109.37 MiB/s |
| 32 MiB input range | 4.56 to 4.77 MiB/s | 2.55 to 3.79 MiB/s |
| Spawn-close p50 / p99 | 16.92 / 18.20 ms | 17.38 / 18.15 ms |
| 100 idle sessions | 100, 9 threads, 152 KiB RSS delta | 100, 9 threads, 152 KiB RSS delta |
| Stale handle | rejected | rejected |
| Host dynamic library size | 380,272 bytes | 501,526 bytes |

Zig showed no stable latency, output, lifecycle, or scaling advantage and was
substantially slower and less stable for input. These results do not establish
production performance, but they directly refute the required material
whole-package performance advantage for replacing Rust. Raw results are in
`benchmark/results/candidate-rust-macos-x64-2026-07-27.json` and
`benchmark/results/candidate-zig-macos-x64-2026-07-27.json`.

Commands:

```text
zig fmt core.zig
zig test core.zig -lc
zig build-lib -dynamic -O ReleaseFast -lc -femit-bin=libptyc.dylib core.zig
zig cc abi_harness.c -L. -lptyc -lpthread -O2 -o abi_harness
./abi_harness
zig build-lib -target aarch64-macos -dynamic -O ReleaseFast -lc \
  -femit-bin=libptyc-arm64.dylib core.zig
```

## External revisions and platform sources

Behavioral source inspections used these upstream revisions:

- `wezterm/wezterm` and `portable-pty`:
  `76b606ec597a3c0263fa60321548637451c0a547`;
- `ghostty-org/ghostty`:
  `24f7fb983506469843c824f65e0c0f7cdf33661c`;
- `microsoft/node-pty`:
  `10155669a0dfe1d97ec925131a344dd48bf7953e`;
- `microsoft/terminal`:
  `4f225a56aff245bbb9d1400f266c20e1747cc580`.

The installed `portable-pty` 0.9.0 crate source corresponds to repository
revision `f8921727a11b9f8b073e8c24821d72fd41283500f`.

Platform feasibility follows the authoritative
[POSIX process-spawn specification](https://pubs.opengroup.org/onlinepubs/9799919799/functions/posix_spawn.html),
[Linux pidfd documentation](https://man7.org/linux/man-pages/man2/pidfd_open.2.html),
[Apple kqueue documentation](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html),
[Microsoft CreatePseudoConsole documentation](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole),
[Microsoft CancelIoEx documentation](https://learn.microsoft.com/en-us/windows/win32/fileio/cancelioex-func),
and
[Microsoft job-object documentation](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).
