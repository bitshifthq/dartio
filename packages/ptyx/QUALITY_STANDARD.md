# ptyx Quality Standard

## Purpose

`ptyx` is the low-level pseudo-terminal session primitive for Dart. It provides
safe, lossless, high-throughput control of local processes attached to native
pseudo-terminals.

The package is intended for interactive shells, terminal applications, IDE
terminals, TTY-dependent automation, long-running high-volume sessions, and
services that manage many concurrent sessions. It must be suitable for
critical production use.

This document defines the quality bar for the package. It governs public
behavior, native implementations, architecture, reviews, tests, benchmarks,
and maintenance. A feature is not complete when it merely works in a common
case. Its behavior, failure modes, resource use, cleanup, documentation, and
platform support must all satisfy this standard.

The words **must**, **must not**, **should**, and **may** are normative.

## Product boundary

`ptyx` owns:

- spawning a local process attached to a native pseudo-terminal;
- exact byte-oriented input and output;
- arguments, environment, working directory, and initial terminal size;
- resize, signaling, process exit, and terminal job cleanup;
- bounded buffering and backpressure;
- observable lifecycle and operation-specific failures;
- supported terminal metadata and mode snapshots;
- consistent Dart semantics over platform-appropriate native backends.

`ptyx` does not own:

- terminal emulation or escape-sequence parsing;
- rendering, keyboard mapping, or application UI;
- shell command parsing or implicit shell invocation;
- text encoding or line-ending conversion;
- remote terminal protocols such as SSH;
- sandboxing or privilege reduction;
- general process APIs unrelated to pseudo-terminals.

The package must not add behavior from the excluded areas for convenience when
that behavior would make byte transport or process execution ambiguous.

## Decision hierarchy

Design and review decisions follow this order:

1. Memory safety, process safety, byte integrity, and truthful semantics are
   non-negotiable.
2. Deterministic lifecycle, bounded resources, cleanup, and freedom from
   deadlocks and leaks are non-negotiable.
3. Platform correctness uses native-appropriate behavior instead of forced
   internal uniformity.
4. Throughput, latency, CPU use, memory use, allocation, copying, fairness, and
   scaling are primary design requirements.
5. The safest and most efficient path must also be the easiest path for a Dart
   caller.
6. Explicit ownership, limited unsafe code, maintainability, and useful
   diagnostics must preserve the preceding properties.
7. Compatibility is preserved unless it would retain a proven correctness,
   safety, or fundamental usability defect.
8. A feature is accepted only when its complete quality cost can be sustained.

Performance improvements must not introduce silent loss, unbounded buffering,
unsafe cleanup, misleading semantics, cross-session starvation, or fragile
platform behavior.

## Supported platforms

Support is an explicit `ptyx` guarantee. It is not inherited from a dependency
or inferred from successful cross-compilation.

The required production matrix is:

| Operating system | Architectures |
|------------------|---------------|
| Linux            | x64, arm64    |
| macOS            | x64, arm64    |
| Windows          | x64, arm64    |

Android armv7, arm64, and x64 are production-supported only after the same
relevant runtime qualification passes on devices or representative emulators.
Other Unix targets may be added through the same qualification process.

Every advertised target must run end-to-end tests for spawn, byte I/O,
backpressure, resize, signaling or native termination, process exit, error
handling, job cleanup, and resource reclamation. Compilation alone is not
qualification.

The Dart contract must be uniform where operating systems provide equivalent
capabilities. Genuine differences, including signal behavior, exit status,
terminal metadata, terminal modes, and ConPTY constraints, must be exposed as
explicit capabilities with documented semantics. The API must neither collapse
to a lowest common denominator nor pretend that different native facilities
are identical.

## Public API standard

`ptyx` has three supported consumption layers:

- an idiomatic pure-Rust crate that owns PTY behavior and operating-system
  mechanics;
- a stable, language-neutral C ABI over that crate;
- an idiomatic Dart package that uses the C ABI without exposing FFI concepts.

The Rust crate and C ABI are independently useful without Dart, the Dart VM,
Dart headers, native ports, or isolate lifecycle concepts. The Dart adapter
may contain only the transport needed to deliver C ABI events to an isolate
and reclaim an owner's resources when that isolate exits. It must not
reimplement PTY lifecycle, buffering, backpressure, failure precedence, or
platform behavior.

The public API must be:

- small enough that ownership and lifecycle remain clear;
- byte-oriented at the transport boundary;
- idiomatic for Dart;
- difficult to misuse accidentally;
- explicit about admission, backpressure, and delayed failure;
- capability-driven where behavior cannot be portable;
- fully documented, including errors and ordering.

Potentially unbounded OS or lifecycle operations must be asynchronous. Process
spawn and session close belong in this category. Input admission is synchronous
only because it performs bounded validation, ownership transfer, and queue
insertion without waiting on a child, worker, descriptor, handle, or queue
capacity.

The default API must require no performance tuning. An advanced configuration
surface may expose policy choices that callers need to enforce resource or
latency budgets:

- maximum buffered input;
- maximum buffered or in-flight output;
- a latency-versus-throughput profile;
- lossless backpressure or an explicit stream-cancellation discard policy;
- graceful-shutdown deadlines.

Platform buffer sizes, batching intervals, external-data thresholds, polling
details, worker topology, and similar implementation controls must remain
internal unless a demonstrated caller requirement makes them part of the
public contract. All exposed values must be validated against safe bounds.

## Byte transport

### Output

Output bytes must be emitted exactly once and in the order read from the
pseudo-terminal. The implementation must not silently drop, duplicate,
reorder, decode, normalize, or mutate them.

Output chunk boundaries are arbitrary. They carry no message, character, line,
or write-call meaning.

The output stream is single-subscription. Its behavior is:

- before a listener attaches, buffering remains bounded and then propagates
  backpressure to the child;
- a paused subscription remains bounded and then propagates backpressure;
- resuming continues delivery without loss;
- canceling explicitly requests drain-and-discard for later output so the
  child can continue;
- a normal child exit preserves all trailing output;
- stream completion means no later output can be delivered;
- an output read failure is reported on the output stream after every byte
  that can be delivered safely.

Canceling the output subscription is the sole public drain-and-discard
operation. Callers interested only in completion may attach and immediately
cancel a subscription. A separate session-level discard method must not
duplicate that stream lifecycle operation. Awaiting process exit without
consuming or canceling output must be documented as a possible source of
normal PTY backpressure.

### Input

Each write must either be accepted in full into a bounded owned queue or be
rejected in full. Partial acceptance must not be hidden.

Accepted writes preserve invocation order. Native partial writes,
interruptions, and temporary readiness failures must be handled internally
without changing that order. Returning from the write operation means that
`ptyx` accepted the bytes, not that the child consumed them.

The API must provide:

- one synchronous, all-or-reject write operation that transfers ownership into
  bounded native storage without waiting for capacity;
- a recoverable, typed backpressure error when the complete input cannot be
  admitted;
- sticky direction-scoped input failure after an unrecoverable native write
  error, observable through a later write, terminal output, or close.

Queue exhaustion must produce explicit backpressure. It must not cause
unbounded growth, session failure, blocking admission, or silent loss.

Temporary native write conditions are handled internally. An unrecoverable
write failure disables input but does not terminate output, exit observation,
signaling, metadata, or cleanup. Whole-session failure is reserved for loss of
infrastructure or ownership that makes continued operation unsafe.

## Lifecycle and ownership

A session owns the complete terminal job, not only the direct child process.
The native backend must establish and retain the platform-appropriate process
group, session, controlling-terminal, or Windows job relationship.

Signals intended for terminal behavior must reach the appropriate foreground
process group where the platform supports that model. Shutdown must reclaim
owned descendants without targeting unrelated or PID-reused processes. The
reported exit status remains the direct child's exit status.

A same-privilege child may deliberately escape terminal job or process-group
containment on platforms without an enforceable job facility. That process is
outside the portable cleanup guarantee and must not prevent `ptyx` from
reclaiming its own resources. The limitation must be documented rather than
hidden behind an unbounded shutdown wait.

Lifecycle behavior must be specified as an explicit state machine. Child
state, input state, output state, failure state, and resource ownership may
advance independently where the operating system permits it. The design must
define linearization points and deterministic results for:

- write racing with close or child exit;
- signal racing with process exit or reaping;
- resize racing with close;
- output EOF racing with exit notification;
- native failure racing with normal EOF;
- output pause or cancel racing with close;
- repeated or concurrent close;
- Dart isolate shutdown or native-port closure;
- partial spawn followed by failure.

Cleanup must be monotonic and idempotent. Handles, descriptors, buffers,
workers, child state, and Dart ports move toward release exactly once. Late
messages, stale handles, double completion, double close, use-after-free, and
PID reuse must be prevented by design and verified under stress.

Closing a session must have explicit graceful and forced termination phases
with bounded waits. It must always attempt complete cleanup. If a promised
cleanup result cannot be established, close must report that failure after
performing every safe best-effort action.

## Errors

No operational failure may be silently swallowed or reported through an
unrelated channel.

Errors must identify:

- the operation that failed;
- a stable package error category;
- the native status or OS error code when available;
- useful context that does not expose secrets or unstable implementation
  details.

Spawn, resize, metadata access, and queue rejection report their own failures.
A delayed native input failure is retained for later writes and close and,
after safely buffered bytes, becomes the terminal output event so it remains
observable without a separate input-completion API. Output read failures
affect output. Wait failures affect exit observation. Mode-observation
failures affect mode observation. Cleanup failures affect close.

An operation must distinguish an unavailable capability, a state in which the
operation no longer applies, and a failed native attempt. For example, an
already exited child is not the same result as a failed signal operation.

Panic, unwinding, or a foreign exception must never cross the C ABI.

## Terminal modes

An on-demand terminal-mode query is an accurate snapshot at the instant
provided by the operating system. Unsupported fields are represented
explicitly.

A mode-change stream is an opt-in stream of distinct states observed by
`ptyx`. Passive observation cannot guarantee every arbitrarily short-lived
termios transition. The API and documentation must not claim otherwise, and
the stream must not be used as a security boundary.

Mode observation should use reliable event-driven facilities where a platform
provides them without weakening byte integrity. Polling must occur only while
there is an observer, use an adaptive and benchmarked strategy, and report
observation failures. Tests must exercise rapid mode transitions and measure
the supported detection envelope. CPU impact must be measured with 1, 10, and
100 idle sessions.

## Native architecture

The Rust crate owns the authoritative cross-platform session semantics and
native backends. It owns OS calls, process and job relationships, thread or
reactor behavior, memory, descriptors, handles, bounded queues, failure
precedence, and cleanup.

The C ABI translates Rust ownership into explicit language-neutral handles,
events, status values, and buffer lifetimes. Dart owns its public API shape,
typed Dart errors, streams, and futures, but adapts rather than reimplements
the native state machine.

Linux, macOS, and Windows may use different internal strategies. Shared code
must represent genuinely shared semantics rather than hide meaningful native
differences.

The runtime topology is selected by evidence. Relevant candidates include
nonblocking descriptors with an efficient Linux reactor, `kqueue` on macOS,
and overlapped I/O or IOCP where ConPTY permits it on Windows. Dedicated
workers remain acceptable where an OS API requires them or measurements show
that they provide the best complete result.

The selected design must:

- keep blocking spawn and reaping work away from Dart and I/O reactors;
- avoid fixed wakeups and polling when event notification exists;
- bound work per wakeup and provide cross-session fairness;
- isolate a stalled, failed, or backpressured session from other sessions;
- initialize and tear down safely across Dart isolates;
- define session serialization and isolate ownership;
- scale to the required session counts without thread explosion.

Idle sessions must not require periodic wakeups unless an observed capability
has no event-driven implementation. Runtime work must be coalesced and fairly
bounded per wake. Default queue sizes and batching must minimize resident
memory at 100 sessions while retaining the throughput and latency envelope.
CPU time, wakeup count, context switches, and idle energy impact are
qualification metrics, not incidental diagnostics.

Portable-pty is an implementation tool, not an architectural constraint. Every
relied-upon spawn, ownership, inheritance, exit, signal, resize, I/O, and
cleanup behavior must be audited and benchmarked. The dependency may be
patched, bypassed, reimplemented, or replaced when evidence shows that it
cannot satisfy this standard. Focused upstream fixes are preferred when they
can meet the requirement without delaying correctness.

Rust is the provisional native language. A substantial Rust rearchitecture is
allowed. A Zig implementation may replace it only after representative
prototypes demonstrate a material whole-package advantage across correctness
risk, throughput, latency, CPU, memory, scaling, binary size,
cross-compilation, build reliability, diagnostic tooling, and
maintainability. A microbenchmark win is insufficient. A permanent mixed
Rust-and-Zig core should be avoided.

Unsafe code must be minimal, local, and accompanied by the ownership, lifetime,
aliasing, layout, and concurrency invariants that make it valid. Generated
bindings must follow their authoritative header or interface definition.

## Performance standard

`ptyx` aims to be the best-performing production-quality PTY package across
the full scorecard, not the winner of a single throughput microbenchmark.

The benchmark set must include direct native baselines, comparable PTY
packages, and mature terminal PTY implementations. Ghostty's PTY path is one
important reference. Comparisons must isolate equivalent transport work and
use the same child, payload, terminal mode, host, buffer policy, warmup, and
measurement boundaries.

The minimum performance envelope is:

- no more than 2 ms of added idle batching latency at p99 under normal host
  load;
- at least 90% of the sustained throughput of a minimal direct native PTY
  baseline unless a documented platform boundary proves a different ceiling;
- multi-gigabyte bidirectional transfers with exact byte verification;
- 100 concurrent idle sessions;
- at least 16 simultaneously active high-output sessions;
- multi-day sessions with repeated resize, pause, resume, writes, process
  changes, and output bursts;
- bounded and documented memory under every backpressure state;
- no starvation of another session or the Dart isolate by a noisy session.

Benchmarks must report:

- sustained and burst throughput;
- p50, p95, and p99 latency;
- CPU time and utilization;
- resident and peak memory;
- allocations and byte copies where measurable;
- worker, descriptor, and handle counts;
- scaling by idle and active session count.

The strongest comparable implementations establish competitive targets for
throughput, latency, CPU, and memory. No one competitor defines the
architecture, and no one metric may be optimized by neglecting the others.

Benchmarks must be reproducible, retained as baselines, and run on Linux,
macOS, and Windows. A regression beyond 5% in a stable benchmark must be fixed
or receive an explicit, evidence-backed exception explaining the complete
tradeoff.

## Security standard

Children run with the parent process's OS privileges. `ptyx` is not a sandbox,
and its documentation must say so plainly.

The package must remain safe when a child is buggy or hostile:

- executable and arguments are passed directly without implicit shell
  interpolation;
- Windows quoting and environment construction preserve exact arguments and
  values;
- untrusted output cannot cause memory corruption, unbounded allocation,
  invalid Dart messages, or native panic;
- floods, stalls, rapid mode changes, inherited descriptors or handles,
  descendants, and termination resistance cannot wedge cleanup indefinitely;
- pointers, lengths, counts, flags, encodings, integer conversions, and
  messages are validated at every interoperability boundary;
- signals cannot target an unrelated or PID-reused process;
- secret environment values and input bytes do not appear in errors, logs,
  diagnostics, or benchmarks by default;
- native artifacts are integrity-verified and reproducible;
- dependencies are pinned, reviewed, and monitored for relevant
  vulnerabilities.

Security defects are correctness defects. They do not receive performance or
compatibility exceptions.

## Verification standard

Verification is permanent engineering infrastructure, not a one-time
publication exercise. Every supported behavior must have success, boundary,
failure, race, and cleanup coverage appropriate to its risk.

The required verification layers are:

### Contract and integration tests

- Dart tests for every documented public behavior and error.
- End-to-end runtime tests on every supported OS and architecture.
- Interactive shell, TTY-dependent automation, high-volume child, many-session
  service, and hostile or stuck child scenarios.
- Deterministic regression tests for every fixed defect.

### Native and ABI tests

- Native unit tests for queues, state transitions, ownership, conversions,
  status mapping, and cleanup.
- A standalone C harness for layouts, calling conventions, version mismatch,
  ownership transfer, null and invalid inputs, and panic containment.
- Tests that authoritative headers and generated Dart bindings agree.

### Generated and adversarial tests

- Property-based tests for byte ordering, arbitrary chunking, arguments,
  environment conversion, sizes, and lifecycle sequences.
- Model-based tests that generate spawn, write, pause, resume, resize, signal,
  exit, failure, and close races.
- Fuzzing of the C ABI, native conversions, and Dart-native message decoding.
- Fault injection for allocation, thread or reactor setup, port posting,
  partial writes, interruption, temporary unavailability, broken pipes,
  process-exit races, invalid handles, and cleanup failures.

### Dynamic analysis

- Address, leak, and thread sanitizers where supported.
- Platform-equivalent memory, handle, race, and process diagnostics.
- Miri or an equivalent tool for isolated unsafe Rust components where
  applicable.
- Assertions for descriptor, handle, worker, child-process, and memory
  reclamation.

### Stress and performance

- Multi-gigabyte integrity runs in both directions.
- Concurrency saturation and fairness tests.
- Multi-day soak tests.
- Reproducible benchmarks with stored baselines and profiles.

Flaky tests must be treated as evidence of an unresolved race unless a genuine
external limitation is isolated and documented. Required tests must not be
silenced to obtain a passing result.

All changed Dart files must be formatted. Dart analysis must run with fatal
infos and warnings. Affected Dart and native tests, formatters, linters,
documentation checks, sanitizers, and benchmarks must pass according to the
risk and platforms touched.

## Capability completeness

A maintained capability matrix must compare `ptyx` with leading PTY packages,
mature terminal PTY implementations, and native OS facilities. It covers at
least:

- spawn and executable lookup;
- arguments, environment, and working directory;
- terminal size and resize;
- byte input and output;
- buffering, synchronous acceptance, backpressure, and terminal input failure;
- exit, signals, process groups, and job cleanup;
- terminal metadata and modes;
- errors, cancellation, and isolate loss;
- platform and architecture constraints.

Each relevant capability is classified as:

- supported uniformly;
- supported through an explicit platform capability;
- deferred with a recorded reason;
- rejected as outside the product boundary.

The package does not copy a competitor API method for method. A capability is
included when real PTY workloads require it and the full quality cost can be
met.

Representative usability exercises must validate an interactive shell, an IDE
terminal, TTY-dependent automation, a high-volume process, a many-session
service, and a hostile or stuck child. The safe, bounded, high-performance path
must be the natural result of following the primary documentation.

## Completion criteria

The standard is satisfied only when:

- every advertised platform and architecture passes runtime qualification;
- public semantics and lifecycle races are specified and tested;
- no known critical correctness, safety, deadlock, leak, data-loss, or
  cleanup defect remains;
- resource bounds hold under pause, cancellation, child stalls, floods,
  failure, and shutdown;
- the required static, dynamic, fuzz, fault-injection, stress, and soak
  verification passes;
- benchmark targets are met without weakening correctness or safety;
- the capability matrix has no unexplained gap for the defined workloads;
- public API documentation, examples, platform constraints, security limits,
  and error behavior are complete;
- native artifacts and build selection are deterministic, integrity-verified,
  and suitable for independent package publication.

Passing unit tests or compiling on a target is necessary but never sufficient.
