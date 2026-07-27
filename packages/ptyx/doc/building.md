# Building from source

The package requires Dart 3.11 or newer, a stable Rust toolchain, Cargo, and a
C11 compiler. Release artifacts contain a native controller and, on Unix, a
matching broker.

From `packages/ptyx`:

```sh
cargo build --manifest-path native/broker/Cargo.toml --release
PTYX_BROKER_BINARY="$PWD/native/broker/target/release/ptyx-broker" \
  cargo build --manifest-path native/Cargo.toml --release
dart run tool/verify_abi.dart native/target/release/libptyx.dylib
```

Use `libptyx.so` on Linux and `ptyx.dll` on Windows. The ABI verifier checks
the version and every authoritative exported symbol. The standalone C harness
under `native/tests/abi_harness.c` additionally checks header layout, null
inputs, validation limits, and stale-handle rejection.

The Dart build hook normally selects and builds the correct package-local
artifact. Cross-compilation proves only that a target builds; publishable
support also requires the runtime evidence recorded in
[platforms.md](platforms.md).

## Unix broker selection

Set `PTYX_BROKER_BINARY` to an executable broker built from the same package
revision when automatic materialization is unsuitable. The controller and
broker exchange protocol, architecture, build, and ABI identities during
startup and reject mismatches.

## Generated bindings

`include/ptyx.h` is authoritative. After changing it, regenerate
`lib/src/ffi/controller.dart` with:

```sh
dart run tool/ffigen.dart
```

Do not hand-edit generated bindings.
