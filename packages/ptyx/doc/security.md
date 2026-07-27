# Security model

`ptyx` creates a local child with the privileges of the Dart process. It is a
transport and ownership library, not a sandbox, policy engine, shell parser,
terminal emulator, or remote-access protocol.

## Trust boundaries

- The caller owns the executable path, arguments, environment, working
  directory, and all bytes written to the child.
- Child output is untrusted binary data. Terminal escape sequences can affect a
  terminal emulator if a caller renders them without filtering.
- No shell is inserted. Shell expansion occurs only when the caller explicitly
  starts a shell.
- Environment values and input payloads are not placed in package diagnostic
  messages. Native error context is limited to operation and safe OS detail.

## Native helper and artifacts

On Unix, the controller verifies the embedded broker bytes before publishing
an owner-only executable materialization. Installation uses an interprocess
lock, unpredictable temporary names, atomic publication, and post-publication
owner, type, mode, size, and content checks.

Linux receives broker-transferred PTY masters atomically close-on-exec.
Darwin provides no equivalent `recvmsg` flag; ptyx marks the descriptor before
publishing it and uses close-by-default for its own process creation. Embedders
that invoke a foreign, inheriting native process-creation API concurrently with
PTY spawn must serialize that operation on macOS.

Applications subject to code-signing, sandbox, allow-list, or no-exec policies
must bundle and select their own signed broker with `PTYX_BROKER_BINARY`.
`ptyx` never falls back to forking a child inside the multithreaded Dart host.

On Windows, named controller pipes use unpredictable names, first-instance
creation, and a restrictive access-control list. The child is associated with
a kill-on-close Job Object during process creation.

## Resource denial

Public and native boundaries cap argument and environment counts, individual
strings, input and output queues, dimensions, protocol frames, waiter tables,
and live Windows close operations. Output backpressure is intentional: an
unconsumed stream can stop the child after the bounded queue fills. Call
`discardOutput` when output is not needed.

The caller should still apply its own concurrency, runtime, executable, and
filesystem policies. A child with equal privileges can consume CPU, allocate
memory, access the caller's files, and on Unix deliberately escape the
original terminal process group.

## Sensitive applications

Do not use terminal-mode observation as an authentication boundary. It is a
best-effort snapshot suitable for user-interface behavior such as hidden-input
indication. Keep secrets out of command-line arguments when the operating
system exposes process command lines, and avoid inheriting unrelated
environment secrets.
