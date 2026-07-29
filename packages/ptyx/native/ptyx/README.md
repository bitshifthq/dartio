# ptyx

`ptyx` is an opinionated native pseudo-terminal crate for Linux, macOS, and
Windows. It provides bounded synchronous input admission, ordered output,
owned child-process cleanup, and both blocking and `Future` completion without
requiring an async runtime.

On Linux and macOS, the private broker isolates the fork/session setup required
for safe process creation in a multithreaded application. A qualified release
artifact packages its target-matched broker and materializes it in a per-user,
mode-checked cache. The repository source crate deliberately contains no
prebuilt executable: source consumers must build the same-target broker and
select it with `RuntimeBuilder::broker_path` or `PTYX_BROKER`. This distinction
prevents a host broker from being embedded into a cross-target build.

```rust,no_run
use bytes::Bytes;
use ptyx::{Event, Runtime, Size, SpawnOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = if let Some(path) = std::env::var_os("PTYX_BROKER") {
        Runtime::builder().broker_path(path).build()?
    } else {
        Runtime::new()?
    };
    let spawned = runtime.spawn_blocking(
        SpawnOptions::new("sh")
            .with_arguments(["-c", "read line; printf '%s' \"$line\""])
            .size(Size::new(24, 80)),
    )?;
    let (session, mut events) = spawned.into_parts();
    session.write(Bytes::from_static(b"hello\n"))?;

    while let Event::Output(chunk) = events.recv()? {
        println!("{}", String::from_utf8_lossy(&chunk));
    }
    session.close()?.wait()?;
    Ok(())
}
```

The safe Rust API does not expose reactor handles, native queues, C ABI types,
or Dart runtime concepts. See the repository documentation for platform
capabilities and lifecycle guarantees. Delayed input, output, metadata,
infrastructure, and cleanup failures retain their stable operation, category,
and native status in `OperationError`. A rejected write also returns the exact
unaccepted `Bytes`; after a terminal input failure it exposes the same retained
cause. `CloseResult` preserves simultaneous input, output, and cleanup failures
and selects cleanup, then input, then output as its diagnostic priority.
