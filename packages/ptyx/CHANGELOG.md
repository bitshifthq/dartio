# Changelog

## 0.0.1

- Added asynchronous native PTY session creation with a staged publication
  barrier.
- Added bounded, lossless input and output with capacity waits, flush, explicit
  output discard, and typed terminal input failure.
- Added environment modes, working directories, resize, signaling, metadata,
  mode observation, native capabilities, and configurable graceful close.
- Added shared native reactors, a hardened Unix spawn/reaping broker, and
  generation-checked session ownership.
- Added generated ABI bindings, lifecycle documentation, runtime contract
  tests, and a reproducible production scorecard.
