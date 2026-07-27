# Architecture candidate gates

The retained base is commit
`4c6120c38dec4ac36a7fb349c274ae3123fe4224`. Candidate measurements use the
same child commands, payloads, Dart boundary, release mode, host, warmup, and
sample statistics recorded under `docs/evidence`.

Safety and semantics are rejection gates. A candidate is rejected when its
representative slice cannot provide:

- exact ordered bytes with bounded input, native output, posted Dart data, and
  Dart-side queues;
- all-or-reject writes, awaitable capacity, and a flush linearization point;
- independent input, output, exit, mode-observation, and cleanup failures;
- failure-atomic spawn and bounded, idempotent cleanup;
- safe behavior after Dart port loss, isolate loss, partial I/O, child exit,
  and output cancellation;
- a credible Linux, macOS, Windows x64, and Windows arm64 implementation path.

A qualifying candidate must then demonstrate all of the following on the
local macOS x64 host:

- no dedicated reader, writer, waiter, and mode thread per idle session;
- no more than 2 ms added one-byte p99 latency under normal host load;
- at least 90 percent of an equivalent direct-native PTY baseline for
  sustained input and output;
- bounded memory at 100 idle sessions and under paused output;
- materially fewer than the base input workload's roughly 72,000 context
  switches per 32 MiB;
- clean exit, trailing-output delivery, forced close, and descriptor
  reclamation under repeated runs.

Results with more than 5 percent variance are investigated and repeated.
Throughput gains are rejected when they increase an unbounded queue, weaken
cleanup, omit Dart transfer, or compare different transport boundaries.

## Candidate A: Rust with portable-pty facilities

This candidate may retain portable-pty for PTY allocation or platform spawn
facilities. It must replace the base per-session runtime topology with shared
readiness-driven I/O and explicit queue accounting. The slice must show spawn,
Dart transfer, sustained bidirectional I/O, pause backpressure, exit, and
cleanup. Retaining portable-pty is not sufficient evidence by itself.

## Candidate B: Rust with direct platform backends

This candidate uses direct Unix PTY and process primitives where portable-pty
cannot guarantee inheritance, foreground-group signaling, failure-atomic
spawn, or scalable readiness. Its Windows slice must model ConPTY pipe,
pseudoconsole, process, thread, attribute-list, and job ownership separately.
It wins over candidate A only when the complete contract or measured scorecard
improves enough to justify the additional platform code.

## Candidate C: focused Zig native core

The Zig slice uses the same ABI and workload boundaries as the Rust slices.
It must demonstrate a material whole-package advantage in safety review,
performance, binary and memory cost, cross-compilation, diagnostics, build
reliability, and maintainability. A smaller binary or one faster raw PTY loop
does not justify replacement. Missing sanitizer, fuzzing, package integration,
or Windows ownership evidence is a rejection result.
