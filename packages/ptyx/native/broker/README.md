# ptyx-broker

`ptyx-broker` is the companion process used by `ptyx` on Linux and macOS. It
performs the fork, session, controlling-terminal, descriptor, and child
lifetime operations that cannot safely run in an arbitrary multithreaded
application process.

Install the same version as the `ptyx` crate:

```console
cargo install ptyx-broker --version 0.0.1
```

Pass the resulting executable's absolute path through
`RuntimeBuilder::broker_path`. Windows uses ConPTY directly and does not run
this broker.
