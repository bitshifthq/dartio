//! Asynchronous writes into the PTY.
//!
//! ABI calls enqueue bytes quickly. A dedicated writer thread preserves chunk
//! order, retries transient readiness failures, and reports permanent write
//! errors through the event port.

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::abi::{
    PTYX_ERROR_SOURCE_WRITE, PTYX_STATUS_BROKEN_PIPE, PTYX_STATUS_CLOSED, PTYX_STATUS_ERROR,
    PTYX_STATUS_IO_FAILED, PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_OUT_OF_MEMORY,
    PTYX_STATUS_WOULD_BLOCK,
};
use crate::error::PtyxError;
use crate::fd::{write_retry_context_for_inner, WriteRetryContext};
use crate::message::post_error;
use crate::session::{BusyGuard, SessionInner};

const WRITE_RETRY_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct WriteQueue {
    inner: Arc<SessionInner>,
    event_port: i64,
    shared: Arc<WriteQueueShared>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl WriteQueue {
    pub(crate) fn new(inner: Arc<SessionInner>, event_port: i64, max_bytes: usize) -> Self {
        Self {
            inner,
            event_port,
            shared: Arc::new(WriteQueueShared::new(max_bytes)),
            handle: Mutex::new(None),
        }
    }

    pub(crate) fn enqueue_bytes(&self, bytes: &[u8]) -> Result<(), PtyxError> {
        self.ensure_started()?;
        self.shared.enqueue_bytes(bytes)
    }

    pub(crate) fn enqueue_owned(&self, bytes: Vec<u8>) -> Result<(), (PtyxError, Vec<u8>)> {
        if let Err(error) = self.ensure_started() {
            return Err((error, bytes));
        }
        self.shared.enqueue_owned(bytes)
    }

    pub(crate) fn close(&self) -> Result<(), PtyxError> {
        self.shared.close()
    }

    pub(crate) fn join(&self) -> Result<(), PtyxError> {
        let handle = self
            .handle
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "writer handle lock poisoned"))?
            .take();
        let Some(handle) = handle else {
            return Ok(());
        };
        handle
            .join()
            .map_err(|_| PtyxError::new(PTYX_STATUS_NATIVE_ERROR, "writer panicked"))
    }

    fn ensure_started(&self) -> Result<(), PtyxError> {
        let mut handle = self
            .handle
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "writer handle lock poisoned"))?;
        if handle.is_some() {
            return Ok(());
        }

        let inner = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.shared);
        let event_port = self.event_port;
        *handle = Some(
            thread::Builder::new()
                .name("ptyx-writer".to_string())
                .spawn(move || writer_loop(inner, shared, event_port))
                .map_err(|error| PtyxError::io(PTYX_STATUS_NATIVE_ERROR, error))?,
        );
        Ok(())
    }
}

struct WriteQueueShared {
    state: Mutex<WriteQueueState>,
    ready: Condvar,
    max_bytes: usize,
}

struct WriteQueueState {
    /// Chunks are never split while queued, so one caller write stays
    /// contiguous when the writer thread drains it.
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    closed: bool,
    failure: Option<PtyxError>,
}

impl WriteQueueShared {
    fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(WriteQueueState {
                queue: VecDeque::new(),
                queued_bytes: 0,
                closed: false,
                failure: None,
            }),
            ready: Condvar::new(),
            max_bytes,
        }
    }

    fn enqueue_bytes(&self, bytes: &[u8]) -> Result<(), PtyxError> {
        if bytes.is_empty() {
            return Ok(());
        }

        let byte_count = bytes.len();
        if byte_count > self.max_bytes {
            return Err(queue_full_error());
        }

        self.check_enqueue_allowed(byte_count)?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(byte_count)
            .map_err(|_| allocation_error())?;
        copy.extend_from_slice(bytes);
        self.enqueue_owned(copy).map_err(|(error, _)| error)?;
        Ok(())
    }

    fn enqueue_owned(&self, bytes: Vec<u8>) -> Result<(), (PtyxError, Vec<u8>)> {
        if bytes.is_empty() {
            return Ok(());
        }
        let byte_count = bytes.len();
        if byte_count > self.max_bytes {
            return Err((queue_full_error(), bytes));
        }

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Err((
                    PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"),
                    bytes,
                ))
            }
        };
        if let Err(error) = self.validate_enqueue(&state, byte_count) {
            return Err((error, bytes));
        }
        state.queued_bytes += byte_count;
        state.queue.push_back(bytes);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    fn check_enqueue_allowed(&self, byte_count: usize) -> Result<(), PtyxError> {
        let state = self
            .state
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        self.validate_enqueue(&state, byte_count)
    }

    fn validate_enqueue(
        &self,
        state: &WriteQueueState,
        byte_count: usize,
    ) -> Result<(), PtyxError> {
        if let Some(error) = state.failure.clone() {
            return Err(error);
        }
        if state.closed {
            return Err(PtyxError::new(
                PTYX_STATUS_CLOSED,
                "session runtime is closed",
            ));
        }
        if state.queued_bytes.saturating_add(byte_count) > self.max_bytes {
            return Err(queue_full_error());
        }

        Ok(())
    }

    fn dequeue(&self) -> Result<Option<Vec<u8>>, PtyxError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        loop {
            if let Some(chunk) = state.queue.pop_front() {
                state.queued_bytes = state.queued_bytes.saturating_sub(chunk.len());
                return Ok(Some(chunk));
            }
            if let Some(error) = state.failure.clone() {
                return Err(error);
            }
            if state.closed {
                return Ok(None);
            }
            state = self
                .ready
                .wait(state)
                .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        }
    }

    fn close(&self) -> Result<(), PtyxError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        state.closed = true;
        state.queue.clear();
        state.queued_bytes = 0;
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    fn fail(&self, error: PtyxError) -> Result<(), PtyxError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        state.failure = Some(error);
        state.queue.clear();
        state.queued_bytes = 0;
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    fn is_closed(&self) -> Result<bool, PtyxError> {
        let state = self
            .state
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "write queue lock poisoned"))?;
        Ok(state.closed)
    }
}

fn writer_loop(inner: Arc<SessionInner>, shared: Arc<WriteQueueShared>, event_port: i64) {
    let write_context = match write_retry_context_for_inner(inner.as_ref()) {
        Ok(context) => context,
        Err(error) => {
            record_failure(&shared, event_port, error);
            return;
        }
    };

    loop {
        let chunk = match shared.dequeue() {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return,
            Err(error) => {
                post_error(event_port, PTYX_ERROR_SOURCE_WRITE, error);
                return;
            }
        };

        if inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
            record_failure(
                &shared,
                event_port,
                PtyxError::new(PTYX_STATUS_CLOSED, "session is closed"),
            );
            return;
        }

        let result = (|| {
            let _guard = BusyGuard::enter(&inner.write_busy)?;
            #[cfg(unix)]
            if let Some(fd) = write_context.raw_fd() {
                // The raw fd path avoids blocking on a trait object write when
                // poll can tell us the PTY is not ready yet.
                return write_fd_chunk(fd, &chunk, &write_context, &shared);
            }
            let mut writer = inner
                .writer
                .lock()
                .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "writer lock poisoned"))?;
            write_chunk(&mut *writer, &chunk, &write_context, &shared)
        })();

        match result {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                record_failure(&shared, event_port, normalize_write_error(error));
                return;
            }
        }
    }
}

fn write_chunk(
    writer: &mut dyn Write,
    mut data: &[u8],
    context: &WriteRetryContext,
    queue: &WriteQueueShared,
) -> Result<bool, PtyxError> {
    while !data.is_empty() {
        if queue.is_closed()? {
            return Ok(false);
        }
        match writer.write(data) {
            Ok(0) => {
                return Err(PtyxError::io(
                    PTYX_STATUS_IO_FAILED,
                    std::io::Error::new(ErrorKind::WriteZero, "failed to write to PTY"),
                ));
            }
            Ok(n) => data = &data[n..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if !context
                    .wait_writable_for(WRITE_RETRY_INTERVAL)
                    .map_err(|error| map_write_error(error, PTYX_STATUS_IO_FAILED))?
                {
                    continue;
                }
            }
            Err(error) => return Err(map_write_error(error, PTYX_STATUS_IO_FAILED)),
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn write_fd_chunk(
    fd: libc::c_int,
    mut data: &[u8],
    context: &WriteRetryContext,
    queue: &WriteQueueShared,
) -> Result<bool, PtyxError> {
    while !data.is_empty() {
        if queue.is_closed()? {
            return Ok(false);
        }
        // `data` is borrowed for this call and the fd is owned by the session.
        let result = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if result > 0 {
            data = &data[result as usize..];
            continue;
        }
        if result == 0 {
            return Err(PtyxError::io(
                PTYX_STATUS_IO_FAILED,
                std::io::Error::new(ErrorKind::WriteZero, "failed to write to PTY"),
            ));
        }

        let error = std::io::Error::last_os_error();
        match error.kind() {
            ErrorKind::Interrupted => {}
            ErrorKind::WouldBlock => {
                if !context
                    .wait_writable_for(WRITE_RETRY_INTERVAL)
                    .map_err(|error| map_write_error(error, PTYX_STATUS_IO_FAILED))?
                {
                    continue;
                }
            }
            _ => return Err(map_write_error(error, PTYX_STATUS_IO_FAILED)),
        }
    }
    Ok(true)
}

fn normalize_write_error(error: PtyxError) -> PtyxError {
    if error.status == PTYX_STATUS_BROKEN_PIPE {
        return PtyxError::new(PTYX_STATUS_CLOSED, error.message);
    }
    error
}

fn map_write_error(error: std::io::Error, fallback: u32) -> PtyxError {
    match error.kind() {
        ErrorKind::BrokenPipe => PtyxError::new(PTYX_STATUS_BROKEN_PIPE, error.to_string()),
        ErrorKind::WouldBlock => PtyxError::new(PTYX_STATUS_WOULD_BLOCK, error.to_string()),
        ErrorKind::PermissionDenied => {
            PtyxError::new(crate::abi::PTYX_STATUS_PERMISSION_DENIED, error.to_string())
        }
        _ => PtyxError::new(fallback, error.to_string()),
    }
}

fn record_failure(shared: &WriteQueueShared, event_port: i64, error: PtyxError) {
    shared.fail(error.clone()).ok();
    post_error(event_port, PTYX_ERROR_SOURCE_WRITE, error);
}

fn queue_full_error() -> PtyxError {
    PtyxError::new(PTYX_STATUS_WOULD_BLOCK, "PTY write queue is full")
}

fn allocation_error() -> PtyxError {
    PtyxError::new(PTYX_STATUS_OUT_OF_MEMORY, "failed to allocate write chunk")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_preserves_fifo_order() {
        let queue = WriteQueueShared::new(16);

        queue.enqueue_bytes(&[1, 2]).unwrap();
        queue.enqueue_bytes(&[3]).unwrap();

        assert_eq!(queue.dequeue().unwrap(), Some(vec![1, 2]));
        assert_eq!(queue.dequeue().unwrap(), Some(vec![3]));
    }

    #[test]
    fn queue_keeps_large_writes_contiguous() {
        let queue = WriteQueueShared::new(128 * 1024);
        let bytes = vec![7; 64 * 1024 + 1];

        queue.enqueue_bytes(&bytes).unwrap();

        assert_eq!(queue.dequeue().unwrap().unwrap(), bytes);
    }

    #[test]
    fn queue_rejects_writes_larger_than_limit() {
        let queue = WriteQueueShared::new(2);

        let error = queue.enqueue_bytes(&[1, 2, 3]).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_WOULD_BLOCK);
    }

    #[test]
    fn queue_rejects_writes_when_full() {
        let queue = WriteQueueShared::new(3);
        queue.enqueue_bytes(&[1, 2]).unwrap();

        let error = queue.enqueue_bytes(&[3, 4]).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_WOULD_BLOCK);
    }

    #[test]
    fn queue_rejects_writes_after_close() {
        let queue = WriteQueueShared::new(16);
        queue.close().unwrap();

        let error = queue.enqueue_bytes(&[1]).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_CLOSED);
    }

    #[test]
    fn queue_rejects_writes_after_failure() {
        let queue = WriteQueueShared::new(16);
        queue
            .fail(PtyxError::new(PTYX_STATUS_IO_FAILED, "write failed"))
            .unwrap();

        let error = queue.enqueue_bytes(&[1]).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_IO_FAILED);
    }
}
