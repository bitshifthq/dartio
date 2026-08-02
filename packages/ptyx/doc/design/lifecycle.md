# Lifecycle state model

The binding ownership boundary is documented in
[`binding-boundary.md`](binding-boundary.md).

The Rust core owns the authoritative lifecycle. Rust callers observe it
through typed values, C callers through generation-tagged handles and events,
and Dart callers through futures, streams, and typed exceptions.

State transitions are monotonic. Cleanup may retry an owned platform action,
but released ownership never becomes live again.

## Ownership

A runtime owns:

- shared readiness and timer infrastructure;
- the platform driver and Unix broker connection where applicable;
- a generation-checked session registry;
- the coalesced ready-session ring;
- runtime failure and shutdown state.

A spawn transaction owns every resource until it produces either a ready
session or a completed spawn failure. A committed session owns:

- the PTY or ConPTY and platform job identity;
- direct-child exit observation;
- accepted input and its byte accounting;
- output buffers, events, and delivery credit;
- readiness registrations and unavoidable pending platform I/O;
- close state, deadlines, and retained failures.

Transferred output events temporarily own their payload and credit
independently of the session handle. Session release cannot invalidate an
event held by a consumer.

The Dart adapter owns only event routing, Dart delivery tokens, and one native
owner token. Dart objects never own raw native buffers or operating-system
resources.

## Session states

| From | Event and linearization point | To | Result |
| --- | --- | --- | --- |
| spawning | platform spawn and core registration commit | open | the spawn-ready event transfers the public session identity |
| spawning | validation, acquisition, process creation, or registration fails | failed | rollback releases every acquired resource before spawn-failed |
| spawning | consumer releases or activation lease expires | closing | native abandonment owns complete cleanup |
| open | first close, owner loss, port loss, or infrastructure failure commits | closing | later live operations are rejected |
| closing | resources are released or quarantined with a retained failure | closed | one close-complete event records the result |
| closed | repeated close or release | closed | the cached result or idempotent release is used |

## Input

`write` has one native admission linearization point. It either transfers the
complete buffer into bounded ownership or accepts none.

| From | Event | To | Result |
| --- | --- | --- | --- |
| open | complete reservation succeeds | open | bytes receive the next FIFO sequence |
| open | admission is temporarily unavailable | open | recoverable backpressure, no bytes accepted |
| open | buffer exceeds the per-write or session limit | open | invalid argument, no bytes accepted |
| open | temporary native write condition | open | the core retains and retries the same prefix |
| open | permanent endpoint write failure | failed | later writes receive the retained input failure |
| open | close commits before admission | closed | the write receives closed |
| open | admission commits before close | open or closing | accepted bytes are delivered or explicitly fail close |
| failed | write | failed | the same typed input failure is returned |
| closed | write | closed | a closed-state error is returned |

A permanent input failure does not independently discard output, fabricate
child exit, or revoke metadata. The core emits one input-failed event after
safely queued output and includes the failure in close completion.

## Output

Output has `awaitingConsumer`, `flowing`, `paused`, `canceling`, `ended`, and
`failed` states. Native byte credit covers queue storage, transferred C events,
posted Dart messages, and Dart-held paused deliveries.

| From | Event | To | Result |
| --- | --- | --- | --- |
| awaitingConsumer | first consumer starts | flowing | queued events become deliverable |
| awaitingConsumer or flowing | budget becomes full | same state | platform reads stop until credit returns |
| flowing | Dart subscription pauses | paused | the bounded delivery window fills, then reads stop |
| paused | Dart subscription resumes | flowing | retained events continue in order |
| any live state | subscription cancellation commits | canceling | buffered and later output is drained and discarded |
| any live state | PTY EOF after queued bytes | ended | output-done follows the final output event |
| any live state | permanent read failure | failed | safe output precedes one output failure |
| any live state | close commits | canceling or flowing | the close policy resolves undelivered output explicitly |

There is no session-level discard method. Stream cancellation is the sole
public discard transition.

## Child and exit

Child state is independent of output:

| From | Event | To | Result |
| --- | --- | --- | --- |
| running | direct-child status is observed | exited | the exact status is cached once |
| running | exit observation fails | observationFailed | the exit consumer receives a typed error |
| running | termination commits before exit | running or exited | the platform owner reports accepted or already exited |
| exited | termination request | exited | no reused numeric process identity is signaled |
| running | close deadline expires | running | force applies to the retained job identity |

Exit may precede trailing output. Output completion never fabricates an exit
status.

## Events and fairness

A session becomes scheduled when its event queue changes from empty to
nonempty. The ready bit and ready-ring insertion change atomically with that
transition.

The runtime event consumer removes one ready identity, transfers bounded work,
and requeues a still-ready session at the tail. A session can occupy at most
one ready-ring entry. Enqueue racing with consumption therefore cannot lose a
wakeup or create unbounded duplicate readiness.

Runtime shutdown marks the event consumer closed and wakes it. It never relies
on timeout polling.

## Close

Close installs one shared completion and follows these phases:

1. stop accepting new operations;
2. resolve accepted input by delivery or retained failure;
3. request graceful termination where the platform contract supports it;
4. escalate against the retained job identity at the deadline;
5. preserve trailing output or commit shutdown discard;
6. observe direct-child exit independently;
7. consume terminal platform I/O completions;
8. release buffers, events, descriptors, handles, process identities, broker
   routes, and registry ownership;
9. publish one close result.

Failure in one phase does not skip independent safe cleanup. An earlier
infrastructure or accepted-input failure remains authoritative when it caused
later cleanup uncertainty.

`Drop`, C session release, Dart finalization, isolate loss, failed Dart event
posting, and explicit close all enter this state machine. They do not
implement separate destructor sequences.

## Race table

| Race | Deterministic result |
| --- | --- |
| spawn completion versus owner loss | native ownership either routes the completion or abandons the staged session; no live child loses an owner |
| write versus close | one admission gate orders the operations; accepted bytes resolve explicitly, later writes receive closed |
| write versus child exit | exit alone does not revoke input; native endpoint state determines full acceptance or rejection |
| signal versus exit | the retained platform job identity orders delivery; a completed exit reports already exited |
| resize versus close | the operation gate commits resize or returns closed |
| output EOF versus exit | independent events retain their observed order without implying each other |
| output EOF versus read failure | the platform reader commits exactly one terminal output result |
| pause or cancellation versus posted output | every delivery token is either acknowledged for delivery or released for discard exactly once |
| close versus transferred event | the event remains valid until event release |
| repeated close | every caller observes the same completion |
| runtime shutdown versus blocked event receipt | shutdown wakes the receiver with closed |
| owner exit while quiet | the Dart owner guardian requests native abandonment without requiring an output post |
| Dart post versus port closure | failure releases the event token and commits owner abandonment |
| finalizer versus explicit close | both request the same idempotent native transition |
| stale handle versus generation reuse | index and generation must match a live entry; retired generations never wrap into validity |
| input failure during close | close rereads the retained failure after terminal input resolution before completing |
| mode observation versus unsupported capability | observation never starts and the Dart stream remains silent |

## Platform-specific convergence

On Unix, the broker retains direct-child parentage and job identity through
termination and exact reap. Broker loss never authorizes signaling a cached
PID that is no longer bound to the owned terminal.

On Windows, close terminates the Job Object, initiates ConPTY close, and
continues consuming terminal IOCP completions. Pinned `OVERLAPPED` memory and
its handles remain owned until completion. Windows build 26100 is the supported
floor because earlier ConPTY close behavior cannot provide the same bounded
convergence.
