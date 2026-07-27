# Native architecture candidate scorecard

These focused candidates answer one narrow selection question: after including
the Dart FFI crossing, copied input and output, a generation-checked registry,
PTY allocation, child spawn, sustained I/O, wait, and cleanup, does Zig provide
a material advantage over direct Rust?

Both dynamic libraries implement `include/ptyx_candidate.h`; `scorecard.dart`
runs identical child scripts, payloads, buffers, verification, warmup shape,
and repetitions against either library. The retained base package uses a
different asynchronous push boundary, so base-versus-candidate numbers are
reported as boundary decomposition, not an equivalent head-to-head result.

## Predeclared decision gates

A candidate result is rejected regardless of speed if exact bytes, exit,
cleanup, stale-handle rejection, or bounded resource behavior fails. A native
language replacement requires at least two material whole-package advantages
over an algorithmically equivalent implementation, no stable regression above
five percent, credible Linux/macOS/Windows x64 and arm64 paths, deterministic
build integration, and diagnostic coverage at least as actionable as the
current Rust toolchain.

The slice itself is rejected as production code because its FFI calls block,
spawn performs child setup after `fork`, backpressure is only the kernel PTY,
and no Windows backend is present. The source is retained because those
limitations make the selection evidence reviewable instead of silently
turning a benchmark prototype into architecture.

## Build and run on macOS

```text
cargo build --manifest-path benchmark/candidates/rust/Cargo.toml --release
zig build-lib benchmark/candidates/zig/candidate.zig -dynamic \
  -OReleaseSafe -lc -femit-bin=/tmp/libptyx_candidate_zig.dylib
dart run benchmark/candidates/scorecard.dart rust \
  benchmark/candidates/rust/target/release/libptyx_candidate_rust.dylib
dart run benchmark/candidates/scorecard.dart zig \
  /tmp/libptyx_candidate_zig.dylib
```
