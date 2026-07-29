# Native fuzz targets

`broker_decoder` feeds arbitrary bytes through the production broker frame
decoder and, for valid spawn frames, the bounded spawn-payload decoder.
`controller_decoder` exercises the production controller-side frame decoder.
Together they cover both directions of the private broker protocol without a
parallel test parser.

Compile-check the harness:

```sh
RUSTFLAGS="--cfg fuzzing" \
  cargo check --manifest-path native/fuzz/Cargo.toml
```

With `cargo-fuzz` installed, run it from `packages/ptyx`:

```sh
cargo fuzz run broker_decoder --fuzz-dir native/fuzz
cargo fuzz run controller_decoder --fuzz-dir native/fuzz
```

The repository retains a small seed corpus for each target under `corpus/`.
Binary frames are generated from `tool/generate_corpus.dart`; run it with
`--check` to verify that the committed corpus matches its authoritative
inputs. The broker corpus includes a valid spawn frame so fuzzing immediately
reaches the production spawn-payload decoder.
Generated corpus growth and crash artifacts remain CI artifacts rather than
source files. A minimized crash must be added to the appropriate seed corpus
and converted into a deterministic regression test before a fix is accepted.
