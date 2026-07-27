# ptyx Best-in-Class Execution Specification

## Authority

This specification turns the requirements in
[`QUALITY_STANDARD.md`](QUALITY_STANDARD.md) into an executable engineering
program.

The agent must read and follow, in order:

1. the active user prompt and repository `AGENTS.md`;
2. `REPOSITORY.md`;
3. `packages/ptyx/QUALITY_STANDARD.md`;
4. this specification;
5. package source, tests, build hooks, workflows, and public documentation.

The strictest applicable requirement governs. The quality standard defines the
destination. This specification defines how to investigate, choose, implement,
and prove a solution. Existing code is evidence, not an architectural mandate.

## Objective

Deliver a production-quality candidate for `ptyx` that satisfies the quality
standard as completely as the available platforms and tooling can prove.

The result must be a safe, correct, lossless, bounded, high-throughput,
low-latency, CPU-efficient, memory-efficient, cross-platform PTY package with
an idiomatic Dart API and native-optimal platform implementations.

The agent must:

- audit the complete Dart, C ABI, native, build, test, and release path;
- specify every public semantic and lifecycle race before relying on it;
- establish reproducible correctness and performance baselines;
- investigate competing designs instead of assuming the existing architecture
  is optimal;
- prototype representative alternatives when evidence is insufficient;
- choose the best complete design using the decision hierarchy;
- rearchitect or rewrite package code when that is the best supported choice;
- implement through tests and measurable vertical slices;
- build the permanent verification and benchmark infrastructure;
- validate every available platform and report unavailable evidence precisely;
- leave a coherent, maintainable package rather than disconnected experiments.

The task is implementation work. An audit, proposal, or partial refactor alone
does not satisfy it.

## Branch and repository protocol

All task work must occur on the dedicated branch:

```text
codex/ptyx-best-in-class
```

Before changing implementation files, the agent must:

1. inspect the active branch and `git status`;
2. preserve every pre-existing tracked and untracked change;
3. create the task branch from local `main` when it does not exist;
4. resume the existing task branch when it does exist;
5. record the base commit used for baseline comparisons.

The specification documents may begin as user-owned untracked files. They must
be carried onto the task branch and committed there. The agent must not clean,
stash, reset, overwrite, or relocate unrelated user work.

The agent may create logical commits and push this branch normally to run
cross-platform CI. It may dispatch non-release workflows against this branch
and inspect their results. It must not:

- work directly on `main`;
- force-push;
- merge;
- open or update a pull request unless separately authorized;
- create tags or releases;
- publish to pub.dev;
- run a release workflow;
- alter repository or branch protections;
- expose secrets or private benchmark data.

Rejected experiments belong in temporary directories or focused commits that
do not remain in the final branch history. Destructive replacement of the
working implementation is permitted only after a recoverable commit and
evidence that the selected replacement covers the required behavior.

## Scope

The agent may modify:

- `packages/ptyx/**`;
- ptyx-specific workflows and scripts under `.github/`;
- shared lint, workspace, and tooling configuration only where the package
  requires it and no other package contract is weakened.

The agent may:

- redesign the public Dart API;
- change the internal C ABI and regenerate bindings from authoritative inputs;
- rearchitect or replace native modules;
- replace, patch, fork, or remove portable-pty;
- add focused Dart, Rust, C, Zig, benchmark, fuzzing, or test dependencies;
- add platform-specific native backends;
- change build hooks and artifact selection;
- add verification and benchmark tooling;
- remove superseded ptyx code after replacement is proven.

The agent must keep changes package-focused. It must not refactor unrelated
repository code, introduce dependencies between independently publishable
packages, or add terminal emulation, UI, SSH, shell parsing, or sandboxing.

The package has no published compatibility obligation that justifies retaining
a defective contract. Public API changes must still be deliberate,
documented, tested, and chosen as though they will become the long-term 1.0
shape. Cosmetic churn is not authorized.

## Autonomy and decision policy

The user may be unavailable throughout execution. The agent must make safe,
evidence-backed decisions and continue working without waiting for preferences
that the quality standard already resolves.

The agent must not ask the user to choose:

- Rust versus Zig;
- portable-pty versus direct native backends;
- workers versus reactors;
- exact internal buffer sizes;
- a convenient but weaker correctness contract.

Those are engineering decisions. The agent must investigate and choose them.

User input is required only when:

- completing the task needs authority outside this specification;
- an external credential or protected resource is unavailable;
- two choices change the product boundary rather than its implementation;
- an irreversible external action would be required.

If an environment lacks a required target, tool, or approval, the agent must
complete every independent task, prepare the missing validation, record the
exact command and expected evidence, and continue elsewhere. Missing evidence
must never be converted into a passing claim.

## Sub-agent review protocol

The primary agent owns the plan, implementation, integration, and final
decision. It must use sub-agents as independent reviewers and investigators,
not as a substitute for understanding the code.

Sub-agents should perform bounded, read-heavy work in parallel. Concurrent
write-heavy work in the same files is prohibited. A reviewer must receive the
quality standard, this specification, the candidate or final diff, its claimed
invariants, relevant tests, and benchmark evidence.

### Candidate review gates

Each architecture candidate that reaches a representative prototype must be
reviewed independently before selection:

- one sub-agent reviews correctness, ownership, lifecycle, and race behavior;
- one sub-agent reviews performance design, benchmark fairness, and scaling;
- one sub-agent reviews platform feasibility, build integration, and
  maintainability.

The primary agent must wait for all candidate reviews. It must either address
each finding in the candidate or record a concrete evidence-backed reason that
the finding does not apply. Candidate selection must include the review
results, not only the primary agent's measurements.

A candidate may be rejected before implementation review when a documented
feasibility result proves that it cannot meet a non-negotiable requirement.
The primary agent must not build complete alternatives merely to satisfy a
review count.

### Selected implementation review gates

After the selected design is integrated and its primary tests pass, use
separate sub-agents to review:

1. Dart API semantics, usability, errors, and documentation;
2. C ABI, FFI ownership, unsafe code, memory safety, and security;
3. native process lifecycle, concurrency, cancellation, cleanup, and platform
   behavior;
4. benchmark methodology, profiles, throughput, latency, CPU, memory,
   fairness, and regression claims;
5. test completeness, fault injection, fuzzing, sanitizers, build hooks, CI,
   and platform qualification.

Reviews may run in waves when concurrency is limited. The primary agent must
wait for every review, consolidate overlapping findings, fix all valid
findings, and rerun the affected checks.

Material fixes made after review require a focused re-review of the affected
area. Before final completion, at least one independent sub-agent must compare
the final branch against the base commit and the complete definition of done.

Reviewers must lead with concrete findings, severity, file or symbol
references, reproduction or failure reasoning, and missing evidence. They
must not approve through absence of inspection, focus on style-only comments,
or repeat benchmark claims without checking the measurement boundary.

The final report must list every sub-agent review, its scope, its findings, and
how each finding was resolved. The primary agent may not declare completion
while a valid critical or high-severity review finding remains.

## Initial audit

The audit must cover every file in `packages/ptyx`, every ptyx workflow, and
all shared configuration that affects the package. It must trace public calls
from Dart through generated bindings and the C ABI to OS resources, then trace
messages, failures, and cleanup back to Dart.

The audit must verify or disprove these starting observations:

- the package README contains no user guidance;
- Windows end-to-end session tests are excluded;
- Android and several non-host architectures receive compilation without
  runtime qualification;
- process spawn crosses a synchronous FFI boundary;
- asynchronous write failures share the output error path;
- signal delivery can target only the direct child;
- terminal modes use fixed-interval polling;
- active sessions may create several dedicated native workers;
- input and output bounds permit a large per-session memory footprint;
- no committed competitor benchmark, direct-native baseline, fuzz target,
  standalone ABI harness, fault-injection layer, or multi-day soak harness
  proves the quality standard;
- cross-platform lifecycle and error semantics remain partly implicit in code.

The audit must also inspect:

- every unsafe block and foreign layout;
- ownership transfer for native and external typed-data buffers;
- all possible Dart port closure and isolate-loss paths;
- process spawn after a multithreaded runtime, including fork safety;
- descriptor and handle inheritance;
- child reaping and PID reuse;
- Windows argument quoting, environment requirements, ConPTY ownership, and
  job behavior;
- Unix session, controlling-terminal, foreground-group, signal, and EOF
  behavior;
- build-hook determinism, fallback builds, hashes, cross-compilation, and
  artifact mismatch handling;
- dependency licenses, security advisories, and maintenance risk.

Findings must be classified by severity and tied to a requirement, test, or
benchmark. Do not perform drive-by cleanup during discovery.

## Required design artifacts

Implementation must be guided by concise, maintained design artifacts under
`packages/ptyx`. The final file organization may combine closely related
material, but it must preserve all of the following information:

### Public contract

Document:

- session and sub-resource states;
- operation ordering and linearization points;
- spawn completion;
- write acceptance, capacity, flush, and failure;
- output listen, pause, resume, cancel, EOF, and errors;
- child exit versus trailing output;
- signal, resize, and metadata behavior;
- graceful and forced close;
- capability and unsupported behavior;
- isolate ownership and isolate loss;
- every public exception category.

### Lifecycle state model

Provide a state machine and race table for at least:

- spawn success, failure, and partial failure;
- write versus close and child exit;
- signal versus exit and reaping;
- resize versus close;
- output EOF versus child exit;
- normal EOF versus native failure;
- pause or cancel versus close;
- repeated or concurrent close;
- Dart native-port closure;
- worker or reactor failure.

State transitions must name resource ownership and the event that linearizes
each transition.

### Native architecture

Document:

- selected language and why;
- selected platform backends;
- process and terminal ownership;
- reader, writer, reaper, mode observer, and timer topology;
- queue and backpressure algorithms;
- Dart messaging and buffer ownership;
- error propagation;
- shutdown and failure containment;
- unsafe invariants;
- rejected alternatives and the evidence that rejected them.

### Platform contract

For each OS and architecture, record:

- native PTY facility;
- spawn and executable lookup behavior;
- argument and environment behavior;
- process group or job model;
- signal and termination behavior;
- exit-status behavior;
- resize behavior;
- terminal metadata and mode capabilities;
- I/O cancellation strategy;
- runtime and cross-compilation evidence;
- known OS limitations.

### Benchmark protocol

Document:

- benchmark workloads;
- direct-native and competitor baselines;
- equivalent measurement boundaries;
- hardware and software metadata;
- build modes;
- warmup, repetitions, statistics, and noise controls;
- throughput, latency, CPU, memory, copy, allocation, worker, and handle
  metrics;
- regression thresholds;
- how to reproduce results.

## Investigation and experiment protocol

Architecture changes must follow evidence rather than preference.

### Baseline

Before material implementation changes:

- run every available Dart and native check;
- record failures without weakening checks;
- run representative correctness, throughput, latency, memory, CPU, and
  session-scaling workloads against the base commit;
- use release-mode native artifacts for performance;
- retain exact commands, configuration, host details, and raw results;
- profile enough of each workload to identify dominant costs.

Baseline comparisons must use an isolated worktree or other non-destructive
method. The agent must not reset the active task worktree to collect them.

### Candidate designs

At minimum, evaluate:

1. a corrected and optimized Rust design retaining useful portable-pty
   facilities;
2. Rust with direct platform backends where portable-pty obstructs the
   contract;
3. a focused Zig prototype when it could materially improve the full
   scorecard.

This does not require three complete implementations. Each candidate needs a
representative vertical slice sufficient to test its important claims. The
slice must include the expensive and risky boundaries, not only an in-memory
queue microbenchmark.

Representative slices include:

- spawn and failure-atomic cleanup;
- sustained PTY read and write;
- Dart message or external-data transfer;
- output backpressure;
- process exit and forced close;
- Windows ConPTY behavior when the candidate claims Windows advantages.

### Selection

Choose the design using this order:

1. ability to meet truthful semantics and memory and process safety;
2. deterministic cleanup and race behavior;
3. supported-platform feasibility;
4. sustained throughput and tail latency;
5. CPU, memory, copies, allocations, and scaling;
6. build, diagnostic, sanitizer, fuzzing, and cross-compilation quality;
7. maintainability and dependency risk.

A faster candidate loses if it weakens byte integrity, boundedness, cleanup,
platform support, or verifiability. A familiar candidate loses if another
design demonstrates a material complete advantage.

The decision record must contain measured results and known uncertainty. It
must not justify a choice with language preference.

### External references

Use primary source code, operating-system documentation, official Dart
documentation, and authoritative dependency documentation. Ghostty's PTY path
is a performance and correctness reference, not a template to copy blindly.
Other useful comparisons include direct OS facilities, portable-pty,
node-pty, and mature terminal implementations.

Record upstream repository revision identifiers for behavior or benchmark
comparisons. If code or a non-obvious algorithm is adapted, verify license
compatibility and place attribution beside the adapted implementation as
required by the project documentation rules.

## Implementation requirements

### Dart API

The Dart API must:

- make spawn asynchronous;
- keep bounded fast writes available without hiding partial acceptance;
- expose awaitable capacity and input flush;
- separate input failure, output failure, exit failure, and close failure;
- expose capabilities instead of ambiguous `null` or fabricated equivalence;
- make drain-and-discard explicit and easy;
- preserve raw bytes without encoding or line-ending assumptions;
- define stream subscription behavior;
- provide idempotent asynchronous close;
- prevent accidental use after close;
- remain clear under normal Dart cancellation and error handling;
- document every non-trivial API with a short example.

The API should minimize allocation and futures on hot paths without moving
unbounded or blocking work onto the Dart isolate.

### Interoperability boundary

The C ABI and Dart message protocol must:

- use authoritative declarations and generated bindings;
- verify compatible artifacts before creating a session;
- validate pointers, lengths, counts, flags, tags, and integer conversions;
- define ownership before and after every fallible call;
- prevent use-after-free, double free, stale handle use, and late-message use;
- contain panic and foreign exceptions;
- preserve native error category and OS error data;
- handle Dart port closure and failed message posting;
- make external typed-data finalization safe during shutdown and isolate loss;
- have layout and calling-convention assertions on every supported target.

Internal ABI compatibility may change with the Dart artifact. Mismatch
detection must fail early and safely.

### Native process model

Unix implementations must verify:

- PTY allocation and slave setup;
- session and controlling-terminal creation;
- standard stream duplication;
- close-on-exec and closure of unrelated descriptors;
- signal disposition and mask handling;
- async-signal safety between fork and exec where fork is used;
- executable lookup and exec error reporting;
- foreground process-group signaling;
- exact-once reaping;
- `EINTR`, `EAGAIN`, PTY `EIO`, EOF, and hangup behavior;
- resize and `SIGWINCH`;
- bounded termination escalation.

Windows implementations must verify:

- ConPTY availability and supported OS requirements;
- pipe and pseudoconsole ownership;
- `STARTUPINFOEX` and attribute-list lifetime;
- exact command-line quoting;
- environment block construction and required system entries;
- working directory and executable lookup;
- process, thread, pseudoconsole, and pipe handle closure;
- job-object behavior and its interaction with ConPTY;
- overlapped or blocking I/O cancellation;
- exit-code observation and bounded forced termination;
- arm64 behavior, not only x64 compilation.

All platforms must prevent a failed spawn from leaving a child, job,
descriptor, handle, worker, buffer, or port behind.

### I/O and backpressure

The implementation must:

- preserve byte order and exact content;
- bound every native and Dart-side queue;
- account for bytes across native buffers, posted messages, Dart queues, and
  external typed data;
- reserve and release accounting without underflow, overflow, or lost wakeup;
- handle partial reads and writes correctly;
- avoid busy waits and polling where readiness notification exists;
- stop or throttle the producer when the consumer pauses;
- drain and discard only after explicit cancellation or shutdown semantics;
- provide cross-session fairness;
- avoid one future, allocation, lock, or system call per byte-sized operation
  when batching can preserve semantics;
- prove finalizer, shutdown, and acknowledgment races.

Default budgets must be conservative for the 100-session workload. Advanced
budgets must have safe minima, maxima, and overflow checks.

### Terminal modes

Mode snapshots must represent only fields the platform reports. Mode-change
observation must:

- activate only with an observer;
- avoid claiming lossless transient detection;
- use reliable event notification where available and safe;
- use adaptive, measured polling otherwise;
- preserve output framing if native packet modes are investigated;
- surface observation failure independently;
- stop promptly during close;
- have rapid-transition and idle-CPU tests.

### Build and distribution

Build hooks and native artifacts must:

- select the correct OS and architecture deterministically;
- reject incompatible or corrupted artifacts;
- use authoritative source inputs;
- produce reproducible release-mode binaries as far as toolchains permit;
- support source-build fallback deliberately;
- validate Cargo, Zig, C compiler, Dart SDK, NDK, linker, and archiver
  discovery;
- avoid host-only assumptions in cross-compilation;
- keep generated hashes and release metadata consistent;
- remain suitable for `dart pub publish --dry-run`.

A language or dependency change must update every build, CI, release, and
source-distribution path before it is accepted.

## Verification program

Use the project skills `/tdd`, `/dart`, `/test`, and `/doc` whenever their
scope applies. Tests must drive behavior and reproduce defects before fixes
where practical.

### Required test classes

Implement and run:

- Dart public-contract tests;
- native unit and integration tests;
- a standalone C ABI harness;
- generated-binding agreement checks;
- property-based byte, conversion, and lifecycle tests;
- state-model race tests;
- fuzz targets for ABI conversion and message decoding;
- deterministic fault injection;
- descriptor, handle, process, worker, and memory leak tests;
- concurrency and fairness tests;
- multi-gigabyte exact-integrity tests;
- long-running soak tests;
- benchmark regression tests.

### Required fault coverage

Inject or deterministically simulate:

- allocation failure;
- worker, reactor, and timer creation failure;
- Dart API initialization and message-post failure;
- partial read and write;
- interruption and temporary unavailability;
- broken pipe, hangup, EOF, and platform-native errors;
- input queue exhaustion;
- output acknowledgment and finalizer races;
- child exit during every public operation;
- port closure and isolate loss;
- graceful termination refusal;
- force-kill failure;
- wait and reap failure;
- cleanup after partial spawn;
- invalid and stale ABI handles.

### Dynamic tooling

Run the strongest applicable combination of:

- Rust formatting, Clippy with warnings denied, tests, and docs;
- AddressSanitizer;
- LeakSanitizer;
- ThreadSanitizer;
- Miri for isolated unsafe components;
- macOS allocation, leak, handle, and process diagnostics;
- Windows sanitizer and handle diagnostics;
- fuzzers with retained regression corpora.

Unsupported tool and target combinations must be documented with the
platform-equivalent check used instead.

### Cross-platform CI

CI must distinguish:

- host runtime tests;
- architecture runtime tests;
- cross-target compilation;
- sanitizer or diagnostic runs;
- stress and benchmark jobs;
- extended scheduled soak and fuzz jobs.

Linux, macOS, and Windows x64 and arm64 need real runtime evidence for a
production claim. Android armv7, arm64, and x64 need device or representative
emulator evidence. Cross-compilation never substitutes for runtime evidence.

The agent may push only the task branch and manually dispatch non-release
workflows. It must monitor results, inspect full failures, fix every in-scope
failure, and rerun until green. A runner unavailable to the repository remains
an explicit qualification gap.

### Mandatory local commands

At minimum, run the repository-prescribed commands and their native
equivalents:

```text
dart format <changed Dart files>
dart analyze --fatal-infos --fatal-warnings
dart test
cargo fmt --manifest-path packages/ptyx/native/Cargo.toml -- --check
cargo clippy --manifest-path packages/ptyx/native/Cargo.toml \
  --all-targets -- -D warnings
cargo test --manifest-path packages/ptyx/native/Cargo.toml
cargo doc --manifest-path packages/ptyx/native/Cargo.toml --no-deps
dart pub publish --dry-run
```

Adjust native commands if the selected architecture replaces Cargo, but do not
drop equivalent formatting, lint, test, documentation, and release-build
checks.

## Performance program

Performance work begins with correctness-enabled builds and exact integrity
checks. Benchmarks that omit backpressure, errors, cleanup, or Dart transfer
cost do not establish package performance.

### Required workloads

Measure:

- one-byte and small interactive round trips;
- mixed realistic terminal chunks;
- large one-way output;
- large one-way input;
- simultaneous bidirectional transfer;
- no listener, paused listener, resume, and explicit discard;
- queue saturation and capacity recovery;
- child exit with trailing output;
- repeated spawn and close;
- 1, 10, and 100 idle sessions;
- 1, 4, and at least 16 busy sessions;
- resize and mode-observation load;
- multi-gigabyte integrity;
- extended soak behavior.

### Required comparisons

Use:

- a minimal direct native PTY baseline;
- the base `ptyx` revision;
- the selected implementation;
- portable-pty where relevant;
- Ghostty's isolated PTY I/O path where a fair harness is possible;
- at least one other mature PTY package with equivalent semantics.

Do not compare a raw native loop with a Dart API and call the result
equivalent. Report each boundary included in the measurement.

### Required analysis

For every representative workload, collect:

- sustained and burst throughput;
- p50, p95, and p99 latency;
- CPU time and utilization;
- resident and peak memory;
- allocations and copies where measurable;
- system calls and wakeups;
- worker, descriptor, and handle counts;
- fairness across sessions.

Use repeated samples, report variance, and investigate regressions rather than
averaging them away. Profile before optimizing. Retain raw results and scripts
needed to reproduce conclusions.

The implementation must meet the quality standard's minimum envelope and aim
to lead the strongest comparable package on the complete scorecard. A 5%
regression requires a fix or an explicit evidence-backed tradeoff that does
not weaken a non-negotiable requirement.

## Documentation and usability

The final package documentation must include:

- a complete README with platform support, installation, first session,
  output consumption, input, resize, exit, close, errors, and security;
- examples for interactive use and intentional drain-and-discard;
- public API documentation for every non-trivial symbol;
- architecture, ownership, lifecycle, and unsafe invariants;
- platform capabilities and limitations;
- benchmark methodology and reproduction;
- build-from-source requirements;
- a changelog entry for public behavior and API changes.

Prose must follow the project `/doc` skill. Adapted behavior must be attributed
where it is implemented.

Usability must be exercised with:

- an interactive shell;
- an IDE-style terminal owner;
- TTY-dependent automation;
- a high-volume child;
- a service managing many sessions;
- a stuck or hostile child.

Following the primary example must produce safe cleanup, bounded memory, and
correct output consumption without hidden expert knowledge.

## Definition of done

The task is complete only when all of the following are true:

### Contract

- The public Dart contract satisfies `QUALITY_STANDARD.md`.
- Public ordering, completion, backpressure, error, and close semantics are
  documented and tested.
- The lifecycle state machine and race table match the implementation.
- Platform differences use explicit capabilities.
- No failure is silently lost or sent through an unrelated channel.

### Correctness and safety

- Exact byte integrity holds under multi-gigabyte bidirectional stress.
- All queues and posted data have measured, documented bounds.
- Spawn and partial-spawn failure are resource-atomic.
- Process and terminal job ownership are correct on each supported platform.
- Child exit, output drain, signal, and close races are deterministic.
- No known panic, use-after-free, stale handle, double free, double close,
  invalid message, PID-reuse signal, deadlock, or lost wakeup remains.
- Hostile-child behavior cannot corrupt the Dart process or wedge ptyx cleanup
  indefinitely within the documented OS boundary.
- Unsafe code has explicit invariants and targeted verification.

### Performance

- The minimum performance envelope in `QUALITY_STANDARD.md` is met.
- Results include throughput, latency, CPU, memory, allocation or copy,
  resource-count, and scaling evidence.
- The selected design is supported by fair baseline and competitor
  comparisons.
- No accepted optimization weakens correctness, safety, boundedness, fairness,
  or maintainability.

### Verification

- Dart format and analysis pass with fatal infos and warnings.
- All Dart, native, ABI, property, model, fault, stress, and applicable
  sanitizer tests pass.
- Fuzz targets run without unresolved findings and retain regression cases.
- Leak checks prove reclamation of memory, workers, descriptors, handles, and
  children.
- Runtime CI passes for every available required target.
- Cross-target builds pass for every declared architecture.
- Required unavailable runtime evidence is named as a qualification gap and is
  not described as passing.
- `dart pub publish --dry-run` succeeds without warnings.

### Maintainability and delivery

- The package README and public API documentation are complete.
- Architecture and benchmark decisions are reproducible.
- Generated files match authoritative inputs.
- Dependencies and licenses are reviewed.
- Build hooks and artifact selection remain deterministic and publishable.
- The task branch contains focused logical commits and no discarded
  experiment, benchmark artifact, secret, or unrelated change.
- The final report maps every quality-standard completion criterion to a file,
  test, command, benchmark, CI run, or explicit missing external evidence.

The agent must not mark a Codex goal complete while a required in-scope change
or available validation remains unfinished. An unavailable architecture runner
may prevent a full production-support claim, but it does not justify stopping
work that can proceed independently.

## Final report

The final handoff must lead with the achieved outcome and include:

- branch name and commit range;
- selected architecture and decisive evidence;
- public API and semantic changes;
- correctness and security findings fixed;
- benchmark results against the base and competitors;
- local validation commands and results;
- CI workflow runs and target matrix;
- remaining qualification gaps or risks;
- exact next action for any evidence that requires unavailable infrastructure.

Do not report a target, sanitizer, soak duration, benchmark, or fault case as
passing unless it ran and produced retained evidence.
