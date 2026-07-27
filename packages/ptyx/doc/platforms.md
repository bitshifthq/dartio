# Platform qualification

Platform support is an evidence claim, not a consequence of compiling a
target. The public API is intended for the targets below, but a target is
qualified only after the exact native end-to-end job completes on that
operating system and architecture.

| Target | Backend | Build evidence | Runtime qualification |
| --- | --- | --- | --- |
| macOS x64 | `poll` reactor and Unix broker | local release build | local API, native, ABI, lifecycle, and fault tests |
| macOS arm64 | `poll` reactor and Unix broker | CI build target exists | pending native arm64 CI result |
| Linux x64 | `epoll` reactor and Unix broker | CI build target exists | pending successful CI rerun |
| Linux arm64 | `epoll` reactor and Unix broker | cross-check succeeds | pending native arm64 runtime result |
| Windows x64 | ConPTY, IOCP, and Job Object | cross-check succeeds | pending successful Windows CI rerun |
| Windows arm64 | ConPTY, IOCP, and Job Object | CI build target exists | pending native arm64 runtime result |

Android, iOS, and web are not advertised. Linux arm64 cross-compilation is
useful build evidence but is not runtime qualification.

## Windows floor

ConPTY is required, so the runtime floor is Windows 10 version 1809, build
17763. Older pseudoconsole implementations may block during close. `ptyx`
therefore performs `ClosePseudoConsole` on a bounded process-wide closer pool
and limits admission to 128 live or quarantined sessions. A timed-out close
retains ownership until the worker returns or process shutdown reclaims it.
Windows client and Server SKUs still require separate retained runtime
qualification.

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
