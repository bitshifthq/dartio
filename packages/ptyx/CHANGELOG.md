# Changelog

## 0.0.1

- Replaced synchronous construction with asynchronous native PTY session
  creation and a staged publication barrier.
- Replaced unbounded synchronous input with bounded `tryWrite`, asynchronous
  `write`, capacity waiting, `flush`, and `inputDone`.
- Added bounded, lossless input and output with capacity waits, flush, explicit
  output discard, and typed terminal input failure.
- Added typed `exitStatus` while retaining `exitCode`, including complete
  unsigned 32-bit Windows process exit codes.
- Added environment modes, working directories, resize, signaling, metadata,
  mode observation, explicit native capabilities, and configurable graceful
  close.
- Added stable typed operational errors with operation, category, optional
  native status, and safe context.
- Added shared native reactors, a hardened Unix spawn/reaping broker, and
  generation-checked session ownership.
- Added ABI version 4 generated bindings, Dart finalization and native
  quiet-port liveness probing for isolate loss, lifecycle and security
  documentation, ABI/runtime/fault contract tests, a partial diagnostic
  scorecard, and an extended fuzz/soak workflow.
