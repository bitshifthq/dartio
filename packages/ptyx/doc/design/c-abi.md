# C ABI contract

## Scope

The `ptyx` C ABI is a stable language-neutral interface to the Rust PTY
implementation. It is suitable for C, C++, and generated foreign-function
bindings. It has no Dart dependency and makes no assumption about an
application event loop.

The ABI exposes session operations and one blocking event consumer. It does
not expose reactor commands, queue internals, operating-system handles,
platform driver types, Dart ports, or test controls.

`include/ptyx.h` is authoritative. Generated bindings and ABI tests derive
from that header.

## Versioning

`ptyx_abi_version` returns a packed major and minor version. A major change may
remove or reinterpret an existing contract. A minor change is additive.

Before 1.0, development builds may replace the complete ABI without a
compatibility shim. Once 1.0 is declared:

- existing symbols and value meanings remain compatible within the major
  version;
- structures grow only through a caller-provided `struct_size`;
- new flags use previously reserved bits;
- reserved fields remain zero;
- symbol removal or ownership changes require a new major version.

The exported-symbol allowlist is tested for each produced library.

## Types and layout

The ABI uses:

- opaque incomplete structs for runtime ownership;
- `uint64_t` generation-tagged session identities;
- fixed-width integer status, kind, flag, length, and code fields;
- caller-sized structures with fixed alignment;
- explicit calling and symbol visibility macros.

It does not use C enum layout, `bool`, `size_t`, compiler bitfields, flexible
array members, Rust layout, or platform-dependent handle types in public
structures.

Every input structure begins with `uint32_t struct_size`. A caller initializes
the known prefix and zeroes reserved storage. A callee reads no field beyond
the supplied size.

## Runtime

One runtime owns shared readiness infrastructure, the platform backend, the
Unix broker connection where applicable, the session registry, and fair event
scheduling.

Runtime creation is transactional. Failure returns no usable runtime and
releases every partially acquired resource.

`ptyx_runtime_next_event` is the only blocking operation. One logical consumer
calls it for a runtime. It fairly selects from per-session event queues.
Runtime shutdown wakes a blocked call. Runtime release is valid only after
shutdown has converged and every session and event ownership has been
released.

The ABI has no callback registration. No ptyx operation invokes caller code.

## Sessions

`ptyx_session_spawn_start`:

- validates and copies every caller-owned option before returning;
- allocates a generation-tagged identity;
- returns promptly without waiting for process creation;
- later produces exactly one spawn-ready or spawn-failed event;
- keeps an unpublished spawn under native ownership until routed, released,
  or expired by its bounded activation lease.

Session handles are scoped to their runtime. Index and generation must both
match. A retired generation is never made valid again.

`ptyx_session_write` accepts a complete buffer into bounded native ownership
or accepts none of it. Returning success permits the caller to reuse or free
the input memory. Backpressure is a recoverable status. A sticky input failure
is terminal for later input but does not fabricate output or child state.

Resize, termination, metadata, and mode-observation operations report their
own capability, state, and native failures. They do not silently start close.

`ptyx_session_close` atomically starts or joins asynchronous close. One
close-complete event reports convergence. `ptyx_session_release` is a
nonblocking abandonment operation and does not claim successful close.

`ptyx_session_cancel_output` commits drain-and-discard for the session output
direction. A binding invokes it when its output reader or stream is canceled
and releases every outstanding output event. The operation is required
because releasing one event does not imply that the consumer has abandoned
all future output.

The Dart package does not expose this low-level command as a session method.
It maps `StreamSubscription.cancel` to it.

## Events

An event contains:

- kind and flags;
- originating session identity;
- kind-specific fixed-width values;
- a value-based error when applicable;
- an owned payload reference when applicable;
- private release storage reserved for the implementation.

`ptyx_runtime_next_event` transfers event ownership to the caller on success.
The caller must invoke `ptyx_event_release` exactly once for every owning
event, including events it ignores.

An output pointer remains readable and immutable until release. Releasing an
output event returns its native byte credit. Closing or releasing a session
does not invalidate an event already transferred to the caller.

Non-owning terminal events may still require release so callers can use one
uniform rule. Releasing a zero-initialized event is allowed. Copying an owning
event structure and releasing both copies is invalid.

## Errors

Errors are values containing:

- a stable domain;
- a stable kind;
- the failed operation;
- an optional native or OS code;
- bounded non-secret numeric context.

An error contains no borrowed message pointer. `ptyx_error_format` writes an
actionable description into caller-owned storage and reports the required
length when the buffer is absent or too small.

Errors never include input bytes, environment values, or other caller secrets.
Invalid pointer, length, flag, state, and stale-handle errors are deterministic
and do not mutate session state.

## Threading and reentrancy

Unless a function is documented more narrowly:

- runtime and session command functions may be called concurrently;
- one logical thread consumes runtime events;
- no operation calls back into caller code;
- command functions do not wait for reactor progress or queue capacity;
- event release may run on a different thread from event receipt;
- shutdown and release have explicit ordering requirements.

Concurrent session writes are FIFO by their successful admission
linearization order. Wall-clock invocation order between unsynchronized
threads is not defined.

## Panic and foreign safety

Every exported Rust function validates its pointers before dereference and
contains unwinding. No panic crosses the ABI. A contained panic returns an
infrastructure error or places the affected runtime into deterministic
shutdown when state integrity cannot be established.

Unsafe code states its aliasing, lifetime, layout, and concurrency invariants
beside the unsafe operation. The standalone C and C++ harnesses exercise null
inputs, boundary lengths, stale identities, wrong states, event lifetime,
shutdown, and panic containment.

## Documentation

The public header uses Doxygen groups for:

- version and capabilities;
- runtime;
- sessions;
- events;
- errors.

Every declaration documents `@param[in]`, `@param[out]`, or
`@param[in,out]`, ownership transfer, blocking, thread safety, valid states,
failure results, and release requirements. `WARN_AS_ERROR` and parameter
documentation warnings are enabled in CI.
