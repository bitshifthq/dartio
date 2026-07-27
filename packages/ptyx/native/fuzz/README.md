# Native fuzz targets

`spawn_protocol` feeds arbitrary bytes to the bounded Unix broker spawn
decoder. The target shares the production decoder source so retained crashes
exercise the same length, string, environment, size, and trailing-data checks.

Compile-check the harness:

```sh
cargo check --manifest-path native/fuzz/Cargo.toml
```

With `cargo-fuzz` installed, run it from `packages/ptyx`:

```sh
cargo fuzz run spawn_protocol --fuzz-dir native/fuzz
```

The repository retains a small seed corpus under `corpus/spawn_protocol`.
Generated corpus growth and crash artifacts remain CI artifacts rather than
source files. A minimized crash must be added to the seed corpus and converted
into a deterministic broker regression test before a fix is accepted.
