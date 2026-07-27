# Lifecycle state model

The model separates public acceptance, child state, byte transport, and native
ownership. A transition is monotonic. Cleanup can retry work, but no resource
returns to a live state after its release transition.

## Ownership model

Before spawn commits, a native spawn transaction owns every acquired object.
An unpublished staged entry has a five-second activation deadline; expiry
commits native abandonment so isolate loss between native spawn and Dart
publication cannot orphan the child. The native module is pinned for the
process lifetime before any controller thread starts, ensuring those threads
and their TLS destructors cannot outlive loaded code.
After commit, a generation-checked session entry owns:

- the PTY master and platform child or job identity;
- direct-child exit observation and exact-once reap state;
- input queue bytes, sequence numbers, capacity waiters, and flush barriers;
- output buffers, one copied Dart message, and explicit delivery credit;
- reactor registrations or unavoidable platform workers;
- Dart output, event, and control ports;
- mode-observer registration and timers.

The Dart object owns the public protocol, not raw native memory. Releasing the
Dart wrapper requests shutdown but cannot directly free a pointer that native
operations may still reference.

## Session transitions

| From | Event and linearization point | To | Ownership result |
|---|---|---|---|
| spawning | native session entry and all initial registrations commit | open | spawn transaction transfers all resources to the entry |
| spawning | any acquisition, child setup, exec, registration, or post setup fails | failed | transaction closes and reaps every acquired resource before reporting |
| open | first explicit close, isolate loss, or terminal port-loss decision | closing | new public operations are rejected and one close completion is installed |
| closing | all resources reach released or a final cleanup failure is recorded | closed | registry generation is retired and late operations are rejected |
| closed | repeated close | closed | caller observes the cached close result |

## Input transitions

`tryWrite` and awaiting `write` linearize when a complete byte count and
sequence range are reserved. The writer releases capacity only for bytes
actually passed to the PTY or explicitly failed during shutdown. A flush
linearizes when it snapshots the last accepted sequence.

| From | Event | To | Result |
|---|---|---|---|
| open | full reservation succeeds | open | bytes are accepted in sequence order |
| open | capacity is insufficient | open | fast write returns false; waiter remains unreserved |
| open | permanent native write failure | failed | queued writes and flushes complete with the same input failure |
| open | close commits | closed | unaccepted waits fail as closed; accepted sequences finish or receive explicit close failure |
| failed | write or flush | failed | cached input failure is returned |
| closed | write or flush | closed | state exception is returned |

## Output transitions

Native read credit covers bytes until Dart delivery or explicit discard.
Pausing and listening change credit flow, not byte order.

| From | Event | To | Result |
|---|---|---|---|
| awaitingListener | first listen commits | flowing | buffered bytes begin ordered delivery |
| awaitingListener | budget is full | awaitingListener | PTY read readiness is disabled |
| awaitingListener | explicit discard | discarding | buffered and later bytes are acknowledged without Dart delivery |
| flowing | subscription pause commits | paused | delivery stops and bounded credit eventually disables PTY reads |
| paused | resume commits | flowing | ordered delivery resumes |
| flowing or paused | cancel commits | discarding | no later user bytes are delivered |
| any live output state | PTY EOF after queued bytes | ended | stream closes after all deliverable bytes |
| any live output state | permanent read failure | failed | safe trailing bytes, then one output error, then close |
| any live output state | session close commits | discarding | shutdown may discard undelivered output and releases all credit |

## Child and exit transitions

The native child identity is retained until an exact-once wait operation
records a status or an exit-observation failure. A cached terminal-job
identity, not an unchecked numeric PID, controls signal and cleanup decisions.

| From | Event | To | Result |
|---|---|---|---|
| running | direct child status is observed | exited | typed exit status is cached once while the owned Unix leader remains unreaped until cleanup |
| running | wait facility fails | exitObservationFailed | exit future receives a dedicated failure |
| running | signal races before reap commit | running or exited | delivery result reflects the OS operation |
| exited | signal request | exited | returns already-exited without an OS signal |
| running | graceful close deadline expires | running | force termination is requested against the owned job |

## Race table

| Race | Linearization and deterministic result |
|---|---|
| spawn success versus setup failure | success commits only after every required registration; earlier failures stay transaction-owned and cannot expose a session |
| write versus close | the input reservation lock orders them; accepted writes receive flush or explicit close failure, later writes receive closed |
| write versus child exit | exit alone does not revoke input until the OS write path closes; each write is either accepted or rejected in full |
| signal versus exit and reap | the child-state lock and retained OS identity order the request; after reap commit the result is already-exited |
| resize versus close | the session operation gate orders them; resize either commits before close or receives closed |
| output EOF versus exit | independent completions are preserved; neither fabricates or delays the other |
| output EOF versus read failure | the reader commits exactly one terminal event; already-read bytes precede it |
| pause or cancel versus close | close commits discard and cleanup; a prior cancel also yields discard, and no path delivers late bytes |
| repeated or concurrent close | atomic installation of one shared close completion makes all callers observe one result |
| native output post versus port closure | failed post returns credit and commits native shutdown; no Dart acknowledgment is awaited |
| copied output post versus shutdown | a successful post retains one charged message until Dart delivery or discard returns credit; a failed post rolls the same credit back before shutdown |
| isolate loss versus explicit close | a Dart finalizer, failed operational post, or failed quiet-port probe reaches the same native abandonment entry; the first shutdown cause commits and later causes merge into the same cleanup |
| reactor failure versus public operations | the reactor commits affected sub-resources to typed failures, then performs session cleanup without corrupting other sessions |
| mode timer versus close | generation and observer state are checked at callback commit; a late callback is discarded without touching released state |
| handle reuse versus late ABI call | registry index and generation must both match a live entry; retired generations are never dereferenced |

## Cleanup order

Cleanup first prevents new work and wakes capacity waiters. It then requests
graceful job termination, continues the platform-required output drain,
escalates at the deadline, observes or records direct-child exit, unregisters
readiness and timers, resolves accepted input and output credit, closes OS
resources, and retires the session generation. Failures are accumulated and
reported only after all independent safe cleanup actions have run.
