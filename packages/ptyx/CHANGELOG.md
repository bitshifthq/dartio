# Changelog

## 0.0.1

- Replaced synchronous construction with asynchronous native PTY session
  creation and a staged publication barrier.
- Replaced unbounded synchronous input with ordered asynchronous `write`,
  `flush`, and `inputDone`.
- Added bounded, lossless input and output with internal capacity waiting,
  flush, explicit output discard, and typed terminal input failure.
- Added typed `exitStatus` while retaining `exitCode`, including complete
  unsigned 32-bit Windows process exit codes.
- Added environment modes, working directories, resize, signaling, metadata,
  mode observation, explicit native capabilities, and configurable graceful
  close.
- Added stable typed operational errors with operation, category, optional
  native status, and safe context.
- Added shared native reactors, a hardened Unix spawn/reaping broker, and
  generation-checked session ownership.
- Added ABI version 5 generated bindings, staged-route publication, an
  individual-isolate owner supervisor, and guaranteed native finalization with
  post/finalizer serialization for isolate loss; added lifecycle and security
  documentation, ABI/runtime/fault contract tests, a
  correctness-gated diagnostic scorecard, reproducible comparison harnesses,
  and an extended fuzz/scorecard/soak workflow.
