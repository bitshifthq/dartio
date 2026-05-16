//! PTY reader loop.
//!
//! The Unix path uses nonblocking fd reads so the reader can flush timed output
//! batches and stop promptly. Other platforms fall back to the reader object
//! provided by the PTY backend.

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::abi::{PTYX_STATUS_ERROR, PTYX_STATUS_IO_FAILED};
use crate::error::PtyxError;
#[cfg(unix)]
use crate::fd::{poll_readable, read_fd, set_fd_nonblocking, PollResult, ReadResult};
use crate::output::{OutputBuffer, OutputConfig};
use crate::session::{BusyGuard, SessionInner};

pub(crate) fn run(
    inner: Arc<SessionInner>,
    stop: Arc<AtomicBool>,
    inflight: Arc<(Mutex<u64>, Condvar)>,
    external_bytes: Arc<AtomicUsize>,
    config: OutputConfig,
) {
    let mut output = OutputBuffer::new(config, inflight, external_bytes);
    let _guard = match BusyGuard::enter(&inner.read_busy) {
        Ok(guard) => guard,
        Err(error) => {
            output.post_error(error);
            output.close_ports();
            return;
        }
    };

    #[cfg(unix)]
    {
        if let Some(fd) = match inner.master.lock() {
            Ok(master) => master.as_raw_fd(),
            Err(_) => {
                output.post_error(PtyxError::new(PTYX_STATUS_ERROR, "master lock poisoned"));
                output.close_ports();
                return;
            }
        } {
            read_unix_fd(fd, &stop, &mut output);
            return;
        }
    }

    read_blocking(Arc::clone(&inner), &stop, &mut output);
}

#[cfg(unix)]
fn read_unix_fd(fd: libc::c_int, stop: &AtomicBool, output: &mut OutputBuffer) {
    let mut buffer = vec![0_u8; output.read_buffer_size()];
    let _nonblocking = match set_fd_nonblocking(fd) {
        Ok(guard) => guard,
        Err(error) => {
            output.post_error(error);
            output.close_ports();
            return;
        }
    };

    'read_loop: loop {
        if stop.load(Ordering::SeqCst) {
            output.flush(stop).ok();
            break;
        }

        loop {
            match read_fd(fd, &mut buffer) {
                Ok(ReadResult::Data(n)) => {
                    if let Err(error) = output.append(&buffer[..n], stop) {
                        output.post_error(error);
                        break 'read_loop;
                    }
                }
                Ok(ReadResult::WouldBlock) => break,
                Ok(ReadResult::Eof) => {
                    output.flush(stop).ok();
                    break 'read_loop;
                }
                Err(error) => {
                    output.flush_and_post_error(stop, error);
                    break 'read_loop;
                }
            }

            if stop.load(Ordering::SeqCst) {
                continue 'read_loop;
            }
        }

        match poll_readable(fd, output.poll_timeout()) {
            Ok(PollResult::Timeout) => {
                if let Err(error) = output.flush(stop) {
                    output.post_error(error);
                    break;
                }
            }
            Ok(PollResult::Ready) => continue,
            Err(error) => {
                output.flush_and_post_error(stop, error);
                break;
            }
        }
    }

    output.close_ports();
}

fn read_blocking(inner: Arc<SessionInner>, stop: &AtomicBool, output: &mut OutputBuffer) {
    let mut buffer = vec![0_u8; output.read_buffer_size()];
    let mut reader = match inner.reader.lock() {
        Ok(reader) => reader,
        Err(_) => {
            output.post_error(PtyxError::new(PTYX_STATUS_ERROR, "reader lock poisoned"));
            output.close_ports();
            return;
        }
    };

    loop {
        if stop.load(Ordering::SeqCst) {
            output.flush(stop).ok();
            break;
        }

        match reader.read(&mut buffer) {
            Ok(0) => {
                output.flush(stop).ok();
                break;
            }
            Ok(n) => {
                if let Err(error) = output.append(&buffer[..n], stop) {
                    output.post_error(error);
                    break;
                }
                if output.should_flush_for_delay() {
                    if let Err(error) = output.flush(stop) {
                        output.post_error(error);
                        break;
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                output.flush(stop).ok();
                break;
            }
            Err(e) => {
                output.flush_and_post_error(stop, PtyxError::io(PTYX_STATUS_IO_FAILED, e));
                break;
            }
        }
    }

    output.close_ports();
}
