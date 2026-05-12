//! Session runtime threads and native port integration.
//!
//! The runtime starts one reader thread, one waiter thread, one writer thread,
//! and, when enabled, a terminal mode watcher. Shutdown is coordinated through
//! a shared stop flag so close can interrupt backpressure waits and polling.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::abi::ptyx_term_mode_t;
use crate::abi::{
    ptyx_session_options_t, session_options_from_ptr, DEFAULT_MAX_EXTERNAL_OUTPUT_BYTES,
    DEFAULT_MAX_INFLIGHT_BYTES, DEFAULT_MODE_POLL_INTERVAL_MS, DEFAULT_OUTPUT_BATCH_DELAY_US,
    DEFAULT_OUTPUT_BATCH_MAX_BYTES, DEFAULT_READ_BUFFER_SIZE, DEFAULT_WRITE_QUEUE_MAX_BYTES, KIB,
    MIB, PTYX_ERROR_SOURCE_MODE, PTYX_ERROR_SOURCE_WAIT, PTYX_SESSION_ENABLE_MODE_EVENTS,
    PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA, PTYX_SESSION_REQUIRE_OUTPUT_ACKS,
    PTYX_SESSION_SUPPORTED_FLAGS, PTYX_STATUS_CLOSED, PTYX_STATUS_ERROR,
    PTYX_STATUS_INVALID_ARGUMENT, PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_OK,
    PTYX_STATUS_UNSUPPORTED,
};
use crate::dart_api;
use crate::error::{ffi_status, PtyxError};
#[cfg(unix)]
use crate::message::post_term_mode;
use crate::message::{post_error, post_exit};
use crate::output::OutputConfig;
use crate::owned_buffer::ptyx_owned_buffer;
use crate::session::{self, ptyx_session, session_ref, SessionInner};
#[cfg(unix)]
use crate::term_mode::{same_observed_mode, term_mode_snapshot};

use crate::writer::WriteQueue;

const MIN_OUTPUT_BATCH_DELAY_US: u32 = 1;
const MAX_OUTPUT_BATCH_DELAY_US: u32 = 1_000_000;
const MIN_MODE_POLL_INTERVAL_MS: u32 = 25;
const MAX_MODE_POLL_INTERVAL_MS: u32 = 60_000;
const MIN_READ_BUFFER_SIZE: u32 = (4 * KIB) as u32;
const MAX_READ_BUFFER_SIZE: u32 = MIB as u32;
const MIN_OUTPUT_BATCH_MAX_BYTES: u32 = (64 * KIB) as u32;
const MAX_OUTPUT_BATCH_MAX_BYTES: u32 = (256 * KIB) as u32;
const MIN_WRITE_QUEUE_MAX_BYTES: usize = 64 * KIB;
const MAX_WRITE_QUEUE_MAX_BYTES: usize = 512 * MIB;
const INTERRUPTIBLE_SLEEP_STEP: Duration = Duration::from_millis(10);

struct RuntimeConfig {
    output: OutputConfig,
    require_acks: bool,
    enable_mode_events: bool,
    mode_poll_interval: Duration,
    write_queue_max_bytes: usize,
}

impl RuntimeConfig {
    fn from_options(options: ptyx_session_options_t) -> Self {
        let output_batch_max_delay_us = if options.output_batch_max_delay_us == 0 {
            DEFAULT_OUTPUT_BATCH_DELAY_US
        } else {
            options
                .output_batch_max_delay_us
                .clamp(MIN_OUTPUT_BATCH_DELAY_US, MAX_OUTPUT_BATCH_DELAY_US)
        };

        let mode_poll_interval_ms = if options.mode_poll_interval_ms == 0 {
            DEFAULT_MODE_POLL_INTERVAL_MS
        } else {
            options
                .mode_poll_interval_ms
                .clamp(MIN_MODE_POLL_INTERVAL_MS, MAX_MODE_POLL_INTERVAL_MS)
        };

        let require_acks = options.flags & PTYX_SESSION_REQUIRE_OUTPUT_ACKS != 0;
        Self {
            output: OutputConfig {
                require_acks,
                max_inflight: default_u64(options.max_inflight_bytes, DEFAULT_MAX_INFLIGHT_BYTES),
                read_buffer_size: default_u32(options.read_buffer_size, DEFAULT_READ_BUFFER_SIZE)
                    .clamp(MIN_READ_BUFFER_SIZE, MAX_READ_BUFFER_SIZE)
                    as usize,
                output_batch_max_bytes: default_u32(
                    options.output_batch_max_bytes,
                    DEFAULT_OUTPUT_BATCH_MAX_BYTES,
                )
                .clamp(MIN_OUTPUT_BATCH_MAX_BYTES, MAX_OUTPUT_BATCH_MAX_BYTES)
                    as usize,
                output_batch_max_delay: Duration::from_micros(output_batch_max_delay_us as u64),
                use_external_output: options.flags & PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA != 0,
                max_external_output_bytes: default_u64(
                    options.max_external_output_bytes,
                    DEFAULT_MAX_EXTERNAL_OUTPUT_BYTES,
                ),
                output_port: options.output_port,
                event_port: options.event_port,
            },
            require_acks,
            enable_mode_events: options.flags & PTYX_SESSION_ENABLE_MODE_EVENTS != 0,
            mode_poll_interval: Duration::from_millis(mode_poll_interval_ms as u64),
            write_queue_max_bytes: default_u64(
                options.write_queue_max_bytes,
                DEFAULT_WRITE_QUEUE_MAX_BYTES as u64,
            )
            .try_into()
            .unwrap_or(usize::MAX)
            .clamp(MIN_WRITE_QUEUE_MAX_BYTES, MAX_WRITE_QUEUE_MAX_BYTES),
        }
    }
}

fn default_u32(value: u32, default: u32) -> u32 {
    if value == 0 {
        default
    } else {
        value
    }
}

fn default_u64(value: u64, default: u64) -> u64 {
    if value == 0 {
        default
    } else {
        value
    }
}

pub(crate) struct SessionRuntime {
    stop: Arc<AtomicBool>,
    inflight: Arc<(Mutex<u64>, Condvar)>,
    require_acks: bool,
    writer: WriteQueue,
    handle: Option<JoinHandle<()>>,
    wait_handle: Option<JoinHandle<()>>,
    mode_handle: Option<JoinHandle<()>>,
}

pub(crate) fn spawn(
    session_options: *const ptyx_session_options_t,
    out_session: *mut *mut ptyx_session,
) -> u32 {
    ffi_status(|| {
        if !dart_api::is_initialized_bool() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "ptyx_init must be called before ptyx_spawn",
            ));
        }
        if session_options.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "session_options must not be null",
            ));
        }
        let options = session_options_from_ptr(session_options)?;
        if options.flags & !PTYX_SESSION_SUPPORTED_FLAGS != 0 {
            return Err(PtyxError::new(
                PTYX_STATUS_UNSUPPORTED,
                "session option flags are unsupported",
            ));
        }
        if options.output_port == 0 || options.event_port == 0 {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "output and event ports must be valid",
            ));
        }
        if out_session.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "out_session must not be null",
            ));
        }
        // Leave a deterministic null out pointer on every failure path.
        unsafe { *out_session = std::ptr::null_mut() };

        let raw_session = Box::into_raw(session::spawn(options)?);
        // `raw_session` was just created from a Box and remains owned here
        // until runtime startup succeeds.
        if let Err(error) = start_runtime(unsafe { &*raw_session }, options) {
            session::free_failed_session(raw_session);
            return Err(error);
        }
        // Runtime startup succeeded, so ownership of the raw session moves to
        // the C caller until `ptyx_close`.
        unsafe { *out_session = raw_session };
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn ack_output(session: *mut ptyx_session, byte_count: u64) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        let (require_acks, inflight) = {
            let runtime = session
                .runtime
                .lock()
                .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "runtime lock poisoned"))?;
            let Some(runtime) = runtime.as_ref() else {
                return Err(PtyxError::new(
                    PTYX_STATUS_CLOSED,
                    "session runtime is closed",
                ));
            };
            (runtime.require_acks, Arc::clone(&runtime.inflight))
        };
        if require_acks {
            let (lock, cv) = &*inflight;
            let mut bytes = lock
                .lock()
                .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "inflight lock poisoned"))?;
            // ACKs can arrive after output is discarded or after a close race.
            // Saturation keeps the counter usable without trusting the caller.
            *bytes = bytes.saturating_sub(byte_count);
            cv.notify_all();
        }
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn write(session: *mut ptyx_session, data: *const c_void, length: usize) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if length > 0 && data.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "data must not be null when length is non-zero",
            ));
        }
        if length == 0 {
            return Ok(PTYX_STATUS_OK);
        }
        if session.inner.closed.load(Ordering::SeqCst) {
            return Err(PtyxError::new(PTYX_STATUS_CLOSED, "session is closed"));
        }

        // The pointer is non-null for a non-zero write and valid for `length`
        // bytes for the duration of this call.
        let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
        let runtime = session
            .runtime
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "runtime lock poisoned"))?;
        let Some(runtime) = runtime.as_ref() else {
            return Err(PtyxError::new(
                PTYX_STATUS_CLOSED,
                "session runtime is closed",
            ));
        };
        runtime.writer.enqueue_bytes(slice)?;
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn write_owned(
    session: *mut ptyx_session,
    buffer: *mut ptyx_owned_buffer,
    length: usize,
) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if session.inner.closed.load(Ordering::SeqCst) {
            return Err(PtyxError::new(PTYX_STATUS_CLOSED, "session is closed"));
        }

        let mut buffer = crate::owned_buffer::take(buffer, length)?;
        let runtime = session
            .runtime
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "runtime lock poisoned"))?;
        let Some(runtime) = runtime.as_ref() else {
            // Return ownership to the caller because the queue did not accept
            // the buffer.
            let _ = Box::into_raw(buffer);
            return Err(PtyxError::new(
                PTYX_STATUS_CLOSED,
                "session runtime is closed",
            ));
        };

        let bytes = std::mem::take(&mut buffer.bytes);
        match runtime.writer.enqueue_owned(bytes) {
            Ok(()) => Ok(PTYX_STATUS_OK),
            Err((error, bytes)) => {
                buffer.bytes = bytes;
                // Return ownership to the caller on enqueue failure.
                let _ = Box::into_raw(buffer);
                Err(error)
            }
        }
    })
}

fn start_runtime(session: &ptyx_session, options: ptyx_session_options_t) -> Result<(), PtyxError> {
    let mut runtime = session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "runtime lock poisoned"))?;
    if runtime.is_some() {
        return Err(PtyxError::new(
            crate::abi::PTYX_STATUS_BUSY,
            "session runtime already started",
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new((Mutex::new(0_u64), Condvar::new()));
    let external_bytes = Arc::new(AtomicU64::new(0));
    let config = RuntimeConfig::from_options(options);

    let inner = Arc::clone(&session.inner);
    let stop_for_thread = Arc::clone(&stop);
    let inflight_for_thread = Arc::clone(&inflight);
    let external_bytes_for_thread = Arc::clone(&external_bytes);
    let output_config = config.output;
    let event_port = options.event_port;

    let mut writer = WriteQueue::start(
        Arc::clone(&session.inner),
        event_port,
        config.write_queue_max_bytes,
    )?;

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
            PtyxError::io(PTYX_STATUS_NATIVE_ERROR, e)
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
            return Err(PtyxError::io(PTYX_STATUS_NATIVE_ERROR, error));
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
        .map_err(|error| PtyxError::io(PTYX_STATUS_NATIVE_ERROR, error))?;
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
                post_error(event_port, PTYX_ERROR_SOURCE_WAIT, error);
            }
        }
    }
}

#[cfg(unix)]
fn mode_loop(inner: Arc<SessionInner>, stop: Arc<AtomicBool>, interval: Duration, event_port: i64) {
    let mut last_mode: Option<ptyx_term_mode_t> = None;

    while !stop.load(Ordering::SeqCst) {
        match term_mode_snapshot(inner.as_ref()) {
            Ok(mode) => {
                if last_mode
                    .as_ref()
                    .is_none_or(|last| !same_observed_mode(last, &mode))
                {
                    if !post_term_mode(event_port, mode) {
                        break;
                    }
                    last_mode = Some(mode);
                }
            }
            Err(error) => {
                post_error(event_port, PTYX_ERROR_SOURCE_MODE, error);
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

pub(crate) fn close_runtime(session: &ptyx_session) -> Result<u32, PtyxError> {
    let mut slot = session
        .runtime
        .lock()
        .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "runtime lock poisoned"))?;
    let Some(runtime) = slot.as_mut() else {
        return Ok(PTYX_STATUS_OK);
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
            .map_err(|_| PtyxError::new(PTYX_STATUS_NATIVE_ERROR, "session runtime panicked"))?;
    }
    if let Some(handle) = runtime.wait_handle.take() {
        handle
            .join()
            .map_err(|_| PtyxError::new(PTYX_STATUS_NATIVE_ERROR, "waiter panicked"))?;
    }
    if let Some(handle) = runtime.mode_handle.take() {
        handle
            .join()
            .map_err(|_| PtyxError::new(PTYX_STATUS_NATIVE_ERROR, "mode watcher panicked"))?;
    }
    runtime.writer.join()?;
    *slot = None;
    Ok(PTYX_STATUS_OK)
}
