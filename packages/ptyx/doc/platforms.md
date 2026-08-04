# Platform qualification

Platform support is an evidence claim, not a consequence of compiling a
target. The public API is intended for the targets below, but a target is
qualified only after the exact native end-to-end job completes on that
operating system and architecture.

| Target | Backend | Current retained evidence | Remaining qualification |
| --- | --- | --- | --- |
| macOS x64 | `poll` reactor and Unix broker | API, native, ABI, lifecycle, and build jobs | Exact-final-revision scorecard and extended soak |
| macOS arm64 | `poll` reactor and Unix broker | API, native, ABI, lifecycle, and build jobs | Exact-final-revision scorecard and extended soak |
| Linux x64 | `epoll` reactor and Unix broker | API, native, ABI, lifecycle, and build jobs | Exact-final-revision scorecard and extended soak |
| Linux arm64 | `epoll` reactor and Unix broker | API, native, ABI, lifecycle, and build jobs | Exact-final-revision scorecard and extended soak |
| Windows x64 | ConPTY, IOCP, and Job Object | API, native, ABI, lifecycle, export, and build jobs | Exact-final-revision scorecard and extended soak |
| Windows arm64 | ConPTY, IOCP, and Job Object | API, native, ABI, lifecycle, export, and build jobs | Exact-final-revision scorecard and extended soak |

The normal six-target matrix is retained in
[Actions run 30468330811](https://github.com/bitshifthq/dartio/actions/runs/30468330811)
for revision `9cc2e43`. That run deliberately skipped the extended jobs. It
therefore proves the listed runtime and build checks for that revision, not
scorecard, sanitizer, fuzz, or soak qualification for later revisions.
Extended evidence must name its exact revision and retained artifact before
this table can record it as passing.

Android, iOS, and web are not advertised. Linux arm64 cross-compilation is
useful build evidence but does not replace its retained hosted runtime
qualification.

## Windows floor

The runtime support floor for release qualification is build 26100, where
`ClosePseudoConsole` became nonblocking. Retained Windows Server 2022 evidence
showed that its older ConPTY implementation retained one process handle per
completed session even after the child, pipes, HPCON, and job were closed.
Supporting that build would violate the package cleanup contract. The native
implementation rejects older builds before spawning a session, so they are not
runtime-compatible fallbacks. Applications must use Windows 26100 or newer
when deterministic cleanup is required.

`ptyx` still performs `ClosePseudoConsole` on a bounded process-wide closer
pool and limits admission to 128 live closer permits. Canceled overlapped I/O
is synchronously observed to terminal completion before its allocation is
released. Windows client and Server SKUs require separate retained runtime
qualification.

## Unix helper

Unix process creation and reaping use the selected `ptyx-broker`. On macOS the
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
