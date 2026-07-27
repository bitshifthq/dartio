# Base validation

The base revision is `4c6120c38dec4ac36a7fb349c274ae3123fe4224`.
It was checked in a detached worktree at
`/private/tmp/ptyx-base.1j6Lna`. The worktree path is incidental and may be
replaced by any clean detached worktree at the same revision.

## Commands and results

| Command | Result |
|---|---|
| `dart format --set-exit-if-changed --output=none packages/ptyx` | Passed, 28 files checked and no changes |
| `dart analyze --fatal-infos --fatal-warnings packages/ptyx` | Passed, no issues |
| `dart test` from `packages/ptyx` | Passed, 46 tests |
| `cargo fmt --manifest-path packages/ptyx/native/Cargo.toml -- --check` | Passed |
| `cargo clippy --manifest-path packages/ptyx/native/Cargo.toml --all-targets -- -D warnings` | Passed |
| `cargo test --manifest-path packages/ptyx/native/Cargo.toml` | Passed, 30 unit tests and no doctests |
| `cargo doc --manifest-path packages/ptyx/native/Cargo.toml --no-deps` | Passed |
| `dart pub publish --dry-run` from `packages/ptyx` | Passed with zero warnings |

Running `dart test` at the repository root exits with code 65 because the
workspace root has no `test/` directory. Package validation therefore runs the
command from `packages/ptyx`, matching the workflow.

The Rust build emitted Xcode cache warnings in the sandbox while resolving the
Apple SDK. Compilation and all checks still completed. These warnings are
environment evidence, not suppressed package diagnostics.

## Benchmark reproduction

The retained scorecard harness is
`benchmark/base_scorecard.dart`. Run it against the base worktree's package
configuration while keeping the harness source fixed:

```text
dart --packages=<base>/.dart_tool/package_config.json \
  benchmark/base_scorecard.dart <workload>
```

The raw measurements, host metadata, exact workload sizes, and rejected-run
notes are stored in
`benchmark/results/base-macos-x64-4c6120c.json`. The retained runs use:

- 400 sequential one-byte echo round trips after a child READY gate;
- 32 MiB child output with every received byte checked;
- 32 MiB input with the child's exact received count checked;
- 50 complete spawn, output-drain, exit, and close cycles;
- an attempt to hold 100 idle sessions under the host's normal file limit.

Five interactive runs produced p99 values from 1.594 to 1.813 ms. Four of five
output runs were between 84.0 and 90.6 MiB/s; the retained outlier was
56.1 MiB/s. Input was stable between 4.55 and 4.63 MiB/s. Complete lifecycle
p50 was 24.2 ms and p99 was 79.1 ms.

The idle workload failed while creating session 61 with `EMFILE` under the
host's 256-descriptor soft limit. This is a valid package-plus-host result:
ptyx cannot meet the unqualified 100-session requirement on this host without
first accounting for and documenting the operating-system resource
prerequisite.

The initial output verifier flattened chunks into asynchronous per-byte events
and was stopped because it measured the harness. Retained runs iterate native
Dart chunks. An initial profiler invocation attached to the Dart launcher
shell and was also rejected.

The input workload was launched under Xcode Instruments Time Profiler:

```text
xctrace record --template 'Time Profiler' --launch -- <dart-vm> \
  --packages=<base>/.dart_tool/package_config.json \
  benchmark/base_scorecard.dart input
```

Of 8,619 samples, 6,962 ended in `write(2)`: 80.8 percent. The profile and the
separate resource sample both identify synchronous small PTY writes and
wakeups as the first performance experiment. The committed JSON records the
summary; the environment-specific Instruments trace is intentionally not a
source artifact.
