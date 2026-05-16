//! Output batching and backpressure.
//!
//! PTY reads are batched before they are posted to the output port. When ACKs
//! are enabled, each posted byte is counted as in flight until the receiver
//! reports that it has drained or discarded the chunk.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::abi::{PTYX_ERROR_SOURCE_OUTPUT, PTYX_STATUS_CLOSED, PTYX_STATUS_ERROR};
use crate::error::PtyxError;
use crate::message::{post_error, post_output_closed, post_output_copied, post_output_external};

const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BACKPRESSURE_WAIT_INTERVAL: Duration = Duration::from_millis(10);

/// Output settings captured when the session runtime starts.
#[derive(Clone, Copy)]
pub(crate) struct OutputConfig {
    pub(crate) require_acks: bool,
    pub(crate) max_inflight: u64,
    pub(crate) read_buffer_size: usize,
    pub(crate) output_batch_max_bytes: usize,
    pub(crate) output_batch_max_delay: Duration,
    pub(crate) use_external_output: bool,
    pub(crate) max_external_output_bytes: u64,
    pub(crate) output_port: i64,
    pub(crate) event_port: i64,
}

pub(crate) struct OutputBuffer {
    config: OutputConfig,
    inflight: Arc<(Mutex<u64>, Condvar)>,
    external_bytes: Arc<AtomicUsize>,
    batch: Vec<u8>,
    first_pending_at: Option<Instant>,
}

impl OutputBuffer {
    pub(crate) fn new(
        config: OutputConfig,
        inflight: Arc<(Mutex<u64>, Condvar)>,
        external_bytes: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            config,
            inflight,
            external_bytes,
            batch: Vec::with_capacity(config.output_batch_max_bytes.min(config.read_buffer_size)),
            first_pending_at: None,
        }
    }

    pub(crate) fn read_buffer_size(&self) -> usize {
        self.config.read_buffer_size
    }

    pub(crate) fn append(&mut self, mut data: &[u8], stop: &AtomicBool) -> Result<(), PtyxError> {
        while !data.is_empty() {
            if self.batch.is_empty() {
                self.first_pending_at = Some(Instant::now());
            }

            let available = self
                .config
                .output_batch_max_bytes
                .saturating_sub(self.batch.len());
            let take = data.len().min(available.max(1));
            self.batch.extend_from_slice(&data[..take]);
            data = &data[take..];

            if self.batch.len() >= self.config.output_batch_max_bytes {
                self.flush(stop)?;
            }
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self, stop: &AtomicBool) -> Result<(), PtyxError> {
        if self.batch.is_empty() {
            self.first_pending_at = None;
            return Ok(());
        }
        if !self.wait_for_inflight_capacity(stop)? {
            return Ok(());
        }

        let len = self.batch.len();
        // The receiver can ACK synchronously after posting succeeds. Reserve
        // before posting so an early ACK cannot be lost.
        self.reserve_inflight(len as u64)?;
        let posted = if self.config.use_external_output
            && reserve_external_bytes(
                &self.external_bytes,
                self.config.max_external_output_bytes,
                len as u64,
            ) {
            let bytes = std::mem::take(&mut self.batch);
            post_output_external(
                self.config.output_port,
                bytes,
                Arc::clone(&self.external_bytes),
            )
        } else {
            let posted =
                post_output_copied(self.config.output_port, self.batch.as_ptr(), len as isize);
            if posted {
                self.batch.clear();
            }
            posted
        };

        if !posted {
            // Posting failed after the reservation. Roll it back so the reader
            // does not wait for capacity the receiver can no longer
            // acknowledge.
            self.release_inflight(len as u64)?;
            return Err(PtyxError::new(PTYX_STATUS_CLOSED, "failed to post output"));
        }

        self.first_pending_at = None;
        Ok(())
    }

    pub(crate) fn poll_timeout(&self) -> Option<Duration> {
        Some(
            self.first_pending_at
                .map(|started| {
                    self.config
                        .output_batch_max_delay
                        .checked_sub(started.elapsed())
                        .unwrap_or(Duration::ZERO)
                })
                .unwrap_or(IDLE_POLL_INTERVAL),
        )
    }

    pub(crate) fn should_flush_for_delay(&self) -> bool {
        self.first_pending_at
            .is_some_and(|started| started.elapsed() >= self.config.output_batch_max_delay)
    }

    pub(crate) fn close_ports(&self) {
        post_output_closed(self.config.output_port);
    }

    pub(crate) fn flush_and_post_error(&mut self, stop: &AtomicBool, error: PtyxError) {
        self.flush(stop).ok();
        self.post_error(error);
    }

    pub(crate) fn post_error(&self, error: PtyxError) {
        post_error(self.config.event_port, PTYX_ERROR_SOURCE_OUTPUT, error);
    }

    fn wait_for_inflight_capacity(&self, stop: &AtomicBool) -> Result<bool, PtyxError> {
        if !self.config.require_acks {
            return Ok(true);
        }

        let (lock, cv) = &*self.inflight;
        let mut bytes = lock
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "inflight lock poisoned"))?;
        while *bytes >= self.config.max_inflight && !stop.load(Ordering::SeqCst) {
            let result = cv.wait_timeout(bytes, BACKPRESSURE_WAIT_INTERVAL);
            let Ok((guard, _)) = result else {
                return Err(PtyxError::new(PTYX_STATUS_ERROR, "inflight lock poisoned"));
            };
            bytes = guard;
        }
        Ok(!stop.load(Ordering::SeqCst))
    }

    fn reserve_inflight(&self, len: u64) -> Result<(), PtyxError> {
        if !self.config.require_acks {
            return Ok(());
        }

        let (lock, _) = &*self.inflight;
        let mut bytes = lock
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "inflight lock poisoned"))?;
        *bytes = bytes.saturating_add(len);
        Ok(())
    }

    fn release_inflight(&self, len: u64) -> Result<(), PtyxError> {
        if !self.config.require_acks {
            return Ok(());
        }

        let (lock, cv) = &*self.inflight;
        let mut bytes = lock
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "inflight lock poisoned"))?;
        *bytes = bytes.saturating_sub(len);
        cv.notify_all();
        Ok(())
    }
}

fn reserve_external_bytes(bytes: &AtomicUsize, max: u64, len: u64) -> bool {
    let Ok(len) = usize::try_from(len) else {
        return false;
    };
    let max = usize::try_from(max).unwrap_or(usize::MAX);
    if len == 0 || len > max {
        return false;
    }
    bytes
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(len).filter(|next| *next <= max)
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(require_acks: bool) -> OutputConfig {
        OutputConfig {
            require_acks,
            max_inflight: 1024,
            read_buffer_size: 1024,
            output_batch_max_bytes: 1024,
            output_batch_max_delay: Duration::from_millis(1),
            use_external_output: false,
            max_external_output_bytes: 0,
            output_port: 0,
            event_port: 0,
        }
    }

    #[test]
    fn flush_rolls_back_reserved_inflight_bytes_when_post_fails() {
        let inflight = Arc::new((Mutex::new(0), Condvar::new()));
        let external_bytes = Arc::new(AtomicUsize::new(0));
        let stop = AtomicBool::new(false);
        let mut output = OutputBuffer::new(config(true), Arc::clone(&inflight), external_bytes);

        output.append(b"abc", &stop).unwrap();

        assert_eq!(output.flush(&stop).unwrap_err().status, PTYX_STATUS_CLOSED);
        assert_eq!(*inflight.0.lock().unwrap(), 0);
    }
}
