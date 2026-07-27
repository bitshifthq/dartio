# Direct Zig candidate

This Zig 0.16 slice implements the same experiment-only ABI and Unix behavior
as the direct Rust crate. It exists to test whether Zig materially improves
the whole native boundary once Dart FFI, PTY syscalls, child lifecycle, and
cleanup are included.

It is not a production implementation. The synchronous calls, fixed shell
child, post-fork setup, process-global allocator, and lack of Windows support
are explicit rejection points unless a complete later design resolves them.
