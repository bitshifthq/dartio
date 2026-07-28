# Platform qualification

Platform support is an evidence claim, not a consequence of compiling a
target. The public API is intended for the targets below, but a target is
qualified only after the exact native end-to-end job completes on that
operating system and architecture.

| Target | Backend | Build evidence | Runtime qualification |
| --- | --- | --- | --- |
| macOS x64 | `poll` reactor and Unix broker | hosted native build | API, native, ABI, lifecycle, fault, scorecard, and five-minute soak jobs |
| macOS arm64 | `poll` reactor and Unix broker | hosted native build | API, native, ABI, lifecycle, fault, scorecard, and five-minute soak jobs |
| Linux x64 | `epoll` reactor and Unix broker | hosted native build | API, native, ABI, lifecycle, fault, scorecard, and five-minute soak jobs |
| Linux arm64 | `epoll` reactor and Unix broker | hosted native build | API, native, ABI, lifecycle, fault, scorecard, and five-minute soak jobs |
| Windows x64 | ConPTY, IOCP, and Job Object | hosted native build | API, native, ABI, lifecycle, fault, and five-minute soak jobs; diagnostic scorecard pending |
| Windows arm64 | ConPTY, IOCP, and Job Object | hosted native build | API, native, ABI, lifecycle, fault, and five-minute soak jobs; diagnostic scorecard pending |

Android, iOS, and web are not advertised. Linux arm64 cross-compilation is
useful build evidence but does not replace its retained hosted runtime
qualification.

## Windows floor

The runtime floor is build 26100, where `ClosePseudoConsole` became
nonblocking. Retained Windows Server 2022 evidence showed that its older
ConPTY implementation retained one process handle per completed session even
after the child, pipes, HPCON, and job were closed. Supporting that build
would violate the package cleanup contract. `ptyx` still performs
`ClosePseudoConsole` on a bounded process-wide closer pool and limits
admission to 128 live or quarantined sessions. Windows client and Server SKUs
require separate retained runtime qualification.

## Unix helper

Unix process creation and reaping use the packaged `ptyx-broker`. On macOS the
controller starts it with `posix_spawn`. On Linux it uses an audited raw
`fork`/`exec` sequence because the broker must receive a pre-created Unix
socket endpoint. The broker is single-threaded before it forks PTY children.

Hardened, sandboxed, or signed applications should provide an executable,
signed broker path through `PTYX_BROKER_BINARY`. A no-exec cache or temporary
filesystem cannot be used for automatic helper materialization.

## Portable differences

- Unix exposes signal, process-group, terminal-mode, and terminal-name
  capabilities.
- Windows exposes ConPTY and job termination. It does not emulate Unix signal
  or termios semantics.
- Pixel dimensions are accepted as session metadata. ConPTY resize operates
  in cells.
- The exit status is always the direct child's status. On Windows every
  unsigned 32-bit process exit code is preserved.
- Close owns the original Unix process group or Windows job. A same-privilege
  Unix descendant that deliberately creates a new session or process group is
  outside the portable descendant-cleanup guarantee.
