# Binding boundary

`ptyx` has one native authority and two adapters. The reusable PTY engine and
the language-neutral C ABI own process lifetime, session state, bounded input,
output leases, backpressure, close sequencing, failure precedence, and native
resource cleanup. The Dart adapter owns only the isolate event pump and the
translation needed to present that contract to Dart.

```text
Dart public API
    │  values, validation, Futures, Streams, error conversion
    ▼
private Dart adapter
    │  FFI marshaling, port delivery, finalizers, event-token bookkeeping
    ▼
language-neutral C ABI
    │  handles, ownership, status codes, event records, cleanup
    ▼
reusable Rust PTY engine
    │  operating-system and process implementation
    ▼
platform backends and broker
```

## Ownership matrix

| Responsibility | Owner | Dart representation |
| --- | --- | --- |
| Process and session lifecycle | Rust engine and C ABI | No lifecycle authority |
| Input admission and backpressure | Rust engine and C ABI | One FFI call per accepted write |
| Output lease and native credit | C ABI and Dart adapter | One pending delivery and an acknowledgement |
| Close sequencing and failure precedence | Rust engine and C ABI | Native close result projected to Futures/Streams |
| Handle validity and release | C ABI and Dart adapter | Finalizer and explicit-release plumbing |
| Port delivery and pending spawns | Dart adapter | Bridge bookkeeping only |
| Stream controllers and completers | Dart wrapper | Public asynchronous projection |
| Public validation and value conversion | Dart wrapper | Immutable snapshots and FFI arguments |
| Native error conversion | Dart wrapper | Typed public exceptions |

The Dart fields that retain an input, terminal, or output error retain a native
outcome for repeated public observations. They do not choose failure
precedence or transition native state. The output pending slot, pause flag,
controllers, and completers are transport and presentation state required by
Dart's stream and future contracts.

## Event contract

The private adapter receives a fixed eleven-field message. The router validates
both its shape and the fields required by the event kind before dispatching it.
Output events carry one nonzero owning token. Every such token is acknowledged
exactly once, including malformed or unowned events. A malformed global message
or event is an infrastructure failure; a malformed event for a live session
terminates that session's projected delivery without inventing a new PTY
semantic.

Close flags remain part of the C ABI diagnostic contract. Dart exposes the
primary native failure through its existing typed channels and does not
recompute close precedence from those flags.

## Cleanup contract

Explicit release and detach operations check native status values. Handles and
event tokens remain tracked until release succeeds or the native boundary
reports them stale. A busy or internal result keeps ownership available for a
later cleanup attempt and is surfaced as infrastructure failure when a Dart
owner still exists. Finalizers use the same idempotent paths asynchronously;
they may suppress diagnostics because no Dart owner remains, but they must not
discard ownership before the native release has succeeded.

The reusable Rust engine does not depend on Dart, Dart headers, isolates, or
native ports. The private Dart adapter is the only layer that depends on
Dart API-DL and port delivery.
