//! Unix file descriptor helpers.
//!
//! These helpers keep PTY reads and writes interruptible without exposing raw
//! descriptor handling to the higher-level runtime code.

#[cfg(unix)]
use std::io::ErrorKind;
use std::thread;
use std::time::Duration;

use crate::error::PtyxError;
#[cfg(unix)]
use crate::error::PtyxErrorKind;
use crate::session::SessionInner;

pub(crate) struct WriteRetryContext {
    #[cfg(unix)]
    fd: Option<libc::c_int>,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) struct ReadFdContext {
    fd: libc::c_int,
}

pub(crate) fn write_retry_context_for_inner(
    inner: &SessionInner,
) -> Result<WriteRetryContext, PtyxError> {
    #[cfg(unix)]
    {
        let master = inner
            .master
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "master lock poisoned"))?;
        Ok(WriteRetryContext {
            fd: master.as_raw_fd(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = inner;
        Ok(WriteRetryContext {})
    }
}

#[cfg(unix)]
pub(crate) fn read_context_for_inner(
    inner: &SessionInner,
) -> Result<Option<ReadFdContext>, PtyxError> {
    let master = inner
        .master
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "master lock poisoned"))?;
    Ok(master.as_raw_fd().map(|fd| ReadFdContext { fd }))
}

impl WriteRetryContext {
    #[cfg(unix)]
    pub(crate) fn uses_direct_io(&self) -> bool {
        self.fd.is_some()
    }

    #[cfg(unix)]
    pub(crate) fn write_direct(&self, data: &[u8]) -> std::io::Result<DirectWriteResult> {
        match self.fd {
            Some(fd) => write_fd(fd, data),
            None => Err(std::io::Error::new(
                ErrorKind::Unsupported,
                "raw fd writes are unavailable",
            )),
        }
    }

    pub(crate) fn wait_writable_for(&self, timeout: Duration) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            if let Some(fd) = self.fd {
                return wait_fd_writable_for(fd, timeout);
            }
        }
        thread::sleep(timeout);
        Ok(true)
    }
}

#[cfg(unix)]
impl ReadFdContext {
    pub(crate) fn set_nonblocking(&self) -> Result<FdNonblockingGuard, PtyxError> {
        set_fd_nonblocking(self.fd)
    }

    pub(crate) fn read(&self, buffer: &mut [u8]) -> Result<ReadResult, PtyxError> {
        read_fd(self.fd, buffer)
    }

    pub(crate) fn poll_readable(&self, timeout: Option<Duration>) -> Result<PollResult, PtyxError> {
        poll_readable(self.fd, timeout)
    }
}

#[cfg(unix)]
pub(crate) enum DirectWriteResult {
    Wrote(usize),
    Interrupted,
    WouldBlock,
}

#[cfg(unix)]
fn wait_fd_writable_for(fd: libc::c_int, timeout: Duration) -> std::io::Result<bool> {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLOUT | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        // `poll_fd` points to one initialized descriptor entry.
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            if poll_fd.revents & libc::POLLNVAL != 0 {
                return Err(std::io::Error::new(
                    ErrorKind::BrokenPipe,
                    "PTY file descriptor is closed",
                ));
            }
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

#[cfg(unix)]
pub(crate) enum PollResult {
    Ready,
    Timeout,
}

#[cfg(unix)]
pub(crate) enum ReadResult {
    Data(usize),
    Eof,
    WouldBlock,
}

#[cfg(unix)]
pub(crate) struct FdNonblockingGuard {
    fd: libc::c_int,
    original_flags: libc::c_int,
}

#[cfg(unix)]
impl Drop for FdNonblockingGuard {
    fn drop(&mut self) {
        // Best effort restoration. There is no useful recovery path during
        // cleanup if the fd has already closed.
        unsafe {
            libc::fcntl(self.fd, libc::F_SETFL, self.original_flags);
        }
    }
}

#[cfg(unix)]
fn set_fd_nonblocking(fd: libc::c_int) -> Result<FdNonblockingGuard, PtyxError> {
    // `fd` is owned by the session and valid while the guard is alive.
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return Err(PtyxError::io(
            PtyxErrorKind::IoFailed,
            std::io::Error::last_os_error(),
        ));
    }
    if original_flags & libc::O_NONBLOCK == 0 {
        // `fd` is still owned by the session; only the status flags change.
        let result = unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) };
        if result < 0 {
            return Err(PtyxError::io(
                PtyxErrorKind::IoFailed,
                std::io::Error::last_os_error(),
            ));
        }
    }
    Ok(FdNonblockingGuard { fd, original_flags })
}

#[cfg(unix)]
fn poll_readable(fd: libc::c_int, timeout: Option<Duration>) -> Result<PollResult, PtyxError> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let timeout_ms = timeout.map(duration_to_poll_timeout_ms).unwrap_or(-1);
    loop {
        // `poll_fd` points to one initialized descriptor entry.
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            if poll_fd.revents & libc::POLLNVAL != 0 {
                return Err(PtyxError::new(
                    PtyxErrorKind::Closed,
                    "PTY file descriptor is closed",
                ));
            }
            return Ok(PollResult::Ready);
        }
        if result == 0 {
            return Ok(PollResult::Timeout);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            continue;
        }
        return Err(PtyxError::io(PtyxErrorKind::IoFailed, error));
    }
}

#[cfg(unix)]
fn duration_to_poll_timeout_ms(duration: Duration) -> libc::c_int {
    if duration.is_zero() {
        return 0;
    }
    duration.as_millis().clamp(1, libc::c_int::MAX as u128) as libc::c_int
}

#[cfg(unix)]
fn read_fd(fd: libc::c_int, buffer: &mut [u8]) -> Result<ReadResult, PtyxError> {
    loop {
        // `buffer` is valid for writes of its full length for this call.
        let result = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if result > 0 {
            return Ok(ReadResult::Data(result as usize));
        }
        if result == 0 {
            return Ok(ReadResult::Eof);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == ErrorKind::WouldBlock {
            return Ok(ReadResult::WouldBlock);
        }
        if error.raw_os_error() == Some(libc::EIO) {
            return Ok(ReadResult::Eof);
        }
        return Err(PtyxError::io(PtyxErrorKind::IoFailed, error));
    }
}

#[cfg(unix)]
fn write_fd(fd: libc::c_int, data: &[u8]) -> std::io::Result<DirectWriteResult> {
    let result = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    if result > 0 {
        return Ok(DirectWriteResult::Wrote(result as usize));
    }
    if result == 0 {
        return Err(std::io::Error::new(
            ErrorKind::WriteZero,
            "failed to write to PTY",
        ));
    }

    let error = std::io::Error::last_os_error();
    match error.kind() {
        ErrorKind::Interrupted => Ok(DirectWriteResult::Interrupted),
        ErrorKind::WouldBlock => Ok(DirectWriteResult::WouldBlock),
        _ => Err(error),
    }
}
