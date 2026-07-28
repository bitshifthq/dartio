# Changelog

## 0.0.1

- Replaced synchronous construction with asynchronous native PTY session
  creation and a staged publication barrier.
- Added a synchronous `write` acceptance API backed by bounded native queues,
  FIFO delivery, buffer ownership transfer, typed recoverable backpressure,
  and sticky terminal input failure. Removed the superseded input readiness,
  flush, and input-completion APIs.
- Added typed `exitStatus` while retaining `exitCode`, including complete
  unsigned 32-bit Windows process exit codes.
- Set the Windows runtime floor to build 26100, the first ConPTY implementation
  that satisfies the package's resource-cleanup contract.
- Added environment modes, working directories, resize, signaling, metadata,
  mode observation, explicit native capabilities, and configurable graceful
  close.
- Added stable typed operational errors with operation, category, optional
  native status, and safe context.
- Added shared native reactors, a hardened Unix spawn/reaping broker, and
  generation-checked session ownership.
- Added ABI version 6 generated bindings, staged-route publication, an
  individual-isolate owner supervisor, and guaranteed native finalization with
  post/finalizer serialization for isolate loss; added lifecycle and security
  documentation, ABI/runtime/fault contract tests, a
  correctness-gated diagnostic scorecard, reproducible comparison harnesses,
  and an extended fuzz/scorecard/soak workflow.
