# Building from source

The package requires Dart 3.11 or newer, a stable Rust toolchain, Cargo, and a
C11 compiler. Release artifacts contain a native controller and, on Unix, a
matching broker.

From `packages/ptyx`:

```sh
cargo build --manifest-path native/broker/Cargo.toml --release
PTYX_BROKER_BINARY="$PWD/native/target/release/ptyx-broker" \
  cargo build --manifest-path native/dart/Cargo.toml --release
dart run tool/verify_abi.dart native/target/release/libptyx_dart.dylib
```

The direct build artifact is `libptyx_dart.so`, `libptyx_dart.dylib`, or
`ptyx_dart.dll`. The Dart build hook installs it under the package asset name
`ptyx`. This product library contains the stable `ptyx_*` C ABI plus the
private `ptyd_*` notification adapter. The ABI verifier checks the version and
every authoritative exported symbol.

Consumers that need only the language-neutral C ABI build
`native/c/Cargo.toml`; its artifact is `libptyx_c.so`,
`libptyx_c.dylib`, or `ptyx_c.dll`. The standalone harness under
`native/c/tests/abi_harness.c` additionally checks header layout, null inputs,
validation limits, and stale-handle rejection.

The Dart build hook normally selects and builds the correct package-local
artifact. Cross-compilation proves only that a target builds; publishable
support also requires the runtime evidence recorded in
[platforms.md](platforms.md).

## Unix broker selection

A qualified Rust or Dart release artifact embeds a target-matched broker and
securely materializes it at runtime. The checked-in Rust source package has no
prebuilt broker and therefore requires `RuntimeBuilder::broker_path` or
`PTYX_BROKER` unless the same-target workspace broker is available. Product
and cross builds set `PTYX_BROKER_BINARY` to the broker built for Cargo
`TARGET`; the build rejects a missing or unqualified cross-target broker
instead of embedding a host executable. Release qualification must package
and execute the resulting target asset, not merely compile the controller.
The controller and broker exchange protocol, architecture, build, and ABI
identities during startup and reject mismatches.

## Generated bindings

`native/include/ptyx/ptyx.h` and the private
`native/dart/include/ptyx_dart.h` are authoritative. After changing either,
regenerate `lib/src/ffi/ptyx.g.dart` with:

```sh
dart run tool/ffigen.dart
```

Do not hand-edit generated bindings.
