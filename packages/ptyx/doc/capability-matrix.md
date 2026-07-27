# Capability matrix

This matrix records product behavior, not API-shape parity. Competitor entries
are intentionally limited to capabilities verified from retained upstream
source revisions; unresolved comparisons remain explicit.

| Capability | `ptyx` | Native OS boundary | Comparable packages / terminals |
| --- | --- | --- | --- |
| Async spawn publication | staged native registration before return | platform-specific | comparison pending reproducible harness |
| Arguments and environment | direct argv; inherit, overlay, replace, clear | `exec` / `CreateProcessW` | broadly available; exact semantics vary |
| Working directory | supported | child setup | broadly available |
| Raw byte I/O | bounded, lossless, ordered | PTY master / ConPTY pipes | broadly available |
| Input readiness | `tryWrite`, async capacity wait, `write` | readiness / IOCP | comparison pending |
| Flush | accepted sequence reaches PTY master | write completion | comparison pending |
| Output backpressure | one credited Dart message; bounded queue | readiness disabled at bound | comparison pending |
| Resize | cells and portable pixel metadata | `TIOCSWINSZ` / ConPTY cells | broadly available |
| Exit status | typed Unix signal; full Windows DWORD | broker `waitid` observation plus exact cleanup reap / process handle | semantics vary |
| Signals | explicit Unix capability | process-group signal | Windows packages generally terminate jobs/processes |
| Descendant cleanup | original Unix process group; Windows Job Object | broker / Job Object | implementation-dependent |
| Terminal name | explicit Unix capability | slave PTY name | implementation-dependent |
| Terminal modes | snapshot and distinct-change stream on Unix | termios polling | implementation-dependent |
| Isolate loss | native finalizer and failed-port cleanup | controller abandonment | comparison pending |
| Errors | typed category, operation, optional OS code | errno / Win32 status | comparison pending |
| Platforms | qualification tracked per OS and architecture | Linux, macOS, Windows | package-specific |

## Deliberate exclusions

Terminal emulation, text decoding, command parsing, shell selection, SSH,
privilege separation, sandboxing, and remote transport are outside the package
boundary.

## Open comparison work

A release claim of best-in-class performance or capability completeness
requires an equivalent retained harness for the base implementation, the
current implementation, direct native code, leading Dart PTY packages,
`portable-pty`/WezTerm where comparable, and Ghostty's isolated PTY path. Until
that matrix exists, this document makes no superiority claim.
