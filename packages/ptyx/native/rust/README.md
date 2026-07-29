# ptyx

`ptyx` is an opinionated native pseudo-terminal crate for Linux, macOS, and
Windows. It provides bounded synchronous input admission, ordered output,
owned child-process cleanup, and both blocking and `Future` completion without
requiring an async runtime.

On Linux and macOS, install the matching `ptyx-broker` version and pass its
absolute executable path to `RuntimeBuilder`. The broker isolates the
fork/session setup required for safe process creation in a multithreaded
application.

```console
cargo install ptyx-broker --version 0.0.1
```

```rust,no_run
use bytes::Bytes;
use ptyx::{Event, Runtime, Size, SpawnOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = Runtime::builder();
    #[cfg(unix)]
    {
        let broker = std::env::var_os("PTYX_BROKER")
            .map(std::path::PathBuf::from)
            .expect("PTYX_BROKER must name the matching ptyx-broker executable");
        builder = builder.broker_path(broker);
    }
    let runtime = builder.build()?;
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
capabilities and lifecycle guarantees.
