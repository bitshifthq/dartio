---
name: ptyx-bindings
description: Maintain the ptyx Dart, generated FFI, private native adapter, and stable C ABI boundary. Use when changing bindings.dart, ptyx.g.dart generation, native marshaling, handles, event decoding, error translation, finalizers, or the C error contract.
---

# Ptyx bindings

Apply this skill before changing the ptyx Dart/FFI/native boundary. Keep the
reusable Rust core independent of Dart, and keep native lifecycle semantics
below Dart.

## Layer rules

Dependency direction is:

```text
public Dart API -> Dart value/marshaling -> generated FFI -> C adapter -> Rust core
```

- `lib/src/ffi/ptyx.g.dart` is generated. Never edit it by hand. Change the
  authoritative header or ffigen configuration, then run
  `dart run tool/ffigen.dart` and review the complete diff.
- Generated functions are the raw ABI surface. They may use `Pointer`, C
  structs, allocator storage, and integer constants.
- `lib/src/native/bindings.dart` is the value-oriented boundary. Its functions
  accept Dart values (`int` handles, `Uint8List`, `String`, `PtySize`, records,
  and options) and return Dart values or translated exceptions. Do not expose
  `Pointer<T>`, allocator objects, or generated structs to callers. Create,
  populate, read, and release native pointers inside this layer.
- High-level session and runtime code must not import generated symbols or
  `dart:ffi`; it calls value-oriented bindings and projects their results into
  the public API. A high-level class may own its finalizer because it owns the
  Dart object; binding functions must not attach, detach, or orchestrate
  finalizers.
- One value-oriented binding function normally maps to one C call. A size-query
  followed by a fill call is the explicit exception required by the C ABI.
- Use integer handles at the Dart boundary. Handles are opaque, nonzero, and
  native-owned; Dart must not dereference or fabricate them.
- Do not add one-line redirect helpers for constants, handle predicates, or
  methods that merely forward to another method. Use the value directly at
  the call site. Keep a helper only when it owns allocation, error conversion,
  cleanup, or another invariant that would otherwise be duplicated.

## Ownership and events

- Native code owns OS handles, threads, queues, buffers, event tokens, and
  cleanup/retry policy. Dart owns public values, stream/future projection,
  argument validation, and exception conversion.
- Decode native event messages once in the runtime-owned decoder. Validate only
  the fixed message shape, byte payload representation, and known event kind;
  native code owns output bounds and token ownership, while session routing
  owns lifecycle-specific interpretation. Unknown numeric kinds are
  infrastructure failures, not guessed events.
- An owning output token is acknowledged or released exactly once. Duplicate,
  stale, or malformed messages converge through the native abort path; Dart
  must not clear maps and hope that a later event cleans up.
- Keep the C event wire kind as a fixed-width integer when future unknown
  values must be tolerated. A private Dart enum may type known kinds. Use C
  enums for closed documented domains only when their ABI width is guarded by
  compile-time checks; never put an implementation-dependent enum in a public
  struct layout.

## Error contract

The C ABI is the concurrency-safe source of truth. Do not use a thread-local
"last error" channel as the primary contract for concurrent sessions or
callbacks: another operation on the same thread can overwrite it before the
caller observes it. A libgit2-style helper may be a documented convenience for
synchronous direct C callers, never the Dart adapter's async contract.

The preferred stable contract is:

```c
ptyx_status_t ptyx_session_write(..., ptyx_error_t *error);
```

- `ptyx_status_t` has an explicit fixed-width representation. Zero is success;
  unknown nonzero values are errors.
- `ptyx_error_t` is caller-owned and versioned by `struct_size`. It contains
  only fixed-width domain/kind/native-code fields: no borrowed pointers,
  operation strings, flags, or message storage. The calling function or event
  kind supplies operation context. A null error is allowed only when
  documented.
- On failure the operation fills the supplied error with the primary failure.
  On success it does not leave stale diagnostics visible. Error lifetime ends
  with the next operation that writes that storage.
- Human-readable messages are formatted into caller-owned storage (or built by
  the Dart binding from stable fields), never returned as borrowed strings that
  cross an async boundary.
- Every exported function documents nullability, invalid handles, threading,
  callback/reentrancy, ownership, blocking, and panic containment.

Do not redesign the error ABI opportunistically. Compare explicit error storage
with a libgit2-style thread-local result in concurrent/reentrant C harnesses and
benchmarks, then obtain approval for any breaking change.

## Change workflow

1. Read the authoritative header, generated bindings, value wrappers, and
   affected tests together. Classify every pointer and error allocation.
2. Change the header/configuration first for ABI changes; regenerate immediately.
   Never patch generated output.
3. Keep raw FFI calls private and value-oriented wrappers small. Add boundary
   tests for nulls, stale handles, malformed events, duplicate tokens,
   concurrent failures, and cleanup races.
4. Format and validate:

```text
dart run tool/ffigen.dart
dart format <changed Dart files>
dart analyze --fatal-infos --fatal-warnings
dart test --concurrency=1
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

5. Review the diff for generated imports in high-level Dart, pointer leakage
   from bindings, duplicate error translation, unowned native resources, stale
   docs, and generated-file drift.
