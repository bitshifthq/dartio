//! Session runtime threads and native port integration.
//!
//! The runtime starts one reader thread, one waiter thread, one writer thread,
//! and, when enabled, a terminal mode watcher. Shutdown is coordinated through
//! a shared stop flag so close can interrupt backpressure waits and polling.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{RuntimeConfig, SessionOptions};
use crate::error::{PtyxError, PtyxErrorKind};
#[cfg(unix)]
use crate::message::post_term_mode;
use crate::message::{post_error, post_exit, ErrorSource};
use crate::session::{self, Session, SessionInner};
#[cfg(unix)]
use crate::term_mode::{term_mode_snapshot, TermMode};

use crate::writer::WriteQueue;

const INTERRUPTIBLE_SLEEP_STEP: Duration = Duration::from_millis(10);

pub(crate) struct SessionRuntime {
    stop: Arc<AtomicBool>,
    inflight: Arc<(Mutex<u64>, Condvar)>,
    require_acks: bool,
    writer: WriteQueue,
    handle: Option<JoinHandle<()>>,
    wait_handle: Option<JoinHandle<()>>,
    mode_handle: Option<JoinHandle<()>>,
}

pub(crate) fn spawn(options: SessionOptions) -> Result<Box<Session>, PtyxError> {
    let SessionOptions {
        spawn: spawn_config,
        runtime: runtime_config,
    } = options;
    let session = session::spawn(spawn_config)?;
    if let Err(error) = start_runtime(&session, runtime_config) {
        session::close(&session);
        return Err(error);
    }
    Ok(session)
}

pub(crate) fn ack_output(session: &Session, byte_count: u64) -> Result<(), PtyxError> {
    let (require_acks, inflight) = {
        let runtime = session
            .runtime
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "runtime lock poisoned"))?;
        let Some(runtime) = runtime.as_ref() else {
            return Err(PtyxError::new(
                PtyxErrorKind::Closed,
                "session runtime is closed",
            ));
        };
        (runtime.require_acks, Arc::clone(&runtime.inflight))
    };
    if require_acks {
        let (lock, cv) = &*inflight;
        let mut bytes = lock
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "inflight lock poisoned"))?;
        // ACKs can arrive after output is discarded or after a close race.
        // Saturation keeps the counter usable without trusting the caller.
        *bytes = bytes.saturating_sub(byte_count);
        cv.notify_all();
    }
    Ok(())
}

pub(crate) fn write(session: &Session, bytes: &[u8]) -> Result<(), PtyxError> {
    if bytes.is_empty() {
        return Ok(());
    }
    if session.inner.closed.load(Ordering::SeqCst) {
        return Err(PtyxError::new(PtyxErrorKind::Closed, "session is closed"));
    }

    let runtime = session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "runtime lock poisoned"))?;
    let Some(runtime) = runtime.as_ref() else {
        return Err(PtyxError::new(
            PtyxErrorKind::Closed,
            "session runtime is closed",
        ));
    };
    runtime.writer.enqueue_bytes(bytes)?;
    Ok(())
}

pub(crate) fn write_owned(session: &Session, bytes: Vec<u8>) -> Result<(), (PtyxError, Vec<u8>)> {
    if session.inner.closed.load(Ordering::SeqCst) {
        return Err((
            PtyxError::new(PtyxErrorKind::Closed, "session is closed"),
            bytes,
        ));
    }

    let runtime = match session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "runtime lock poisoned"))
    {
        Ok(runtime) => runtime,
        Err(error) => return Err((error, bytes)),
    };
    let Some(runtime) = runtime.as_ref() else {
        return Err((
            PtyxError::new(PtyxErrorKind::Closed, "session runtime is closed"),
            bytes,
        ));
    };

    runtime.writer.enqueue_owned(bytes)
}

fn start_runtime(session: &Session, config: RuntimeConfig) -> Result<(), PtyxError> {
    let mut runtime = session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "runtime lock poisoned"))?;
    if runtime.is_some() {
        return Err(PtyxError::new(
            PtyxErrorKind::Busy,
            "session runtime already started",
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new((Mutex::new(0_u64), Condvar::new()));
    let external_bytes = Arc::new(AtomicUsize::new(0));

    let inner = Arc::clone(&session.inner);
    let stop_for_thread = Arc::clone(&stop);
    let inflight_for_thread = Arc::clone(&inflight);
    let external_bytes_for_thread = Arc::clone(&external_bytes);
    let output_config = config.output;
    let event_port = output_config.event_port;

    let writer = WriteQueue::new(
        Arc::clone(&session.inner),
        event_port,
        config.write_queue_max_bytes,
    );

    let handle = thread::Builder::new()
        .name("ptyx-reader".to_string())
        .spawn(move || {
            crate::reader::run(
                inner,
                stop_for_thread,
                inflight_for_thread,
                external_bytes_for_thread,
                output_config,
            );
        })
        .map_err(|e| {
            writer.close().ok();
            writer.join().ok();
            PtyxError::io(PtyxErrorKind::NativeError, e)
        })?;

    let inner_for_wait = Arc::clone(&session.inner);
    let stop_for_wait = Arc::clone(&stop);
    let wait_handle = match thread::Builder::new()
        .name("ptyx-wait".to_string())
        .spawn(move || {
            wait_loop(inner_for_wait, stop_for_wait, event_port);
        }) {
        Ok(handle) => handle,
        Err(error) => {
            stop.store(true, Ordering::SeqCst);
            writer.close().ok();
            let (_, cv) = &*inflight;
            cv.notify_all();
            handle.join().ok();
            writer.join().ok();
            return Err(PtyxError::io(PtyxErrorKind::NativeError, error));
        }
    };

    let mode_handle = if config.enable_mode_events {
        let inner = Arc::clone(&session.inner);
        let stop_for_mode = Arc::clone(&stop);
        match spawn_mode_watcher(inner, stop_for_mode, config.mode_poll_interval, event_port) {
            Ok(handle) => handle,
            Err(error) => {
                stop.store(true, Ordering::SeqCst);
                writer.close().ok();
                let (_, cv) = &*inflight;
                cv.notify_all();
                handle.join().ok();
                wait_handle.join().ok();
                writer.join().ok();
                return Err(error);
            }
        }
    } else {
        None
    };

    *runtime = Some(SessionRuntime {
        stop,
        inflight,
        require_acks: config.require_acks,
        writer,
        handle: Some(handle),
        wait_handle: Some(wait_handle),
        mode_handle,
    });
    Ok(())
}

#[cfg(unix)]
fn spawn_mode_watcher(
    inner: Arc<SessionInner>,
    stop: Arc<AtomicBool>,
    interval: Duration,
    event_port: i64,
) -> Result<Option<JoinHandle<()>>, PtyxError> {
    let handle = thread::Builder::new()
        .name("ptyx-mode".to_string())
        .spawn(move || {
            mode_loop(inner, stop, interval, event_port);
        })
        .map_err(|error| PtyxError::io(PtyxErrorKind::NativeError, error))?;
    Ok(Some(handle))
}

#[cfg(not(unix))]
fn spawn_mode_watcher(
    inner: Arc<SessionInner>,
    stop: Arc<AtomicBool>,
    interval: Duration,
    event_port: i64,
) -> Result<Option<JoinHandle<()>>, PtyxError> {
    let _ = (inner, stop, interval, event_port);
    Ok(None)
}

fn wait_loop(inner: Arc<SessionInner>, stop: Arc<AtomicBool>, event_port: i64) {
    match crate::process::wait_exit(inner.as_ref()) {
        Ok(exit_code) => {
            if !stop.load(Ordering::SeqCst) {
                post_exit(event_port, exit_code);
            }
        }
        Err(error) => {
            if !stop.load(Ordering::SeqCst) {
                post_error(event_port, ErrorSource::Wait, error);
            }
        }
    }
}

#[cfg(unix)]
fn mode_loop(inner: Arc<SessionInner>, stop: Arc<AtomicBool>, interval: Duration, event_port: i64) {
    let mut last_mode: Option<TermMode> = None;

    while !stop.load(Ordering::SeqCst) {
        match term_mode_snapshot(inner.as_ref()) {
            Ok(mode) => {
                if last_mode.as_ref().is_none_or(|last| *last != mode) {
                    if !post_term_mode(event_port, mode) {
                        break;
                    }
                    last_mode = Some(mode);
                }
            }
            Err(error) => {
                post_error(event_port, ErrorSource::Mode, error);
                break;
            }
        }
        sleep_interruptibly(&stop, interval);
    }
}

fn sleep_interruptibly(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep((deadline - now).min(INTERRUPTIBLE_SLEEP_STEP));
    }
}

pub(crate) fn close_runtime(session: &Session) -> Result<(), PtyxError> {
    let mut slot = session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "runtime lock poisoned"))?;
    let Some(runtime) = slot.as_mut() else {
        return Ok(());
    };
    runtime.stop.store(true, Ordering::SeqCst);
    runtime.writer.close()?;
    {
        let (_, cv) = &*runtime.inflight;
        // Wake the reader if it is blocked on output ACK capacity.
        cv.notify_all();
    }

    if let Some(handle) = runtime.handle.take() {
        handle
            .join()
            .map_err(|_| PtyxError::new(PtyxErrorKind::NativeError, "session runtime panicked"))?;
    }
    if let Some(handle) = runtime.wait_handle.take() {
        handle
            .join()
            .map_err(|_| PtyxError::new(PtyxErrorKind::NativeError, "waiter panicked"))?;
    }
    if let Some(handle) = runtime.mode_handle.take() {
        handle
            .join()
            .map_err(|_| PtyxError::new(PtyxErrorKind::NativeError, "mode watcher panicked"))?;
    }
    runtime.writer.join()?;
    *slot = None;
    Ok(())
}
