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

Crash artifacts and generated corpora are intentionally ignored. A minimized
regression must be converted into a deterministic broker test before a fix is
accepted.
