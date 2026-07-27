# Direct Rust candidate

This crate is an experiment, not the production native core. It implements the
shared candidate ABI with direct Unix PTY and process calls so Dart-to-native
transfer, spawn, sustained I/O, wait, and cleanup can be compared with the Zig
slice under the same harness.

The experiment deliberately uses synchronous calls and a fixed `/bin/sh -c`
child. It cannot pass the production contract by itself. In particular, it
does not prove safe process creation after `fork` in a multithreaded host,
bounded asynchronous backpressure, Dart port-loss cleanup, or any Windows
behavior.
