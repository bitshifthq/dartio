# ptyx benchmark methodology

Benchmarks measure the public Dart API and retain direct-native candidate
evidence separately. Correctness gates run first; a fast result with loss,
unbounded memory, starvation, or incomplete cleanup is invalid.

`benchmark/scorecard.dart` implements the public-boundary correctness,
throughput, latency, saturation, fairness, observation, cleanup, and resource
workloads below. Its JSON still records `acceptance_result: false` and the
remaining evidence gaps. It must not be described as a production pass until
the retained multi-gigabyte run, equivalent comparisons, and allocation/copy
instrumentation are complete.

## Reproduction

From `packages/ptyx`, build the release broker and controller, then run:

```sh
cargo build --manifest-path native/broker/Cargo.toml --release
PTYX_BROKER_BINARY="$PWD/native/target/release/ptyx-broker" \
  cargo build --manifest-path native/dart/Cargo.toml --release
dart run benchmark/scorecard.dart all
dart run benchmark/scorecard.dart integrity --integrity-bytes=2147483648
```

The base-compatible harness uses the same shell children and timer boundaries
with both the retained API and the current API:

```sh
dart run benchmark/base_scorecard.dart --bytes=33554432 --repetitions=5
```

Direct-native candidate runs can be retained with an exact dynamic-library
hash:

```sh
dart run benchmark/candidates/scorecard.dart direct-rust \
  benchmark/candidates/rust/target/release/libptyx_candidate_rust.dylib \
  --bytes=33554432 --repetitions=5 --output=benchmark/results/direct.json
```

For a repeated integrity and cleanup run, pass the duration in seconds:

```sh
dart run tool/soak.dart 86400 --output=soak-24h.json
```

The soak checks every byte on every spawn/write/exit/close cycle and
samples the complete Dart/controller/broker/child process tree during steady
state. It waits up to three seconds for process, thread, and descriptor or
handle counts to return to their warmed baseline. Peak RSS growth is limited
to 64 MiB with the AOT fixture used in qualification; source-mode diagnostics
allow 256 MiB for the additional fixture Dart VM. Post-cleanup RSS may retain
at most 32 MiB. All budgets and pass/fail decisions are recorded in JSON
together with exact fixture hashes, revision, full dirty-tree fingerprint,
platform, architecture, Dart version, and UTC bounds. The scheduled hosted job
runs for five hours and forty-five minutes because a GitHub-hosted job cannot
provide a multi-day execution window. Multi-day completion is retained only
from a local or suitably self-hosted command that actually runs for the
recorded duration.

The standalone ABI smoke harness verifies the ABI version and every
authoritative header symbol in a built artifact:

```sh
dart run tool/verify_abi.dart native/target/release/libptyx_c.dylib
```

The scorecard refuses `--output` retention from a dirty tree. The
`--allow-dirty` override exists only for explicitly marked diagnostic work and
records the dirty state plus a fingerprint of tracked binary diffs and every
untracked file.

Set `PTYX_BROKER_BINARY` when the build is cross-targeted or the materialized
broker is not the host artifact. Run benchmarks on an otherwise idle host,
record the repository commit, OS build, architecture, Dart version, compiler
versions, CPU/power state, workload parameters, warmups, repetitions, and raw
samples. Store retained results under `benchmark/results`.

The candidate scorecard and source under `benchmark/candidates` are selection
evidence, not the production API benchmark. Their synchronous ABI is
intentionally insufficient for package semantics. Candidate B's retained
performance and corrected correctness evidence came from different artifacts,
so the performance selection gate remains unresolved until the corrected
artifact is rerun.

## Retained macOS x64 evidence

The retained base-compatible runs use identical shell children, payloads,
terminal setup, and byte verification. The historical base output timer starts
immediately before its one-byte gate write; the current timer starts
immediately after that write. The difference is one bounded API write against
roughly 0.3-second output samples, but means the historical results are
directional regression evidence rather than an exactly identical timer
boundary. At 32 MiB, the current implementation's medians are:

| Workload | Base `4c6120c` | Current `2336697` | Change |
|---|---:|---:|---:|
| Interactive p99 | 1,675 us | 383 us | 77.1% lower |
| Output | 89.289 MiB/s | 99.062 MiB/s | 10.9% higher |
| Input | 4.577 MiB/s | 5.706 MiB/s | 24.7% higher |
| Spawn/close p99 | 79,076 us | 12,744 us | 83.9% lower |

The base output distribution includes the retained 56.058 MiB/s outlier; the
table reports medians rather than removing it. The raw samples and host
metadata are in `benchmark/results/base-macos-x64-4c6120c.json` and
`benchmark/results/current-base-compatible-macos-x64-2336697.json`.

The stricter equivalent-child 128 MiB comparison against the minimal blocking
direct-Rust candidate produced a median 97.195 MiB/s through the production
Dart stream/native-port/credit boundary and 118.006 MiB/s through the
synchronous candidate ABI: 82.36%. The bounded multi-chunk pipeline improved
the production median from a same-session single-flight diagnostic of
74.586 MiB/s, a 30.3% gain. Production interactive p99 was 335 microseconds in
the complete scorecard, below the two-millisecond added-latency limit, while
the direct candidate ranged from 34 to 42 microseconds. The exact retained
samples are in
`current-128m-base-compatible-macos-x64-61c0f76.json` and
`direct-rust-128m-macos-x64-55da5a3.json`.

That retained raw output result did **not** clear the nominal 90%
direct-throughput gate. A follow-up found and removed a redundant Rust copy
when a complete queued read already matched the delivery batch, without
changing the bounded output budget, credit contract, or interactive batching.
On clean exact revision `7849136`, production reached a 95.401 MiB/s median
versus a 113.795 MiB/s direct median, or 83.84%.

The direct candidate omits the asynchronous Dart stream contract, bounded
output ownership, native-port delivery, and credit return. Under the
equivalence rule in `doc/architecture/candidate-gates.md`, it is a useful
ceiling rather than an equivalent baseline: it cannot clear the gate, but its
different boundary also cannot disqualify the production architecture. No
equivalent direct-boundary result currently clears the qualification gap.

Single-flight and larger-batch variants were profiled and rejected. Bounded
pipelining was retained because it improved sustained output without weakening
pause, cancellation, or memory bounds. External typed-data delivery remains
rejected because its finalizer lifetime would outlive output credit and weaken
the process-shutdown ownership proof. Release acceptance still requires an
equivalent performance comparison and a complete exact-revision evidence
bundle.

A dirty-tree AOT-fixture diagnostic exercised the complete scorecard after the
isolate-owner, pipelining, and resource-accounting changes. Interactive p99
was 335 microseconds, 16-session output fairness had a slowest-to-fastest ratio
of 0.998, 100 idle sessions returned from 636 to 36 descriptors and from 102
to 2 processes, and forced descendant cleanup returned from 57 to 36
descriptors. A 300-second AOT soak completed 2,382 exact cycles, held its
steady descriptor count at 47, limited sampled RSS growth to 25,739,264 bytes,
and returned to the 35-descriptor/2-process baseline in 356 milliseconds.
These are diagnostic results in
`current-all-8m-diagnostic-macos-x64.json` and
`soak-300s-resource-gated-diagnostic-macos-x64.json`, not release acceptance.

## Release evidence gate

The release workflow is fail-closed. Acceptance is an external Actions
artifact named `ptyx-release-acceptance-<exact commit SHA>`; keeping it outside
the commit avoids an impossible self-referential commit hash. The
`ptyx-acceptance` workflow runs on a qualification self-hosted runner, verifies
the provisioned bundle for the checked-out SHA, and retains it. Release then
downloads only an unexpired artifact whose workflow run has the tag's exact
SHA.

`tool/verify_release_evidence.dart` requires six advertised runtime targets,
at least 90% direct output throughput, four exact 2 GiB integrity directions,
a real 48-hour soak, sanitizer results, fuzzing, fault injection, and a
publication dry run. A platform-ceiling exception is not self-attestable and
therefore cannot bypass the automated release gate. Schema 2 binds every
claim to a relative evidence file and SHA-256 digest. The verifier opens and
hashes every file, rejects paths and symlinks outside the bundle, and requires
each category to contain a clean, passing, exact-revision structured result.
It independently checks the integrity byte counts and exit codes and
recomputes the soak cleanup and sampled steady-state RSS gates from the
recorded snapshots against fixed 64 MiB steady-state and 32 MiB cleanup
budgets. The bundle must also include the production AOT fixture and its Dart
source; the verifier hashes both files and binds those digests to the soak
record. The soak records its sampling interval explicitly; this is a sampled
steady-state bound, not an operating-system high-water RSS claim.
The manifest is intentionally absent while any requirement remains open; a
diagnostic scorecard can never authorize a release merely by exiting
successfully.

## Required scorecard

The production scorecard covers:

- spawn latency and teardown for 1, 10, and 100 sessions;
- idle CPU, resident memory, native thread count, descriptors or handles, and
  Dart isolate count at the same concurrency levels;
- sustained output, sustained input, and simultaneous bidirectional traffic;
- small interactive writes and first-byte latency;
- output pause/resume and explicit discard;
- bounded-input saturation, rejection latency, and recovery after drain;
- mixed quiet and noisy sessions, fairness, and quiet-session tail latency;
- resize and mode-observation overhead;
- normal exit, forced close, descendant cleanup, and resource return.

Byte counts are checked at both ends. Memory is sampled through steady-state
and saturation, not only before and after. CPU includes the Dart process and
Unix broker. Latency reports distributions including p50, p95, and p99 rather
than only an average. Scaling results retain each session's progress so
aggregate throughput cannot conceal starvation.

## Regression policy

Compare the same workload and environment to the retained base commit and the
last accepted production result. A stable regression above five percent in
throughput, latency, CPU, memory, allocation, copying, fairness, or scaling is
investigated and fixed unless an explicit correctness gain justifies and
documents the cost. No claim is made for a target that only cross-compiled;
runtime evidence must come from that operating system and architecture.
