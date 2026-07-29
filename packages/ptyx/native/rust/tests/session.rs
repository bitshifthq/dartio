#![cfg(any(target_os = "linux", target_os = "macos"))]

use bytes::Bytes;
use ptyx::{Event, Runtime, Size, SpawnError, SpawnOptions, WriteError};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn accepted_input_reaches_the_child_in_order() {
    let broker = broker_path();
    let (result_sender, result_receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_input_round_trip(broker);
        let _ = result_sender.send(result);
    });

    let output = result_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("PTY round trip timed out")
        .expect("PTY round trip failed");

    assert!(
        String::from_utf8_lossy(&output).contains("got:value"),
        "unexpected PTY output: {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn close_waits_for_native_cleanup() {
    let broker = broker_path();
    let (result_sender, result_receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_close(broker);
        let _ = result_sender.send(result);
    });

    result_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("PTY close timed out")
        .expect("PTY close failed");
}

#[test]
fn close_remains_idempotent_after_cleanup() {
    let broker = broker_path();
    let (result_sender, result_receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_repeated_close(broker);
        let _ = result_sender.send(result);
    });

    result_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("repeated PTY close timed out")
        .expect("repeated PTY close failed");
}

#[test]
fn concurrent_close_waiters_share_one_result() {
    let broker = broker_path();
    let (result_sender, result_receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_concurrent_close(broker);
        let _ = result_sender.send(result);
    });

    result_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("concurrent PTY close timed out")
        .expect("concurrent PTY close failed");
}

#[test]
fn rejected_write_accepts_no_partial_prefix() {
    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create PTY runtime");
    let spawned = runtime
        .spawn_blocking(
            SpawnOptions::new("/bin/sh")
                .with_arguments(["-c", "sleep 30"])
                .size(Size::new(24, 80))
                .input_capacity(1),
        )
        .expect("spawn PTY child");
    let (session, _events) = spawned.into_parts();

    let rejected = session.write(Bytes::from_static(b"ab"));
    let accepted = session.write(Bytes::from_static(b"z"));

    assert_eq!(rejected, Err(WriteError::Backpressure));
    assert_eq!(accepted, Ok(()));
}

#[test]
fn snapshot_captures_session_metadata_atomically() {
    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create PTY runtime");
    let spawned = runtime
        .spawn_blocking(
            SpawnOptions::new("/bin/sh")
                .with_arguments(["-c", "sleep 30"])
                .size(Size::new(31, 97)),
        )
        .expect("spawn PTY child");
    let (session, _events) = spawned.into_parts();

    let snapshot = session.snapshot().expect("capture session metadata");

    assert_ne!(snapshot.process_id(), 0);
    assert_eq!(snapshot.size(), Size::new(31, 97));
    assert!(snapshot.terminal_name().is_some());
}

#[test]
fn graceful_close_allows_a_term_handler_to_exit() {
    let elapsed = close_command(
        "trap 'exit 0' TERM; printf ready; while :; do sleep 1; done",
        Duration::from_secs(2),
    );

    assert!(elapsed < Duration::from_secs(2));
}

#[test]
fn graceful_close_escalates_after_the_configured_deadline() {
    let elapsed = close_command(
        "trap '' TERM; printf ready; while :; do sleep 1; done",
        Duration::from_millis(250),
    );

    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[test]
fn zero_graceful_close_timeout_escalates_immediately() {
    let elapsed = close_command(
        "trap '' TERM; printf ready; while :; do sleep 1; done",
        Duration::ZERO,
    );

    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn graceful_close_timeout_rejects_more_than_sixty_seconds() {
    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create PTY runtime");

    let result =
        runtime.spawn(SpawnOptions::new("/bin/sh").graceful_close_timeout(Duration::from_secs(61)));

    assert!(matches!(result, Err(SpawnError::InvalidCloseTimeout)));
}

#[test]
fn graceful_close_flushes_already_accepted_input() {
    let elapsed = close_command_with_input(
        "trap '' TERM; printf ready; read line; while :; do sleep 1; done",
        Duration::from_millis(250),
        Some(Bytes::from_static(b"value\n")),
    );

    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
}

#[test]
fn close_accepts_a_natural_exit_racing_cleanup() {
    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create PTY runtime");

    for _ in 0..64 {
        let spawned = runtime
            .spawn_blocking(
                SpawnOptions::new("/bin/sh")
                    .with_arguments(["-c", "exit 0"])
                    .size(Size::new(24, 80)),
            )
            .expect("spawn short-lived PTY child");
        let (session, events) = spawned.into_parts();

        let close = session.close().expect("start short-lived PTY close");
        drop(events);

        close.wait().expect("close naturally exited PTY child");
    }
}

fn run_input_round_trip(
    broker: PathBuf,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let runtime = Runtime::builder().broker_path(broker).build()?;
    let spawned = runtime.spawn_blocking(
        SpawnOptions::new("/bin/sh")
            .with_arguments(["-c", "read line; printf 'got:%s' \"$line\""])
            .size(Size::new(24, 80)),
    )?;
    let (session, mut events) = spawned.into_parts();
    session.write(Bytes::from_static(b"value\n"))?;

    let mut output = Vec::new();
    let mut output_done = false;
    let mut exited = false;
    while !output_done || !exited {
        match events.recv()? {
            Event::Output(chunk) => output.extend_from_slice(&chunk),
            Event::OutputDone => output_done = true,
            Event::Exited(_) => exited = true,
            Event::InputFailed
            | Event::OutputFailed
            | Event::InfrastructureFailed
            | Event::Closed(_)
            | Event::ModeChanged(_) => {
                return Err(
                    std::io::Error::other("PTY session reported a terminal I/O failure").into(),
                );
            }
        }
    }
    Ok(output)
}

fn close_command(command: &str, timeout: Duration) -> Duration {
    close_command_with_input(command, timeout, None)
}

fn close_command_with_input(command: &str, timeout: Duration, input: Option<Bytes>) -> Duration {
    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create PTY runtime");
    let spawned = runtime
        .spawn_blocking(
            SpawnOptions::new("/bin/sh")
                .with_arguments(["-c", command])
                .size(Size::new(24, 80))
                .graceful_close_timeout(timeout),
        )
        .expect("spawn PTY child");
    let (session, mut events) = spawned.into_parts();
    let mut ready = Vec::new();
    while !ready
        .windows(b"ready".len())
        .any(|window| window == b"ready")
    {
        match events.recv().expect("receive readiness output") {
            Event::Output(chunk) => ready.extend_from_slice(&chunk),
            Event::InputFailed | Event::OutputFailed | Event::InfrastructureFailed => {
                panic!("PTY failed before close")
            }
            _ => {}
        }
    }
    if let Some(input) = input {
        session.write(input).expect("accept input before close");
    }
    let started = Instant::now();
    let close = session.close().expect("start PTY close");
    drop(events);
    close.wait().expect("complete PTY close");
    started.elapsed()
}

fn run_close(broker: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runtime = Runtime::builder().broker_path(broker).build()?;
    let spawned = runtime.spawn_blocking(
        SpawnOptions::new("/bin/sh")
            .with_arguments(["-c", "sleep 30"])
            .size(Size::new(24, 80)),
    )?;
    let (session, events) = spawned.into_parts();

    let close = session.close()?;
    drop(events);
    drop(session);
    close.wait()?;
    Ok(())
}

fn run_repeated_close(broker: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runtime = Runtime::builder().broker_path(broker).build()?;
    let spawned = runtime.spawn_blocking(
        SpawnOptions::new("/bin/sh")
            .with_arguments(["-c", "sleep 30"])
            .size(Size::new(24, 80)),
    )?;
    let (session, _events) = spawned.into_parts();

    session.close()?.wait()?;
    session.close()?.wait()?;
    Ok(())
}

fn run_concurrent_close(broker: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runtime = Runtime::builder().broker_path(broker).build()?;
    let spawned = runtime.spawn_blocking(
        SpawnOptions::new("/bin/sh")
            .with_arguments(["-c", "sleep 30"])
            .size(Size::new(24, 80)),
    )?;
    let (session, _events) = spawned.into_parts();
    let session = Arc::new(session);
    let barrier = Arc::new(Barrier::new(3));
    let mut waiters = Vec::new();
    for _ in 0..2 {
        let session = Arc::clone(&session);
        let barrier = Arc::clone(&barrier);
        waiters.push(thread::spawn(move || {
            barrier.wait();
            session.close()?.wait()
        }));
    }
    barrier.wait();
    for waiter in waiters {
        waiter.join().expect("close waiter panicked")?;
    }
    Ok(())
}

fn broker_path() -> PathBuf {
    std::env::var_os("PTYX_BROKER_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../broker/target/release/ptyx-broker")
        })
}
