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
