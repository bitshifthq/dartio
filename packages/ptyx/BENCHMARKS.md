# ptyx benchmark methodology

Benchmarks measure the public Dart API and retain direct-native candidate
evidence separately. Correctness gates run first; a fast result with loss,
unbounded memory, starvation, or incomplete cleanup is invalid.

`benchmark/scorecard.dart` is currently a diagnostic subset of the acceptance
protocol below. Its JSON explicitly records `acceptance_result: false` and the
missing workloads. It must not be described as a production pass until every
required metric and equivalent comparison is implemented.

## Reproduction

From `packages/ptyx`, build the release broker and controller, then run:

```sh
cargo build --manifest-path native/broker/Cargo.toml --release
PTYX_BROKER_BINARY="$PWD/native/broker/target/release/ptyx-broker" \
  cargo build --manifest-path native/Cargo.toml --release
dart run benchmark/scorecard.dart all
```

For a repeated integrity and cleanup run, pass the duration in seconds:

```sh
dart run tool/soak.dart 86400
```

The soak checks every byte on every spawn/write/flush/exit/close cycle and
reports verified bytes, cycle count, and process RSS. Multi-day completion is
retained only when the command actually runs for that duration.

The standalone ABI smoke harness verifies the ABI version and every
authoritative header symbol in a built artifact:

```sh
dart run tool/verify_abi.dart native/target/release/libptyx.dylib
```

The scorecard refuses `--output` retention from a dirty tree. The
`--allow-dirty` override exists only for explicitly marked diagnostic work and
records the dirty state and a status hash.

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

## Required scorecard

The production scorecard covers:

- spawn latency and teardown for 1, 10, and 100 sessions;
- idle CPU, resident memory, native thread count, descriptors or handles, and
  Dart isolate count at the same concurrency levels;
- sustained output, sustained input, and simultaneous bidirectional traffic;
- small interactive writes and first-byte latency;
- output pause/resume and explicit discard;
- bounded-input saturation, capacity wake latency, and flush;
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
