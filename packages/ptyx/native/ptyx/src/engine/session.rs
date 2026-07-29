use crate::engine::oneshot::Sender as ReplySender;
use crate::engine::CloseResult;
use bytes::{Buf, Bytes, BytesMut};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_INPUT_ENTRIES: usize = 4096;
// This bounds a maximum-capacity session to 4096 queued output allocations
// without copying the common case where the OS already returns a larger chunk.
const OUTPUT_CHUNK_TARGET: usize = 16 * 1024;

pub(crate) struct QueuedInput {
    pub(crate) bytes: Bytes,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) offset: usize,
}

pub(crate) struct QueuedOutput {
    pub(crate) bytes: Bytes,
    pub(crate) offset: usize,
}

pub(crate) struct InputAdmission {
    pub(crate) capacity: usize,
    pub(crate) state: Mutex<InputAdmissionState>,
}

pub(crate) struct InputAdmissionState {
    pub(crate) bytes: usize,
    pub(crate) entries: usize,
    pub(crate) open: bool,
}

impl InputAdmission {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(InputAdmissionState {
                bytes: 0,
                entries: 0,
                open: true,
            }),
        }
    }

    pub(crate) fn entry_capacity(&self) -> usize {
        self.capacity.min(MAX_INPUT_ENTRIES)
    }

    pub(crate) fn release(&self, bytes: usize, entries: usize) {
        if let Ok(mut state) = self.state.lock() {
            debug_assert!(bytes <= state.bytes);
            debug_assert!(entries <= state.entries);
            state.bytes = state.bytes.saturating_sub(bytes);
            state.entries = state.entries.saturating_sub(entries);
        }
    }

    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.open = false;
        }
    }
}

pub(crate) struct SessionCore {
    pub(crate) admission: Arc<InputAdmission>,
    pub(crate) input_bytes: usize,
    pub(crate) input_entries: usize,
    pub(crate) input: VecDeque<QueuedInput>,
    pub(crate) input_failed: bool,
    pub(crate) input_failure_pending: bool,
    pub(crate) input_failure_notified: bool,
    pub(crate) output_capacity: usize,
    pub(crate) output_bytes: usize,
    pub(crate) output: VecDeque<QueuedOutput>,
    output_tail: BytesMut,
    pub(crate) output_outstanding: usize,
    pub(crate) output_lease_bytes: usize,
    pub(crate) output_deadline: Option<Instant>,
    pub(crate) output_done_notified: bool,
    pub(crate) output_failed: bool,
    pub(crate) paused: bool,
    pub(crate) discarding: bool,
    pub(crate) output_eof: bool,
    pub(crate) exit_status: Option<i64>,
    pub(crate) close_started: bool,
    pub(crate) cleanup_failed: bool,
    pub(crate) close_notified: bool,
    pub(crate) close_waiters: Vec<ReplySender<CloseResult>>,
    pub(crate) active: bool,
    pub(crate) activation_deadline: Option<Instant>,
    pub(crate) abandoned: bool,
}

impl SessionCore {
    pub(crate) fn new(admission: Arc<InputAdmission>, output_capacity: usize) -> Self {
        Self {
            admission,
            input_bytes: 0,
            input_entries: 0,
            input: VecDeque::new(),
            input_failed: false,
            input_failure_pending: false,
            input_failure_notified: false,
            output_capacity,
            output_bytes: 0,
            output: VecDeque::new(),
            output_tail: BytesMut::new(),
            output_outstanding: 0,
            output_lease_bytes: 0,
            output_deadline: None,
            output_done_notified: false,
            output_failed: false,
            paused: true,
            discarding: false,
            output_eof: false,
            exit_status: None,
            close_started: false,
            cleanup_failed: false,
            close_notified: false,
            close_waiters: Vec::new(),
            active: false,
            activation_deadline: None,
            abandoned: false,
        }
    }

    pub(crate) fn enqueue_write(&mut self, bytes: Bytes) -> Result<(), Bytes> {
        if self.close_started || self.input_failed || bytes.is_empty() {
            return Err(bytes);
        }
        self.input_bytes += bytes.len();
        self.input_entries += 1;
        self.input.push_back(QueuedInput {
            bytes,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            offset: 0,
        });
        Ok(())
    }

    pub(crate) fn pull_output(&mut self, maximum: usize) -> Bytes {
        self.flush_output_tail();
        let amount = maximum.min(self.output_bytes);
        if self
            .output
            .front()
            .is_some_and(|front| front.offset == 0 && front.bytes.len() == amount)
        {
            let bytes = self
                .output
                .pop_front()
                .expect("output byte count is exact")
                .bytes;
            self.output_bytes -= bytes.len();
            self.output_outstanding += bytes.len();
            if self.output.is_empty() {
                self.output_deadline = None;
            }
            return bytes;
        }
        let mut bytes = BytesMut::with_capacity(amount);
        while bytes.len() < amount {
            let front = self.output.front_mut().expect("output byte count is exact");
            let available = front.bytes.len() - front.offset;
            let take = available.min(amount - bytes.len());
            bytes.extend_from_slice(&front.bytes[front.offset..front.offset + take]);
            front.offset += take;
            if front.offset == front.bytes.len() {
                self.output.pop_front();
            }
        }
        self.output_bytes -= bytes.len();
        self.output_outstanding += bytes.len();
        if self.output.is_empty() {
            self.output_deadline = None;
        }
        bytes.freeze()
    }

    pub(crate) fn credit(&mut self, bytes: usize) -> bool {
        if bytes > self.output_outstanding {
            return false;
        }
        self.output_outstanding -= bytes;
        self.output_lease_bytes = self.output_lease_bytes.saturating_sub(bytes);
        true
    }

    pub(crate) fn enqueue_output(&mut self, mut bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        self.output_bytes += bytes.len();
        if !self.output_tail.is_empty() {
            let amount = (OUTPUT_CHUNK_TARGET - self.output_tail.len()).min(bytes.len());
            self.output_tail.extend_from_slice(&bytes[..amount]);
            bytes.advance(amount);
            if self.output_tail.len() == OUTPUT_CHUNK_TARGET {
                self.flush_output_tail();
            }
        }
        if bytes.len() >= OUTPUT_CHUNK_TARGET {
            self.output.push_back(QueuedOutput { bytes, offset: 0 });
        } else if !bytes.is_empty() {
            self.output_tail.extend_from_slice(&bytes);
        }
    }

    pub(crate) fn clear_output(&mut self) {
        self.output.clear();
        self.output_tail = BytesMut::new();
        self.output_bytes = 0;
    }

    pub(crate) fn forget_output(&mut self) {
        self.clear_output();
        self.output_outstanding = 0;
        self.output_lease_bytes = 0;
    }

    pub(crate) fn has_output(&self) -> bool {
        self.output_bytes != 0
    }

    pub(crate) fn mark_output_leased(&mut self, bytes: usize) {
        debug_assert_eq!(self.output_lease_bytes, 0);
        self.output_lease_bytes = bytes;
    }

    pub(crate) fn output_total(&self) -> usize {
        self.output_bytes + self.output_outstanding
    }

    fn flush_output_tail(&mut self) {
        if !self.output_tail.is_empty() {
            self.output.push_back(QueuedOutput {
                bytes: self.output_tail.split().freeze(),
                offset: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputAdmission, SessionCore, OUTPUT_CHUNK_TARGET};
    use bytes::Bytes;
    use std::sync::Arc;

    fn session(output_capacity: usize) -> SessionCore {
        SessionCore::new(Arc::new(InputAdmission::new(1024)), output_capacity)
    }

    #[test]
    fn one_byte_paused_output_has_a_bounded_entry_count() {
        let capacity = 1024 * 1024;
        let mut session = session(capacity);

        for _ in 0..capacity {
            session.enqueue_output(Bytes::from_static(&[0x5a]));
        }

        let entries = session.output.len() + usize::from(!session.output_tail.is_empty());
        assert!(entries <= capacity.div_ceil(OUTPUT_CHUNK_TARGET));
        assert_eq!(session.output_total(), capacity);

        let mut drained = 0;
        while session.has_output() {
            let bytes = session.pull_output(64 * 1024);
            assert!(bytes.iter().all(|byte| *byte == 0x5a));
            drained += bytes.len();
            assert!(session.credit(bytes.len()));
        }
        assert_eq!(drained, capacity);
        assert_eq!(session.output_total(), 0);
    }

    #[test]
    fn partial_credit_does_not_release_the_output_lease() {
        let capacity = 1024 * 1024 + 2;
        let mut session = session(capacity);
        session.enqueue_output(Bytes::from_static(&[0x11, 0x12]));
        let leased = session.pull_output(2);
        session.mark_output_leased(leased.len());

        for _ in 0..1024 * 1024 {
            session.enqueue_output(Bytes::from_static(&[0x22]));
        }

        assert_eq!(session.output_lease_bytes, 2);
        assert!(session.credit(1));
        assert_eq!(session.output_lease_bytes, 1);
        assert!(session.credit(1));
        assert_eq!(session.output_lease_bytes, 0);
        assert_eq!(session.output_total(), 1024 * 1024);
        assert!(
            session.output.len() + usize::from(!session.output_tail.is_empty())
                <= (1024_usize * 1024).div_ceil(OUTPUT_CHUNK_TARGET)
        );
    }
}
