# Changelog

## 0.0.1

- Replaced synchronous construction with asynchronous native PTY session
  creation and a staged publication barrier.
- Added a synchronous `write` acceptance API backed by bounded native queues,
  FIFO delivery, buffer ownership transfer, typed recoverable backpressure,
  and sticky terminal input failure. Removed the superseded input readiness,
  flush, and input-completion APIs.
- Made output subscription cancellation the only public drain-and-discard
  operation and removed the redundant session-level `discardOutput` method.
- Defined an independently reusable Rust crate, a stable language-neutral C
  ABI, and a thin Dart-over-C binding architecture.
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
- Added version 0.1 of the stable C ABI, generated private Dart bindings,
  atomic staged-route publication, and a private native adapter with an
  isolate-exit guardian and serialized finalization; added lifecycle and
  security documentation, ABI/runtime/fault contract tests, a
  correctness-gated diagnostic scorecard, reproducible comparison harnesses,
  and an extended fuzz/scorecard/soak workflow.
