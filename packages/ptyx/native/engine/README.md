# ptyx-engine

`ptyx-engine` is the implementation crate shared by the public `ptyx` Rust API
and its language-neutral C adapter. It owns platform process, pseudo-terminal,
readiness, backpressure, and cleanup mechanics.

Applications should depend on `ptyx`, not this crate. `ptyx-engine` is
published only so the independently publishable public crate and native
adapter can use the exact same implementation; its API is not a supported
application-level compatibility boundary.
