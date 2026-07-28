# Selected architecture

## Decision

`ptyx` has three supported consumption layers:

1. The `ptyx` Rust crate is the authoritative PTY implementation.
2. The `ptyx` C ABI is a stable, language-neutral adapter over the Rust crate.
3. The Dart package is an idiomatic wrapper over the C ABI.

The core uses direct Rust platform backends with a shared lifecycle, queue,
error, and event model. Unix process creation and reaping remain isolated in a
persistent helper process. Windows uses direct ConPTY, IOCP, and Job Object
ownership.

The core does not depend on Dart, Dart headers, Dart ports, FFI layouts, or
isolate lifecycle. The C ABI does not depend on Dart. A separate private Dart
transport shim uses only the public C ABI and Dart API-DL to deliver events to
an isolate.

## Dependency and ownership layers

```text
Rust application
      |
      v
ptyx Rust crate
      |
      +-------------------+
      |                   |
Unix PTY and broker   Windows ConPTY
      ^
      |
language-neutral C ABI <--- other language bindings
      ^
      |
Dart transport shim
      ^
      |
idiomatic Dart package
```

Dependency arrows point toward the authoritative implementation. Neither the
Rust crate nor the C ABI references a higher layer.

## Rust API

The Rust API is small and concrete:

```rust
pub struct Runtime;
pub struct RuntimeBuilder;
pub struct Session;
pub struct Events;
pub struct Spawn;
pub struct Spawned {
    pub session: Session,
    pub events: Events,
}
pub struct Close;
pub struct OutputChunk;

impl Runtime {
    pub fn builder() -> RuntimeBuilder;
    pub fn spawn(&self, options: SpawnOptions) -> Spawn;
}

impl Session {
    pub fn write(&self, data: bytes::Bytes) -> Result<(), WriteError>;
    pub fn resize(&self, size: Size) -> Result<(), ResizeError>;
    pub fn terminate(&self, signal: Signal) -> Result<bool, TerminateError>;
    pub fn snapshot(&self) -> Result<SessionSnapshot, MetadataError>;
    pub fn close(&self) -> Close;
}

impl Events {
    pub fn recv(&mut self) -> Result<Event, RecvError>;
    pub fn try_recv(&mut self) -> Result<Option<Event>, RecvError>;
    pub fn cancel_output(&mut self) -> Result<(), SessionError>;
}
```

`Spawn` and `Close` implement `Future` and provide consuming blocking
`wait` methods backed by the same completion primitive. `Events` implements
`futures_core::Stream` without requiring a particular async runtime.

`write` is the only input method. It performs bounded, nonblocking,
all-or-nothing admission and accepts owned `Bytes`, allowing Rust callers to
transfer `Vec<u8>` storage without another copy. The public API has no
`try_write`, `send`, capacity waiter, flush, or input-completion object.

`OutputChunk` owns its bytes and native output credit. Dropping it returns the
credit. `Session` is safe to share between threads. `Events` has one logical
consumer. Dropping a live session requests nonblocking abandonment; explicit
`close` is required to observe accepted-input and cleanup failures.

No public backend, executor, reactor, or platform strategy traits are exposed.
The package supports one measured native strategy per target.

## Shared core

One implementation owns:

- session, child, input, output, exit, mode, and cleanup state;
- generation-checked identities and retirement;
- bounded input admission and FIFO partial-write handling;
- bounded output queues and delivery credit;
- sticky direction-scoped failures and error precedence;
- asynchronous spawn and close completion;
- per-session event queues;
- fair runtime event scheduling;
- close deadlines and cleanup convergence.

Unix and Windows modules own only operations that genuinely differ:

- PTY or ConPTY creation;
- platform process and job identity;
- descriptor or handle registration;
- reads, writes, cancellation, resize, signal, and termination syscalls;
- exact exit observation and platform cleanup actions.

Platform drivers report typed results into the shared state model. They do not
implement independent session state machines.

## Event scheduling

Each session owns a bounded payload queue. The runtime owns a coalesced ring of
ready session identities. A session appears in that ring at most once.

The Rust `Events` consumer receives from its session queue. The C adapter uses
the same queue authority through one fair blocking operation:

```c
ptyx_status_t ptyx_runtime_next_event(
    ptyx_runtime_t *runtime,
    ptyx_event_t *event,
    ptyx_error_t *error);
```

The operation selects the next ready session, transfers at most one event or a
bounded byte quantum, and places a still-ready session at the back of the
ring. Runtime shutdown wakes a blocked consumer with a closed result.

This keeps payload capacity independent per session, prevents a noisy session
from starving peers, and avoids public callback, missed-wakeup, polling, and
rearm protocols.

## C ABI

The C ABI exposes only:

- ABI version and capability discovery;
- runtime creation, event receipt, shutdown, and release;
- asynchronous session spawn;
- synchronous bounded write admission;
- resize and termination;
- a typed metadata snapshot;
- mode-observation subscription state;
- asynchronous close and nonblocking release;
- event release and error formatting.

It uses opaque runtime pointers and fixed-width generation-tagged session
handles. Public constants use fixed-width integer typedefs rather than C enum
layout. Extensible structures begin with `struct_size`; reserved fields must
be zero.

Every call documents:

- pointer and buffer ownership;
- valid session states;
- blocking behavior;
- thread safety and reentrancy;
- success and failure postconditions;
- event-release obligations;
- error value and message lifetime.

Events carry value-based stable error domains and kinds plus an optional
native status. Output pointers remain valid until `ptyx_event_release`.
Unwinding is contained at every exported Rust entry point.

The authoritative public header uses Doxygen groups for runtime, sessions,
events, errors, and capabilities. Documentation warnings fail CI. Generated
Dart bindings mirror this header and are never edited manually.

## Dart integration

The Dart public library exports no C type, pointer, integer handle, native
status constant, port protocol, or FFI helper.

The package follows these binding practices:

- a small public export barrel;
- generated native declarations under `lib/src/ffi`;
- generated declarations imported only by private translation modules;
- deterministic native-asset selection in the package build hook;
- authoritative-header-first generation;
- typed error and value translation before values reach implementation code.

The private Dart transport shim owns only:

- Dart API-DL initialization;
- one native event-pump thread per native runtime;
- owner and session-to-port routing;
- retained output event tokens awaiting Dart acknowledgement;
- failed-post cleanup;
- nonblocking owner abandonment and finalization.

It invokes only public C ABI operations. It does not own PTY state, process
cleanup, close timing, mode polling, queue policy, or error precedence.

Dart retains only state required by its public contract:

- output and mode stream controllers;
- spawn, exit, and close completers;
- immediate closing and closed rejection;
- typed Dart error translation;
- immutable spawn option snapshots;
- cached public metadata;
- bounded outstanding output delivery tokens.

A minimal owner guardian remains as an individual-isolate exit oracle. It
owns one native owner token, not session state or spawn execution.
`NativeFinalizer` provides nonblocking unreachable-object and isolate-group
cleanup fallback.

## Output cancellation

`output` is the sole Dart-facing output lifecycle API. Canceling its
subscription commits native drain-and-discard. There is no separate
`discardOutput` method in Dart.

The Dart stream cancellation path submits the same native transition and
releases every retained delivery token. Closing a session may also discard
undelivered output as part of shutdown, but normal child exit preserves
trailing output.

Rust expresses this through the `Events` consumer that owns output delivery.
The C ABI exposes one narrowly scoped `ptyx_session_cancel_output` command so
language bindings can translate their stream or reader cancellation. It is
not exported by the Dart public library.

## Credit and copying

Native output credit remains held while bytes are queued in native memory,
posted to Dart, or retained by a paused Dart subscription. Posting a Dart
message does not return credit.

The transport uses a fixed, byte-accounted, benchmark-selected window of
outstanding output events per session. A single-flight window is not the
default because retained measurements show that bounded multi-message
pipelining materially improves sustained output. Pause, cancellation, stale
route, port failure, close, and isolate loss release every token exactly once.

The initial copy profile is:

- Rust input: zero-copy ownership transfer for owned `Bytes` or `Vec`;
- C input: one copy into core-owned storage;
- Dart input: two copies through ordinary FFI;
- Dart output: one Dart VM typed-data copy.

A Dart-specific one-copy input path or external output data requires a
prototype that proves lower total CPU, memory, allocation, and energy cost
without weakening ownership or cleanup. It is not part of the initial public
contract.

## Efficiency requirements

The runtime uses shared readiness infrastructure rather than per-session I/O
threads. Work per wake is bounded by command, event, byte, and syscall quanta.
Inactive sessions allocate no large scratch buffers and produce no periodic
wakeups. Buffers are allocated lazily, reused where ownership permits, and
released when direction state becomes terminal.

Default budgets are selected against the complete scorecard, including:

- throughput and interactive latency;
- resident and peak memory at 1, 10, and 100 sessions;
- CPU time, wakeups, context switches, and idle energy;
- allocation and copy counts;
- active-session fairness;
- pause, cancellation, saturation, and shutdown memory.

An optimization is retained only when it improves the complete boundary or
has a measured correctness benefit. A throughput gain cannot justify hidden
loss, unbounded mailboxes, busy polling, thread proliferation, or delayed
cleanup.

## Platform ownership

### Unix

A persistent single-threaded broker owns process creation, exact child reaping,
process-group identity, and forced cleanup. PTY masters transfer to the core
through a bounded versioned protocol. The Dart host never forks PTY children
and never becomes their parent.

The reusable Rust crate accepts an explicit broker path or a broker provider
configured by `RuntimeBuilder`. It never silently falls back to in-process
fork. The Dart package supplies its integrity-checked, target-matched broker
through its native-asset build.

### Windows

Windows uses ConPTY, overlapped controller pipe ends, IOCP, and a kill-on-close
Job Object. Pinned I/O memory remains owned until terminal completion.

The supported and tested runtime floor is Windows build 26100. Earlier builds
may work for some workloads but are not supported because their
`ClosePseudoConsole` behavior cannot satisfy deterministic bounded cleanup.

## Failure and close ordering

An unrecoverable input transport failure stops input only when continued byte
ordering cannot be established. It rejects later writes, emits one sticky
input failure after safely buffered output, and is retained by close. It does
not independently discard output, fabricate child exit, or prevent safe
signaling and metadata access.

Close is idempotent:

1. reject new operations;
2. resolve accepted input by delivery or typed failure;
3. request platform job termination;
4. preserve or explicitly discard output according to the close contract;
5. observe direct-child status independently;
6. complete platform I/O and release pinned memory;
7. release process, job, descriptor, handle, broker, queue, and event ownership;
8. emit one close completion containing the authoritative failure, if any.

Every failure path enters this state machine. Destructors and isolate-loss
handlers request it rather than implementing separate cleanup sequences.

## Rejected alternatives

- A Dart-owned supervisor and queue model duplicates native lifecycle and
  backpressure behavior.
- Dart polling adds wakeups, latency, and Dart implementation state.
- Public C callbacks add callback-thread, lifetime, reentrancy, and
  quiescence contracts.
- A public wait, poll, and rearm sequence exposes a missed-wakeup protocol and
  permits unfair draining.
- One global payload queue allows capacity competition and head-of-line
  blocking between sessions.
- Per-session I/O threads scale memory, scheduler work, and energy use with
  idle session count.
- External typed data weakens deterministic output-credit ownership unless its
  VM finalizer and shutdown behavior are separately proven.
- Direct child creation inside the multithreaded Dart host cannot provide the
  required Unix parent and descriptor ownership.

## Evidence and remaining gates

The direct Rust candidate, retained production benchmarks, broker prototypes,
fault tests, and platform runs justify the selected direction. They do not
remove the need to qualify the rewritten boundary.

Before the architecture is complete, retained exact-revision evidence must
cover:

- Rust blocking and async APIs over one state model;
- C event fairness, shutdown wake, ownership, layout, and panic containment;
- Dart isolate loss, failed posts, pause, cancellation, and close;
- broker distribution for standalone Rust and C consumers;
- input and output copy and allocation profiles;
- Windows x64 and arm64 on build 26100 or newer;
- Linux and macOS x64 and arm64;
- 2 GiB integrity, sanitizer, fuzz, fault, soak, and performance gates.
