//! Child process lifecycle helpers.
//!
//! This module centralizes exit observation and signaling so the session never
//! signals a stale PID after the child has been reaped.

use crate::abi::{
    PTYX_STATUS_CLOSED, PTYX_STATUS_ERROR, PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_WAIT_FAILED,
};
use crate::error::PtyxError;
use crate::session::SessionInner;

#[cfg(target_os = "macos")]
pub(crate) struct ExitWatcher {
    kqueue: libc::c_int,
}

#[cfg(target_os = "macos")]
impl ExitWatcher {
    pub(crate) fn new(pid: u32) -> Result<Self, PtyxError> {
        // The watcher lets macOS report fast child exits even when other code
        // may otherwise consume the child status first.
        let kqueue = unsafe { libc::kqueue() };
        if kqueue < 0 {
            return Err(PtyxError::io(
                PTYX_STATUS_NATIVE_ERROR,
                std::io::Error::last_os_error(),
            ));
        }

        let event = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // `event` is a fully initialized registration for this child PID.
        let result =
            unsafe { libc::kevent(kqueue, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            // Registration failed, so close the kqueue before returning.
            unsafe { libc::close(kqueue) };
            return Err(PtyxError::io(PTYX_STATUS_NATIVE_ERROR, error));
        }

        Ok(Self { kqueue })
    }

    fn wait(&self) -> Result<i32, PtyxError> {
        // `event` is initialized by `kevent` before it is read.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::kevent(
                self.kqueue,
                std::ptr::null(),
                0,
                &mut event,
                1,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(PtyxError::io(
                PTYX_STATUS_WAIT_FAILED,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(exit_code_from_wait_status(event.data as libc::c_int))
    }
}

#[cfg(target_os = "macos")]
impl Drop for ExitWatcher {
    fn drop(&mut self) {
        // The kqueue is owned by this watcher.
        unsafe { libc::close(self.kqueue) };
    }
}

pub(crate) fn kill(inner: &SessionInner, signal: i32) -> Result<bool, PtyxError> {
    // After wait has cached an exit code, the OS may reuse the numeric PID.
    // Treat the child as gone instead of signaling a stale identifier.
    if has_cached_exit(inner)? {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        signal_child(inner, signal)
    }

    #[cfg(not(unix))]
    {
        let _ = signal;
        let mut killer = inner
            .killer
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "child killer lock poisoned"))?;
        killer
            .kill()
            .map_err(|e| PtyxError::io(PTYX_STATUS_NATIVE_ERROR, e))?;
        Ok(true)
    }
}

fn has_cached_exit(inner: &SessionInner) -> Result<bool, PtyxError> {
    let cache = inner
        .wait_cache
        .lock()
        .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "wait cache lock poisoned"))?;
    Ok(cache.is_some())
}

#[cfg(unix)]
pub(crate) fn ensure_child_exit_status_is_waitable() {
    // The host runtime may install a SIGCHLD handler that reaps native
    // children before Rust observes their status. PTYX owns these child
    // processes, so restore default SIGCHLD handling before spawning or waiting
    // for them.
    unsafe {
        // `sigaction` is initialized before it is passed to libc.
        let mut next: libc::sigaction = std::mem::zeroed();
        next.sa_sigaction = libc::SIG_DFL;
        next.sa_flags = 0;
        libc::sigemptyset(&mut next.sa_mask);
        libc::sigaction(libc::SIGCHLD, &next, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn ensure_child_exit_status_is_waitable() {}

#[cfg(unix)]
fn signal_child(inner: &SessionInner, signal: i32) -> Result<bool, PtyxError> {
    let Some(pid) = inner.child_pid else {
        return Ok(false);
    };
    // The cached exit check in `kill` prevents signaling after the child has
    // been reaped.
    let result = unsafe { libc::kill(pid as i32, signal) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(PtyxError::io(PTYX_STATUS_NATIVE_ERROR, error));
    }
    Ok(true)
}

pub(crate) fn wait_exit(inner: &SessionInner) -> Result<i32, PtyxError> {
    ensure_child_exit_status_is_waitable();

    {
        let cache = inner
            .wait_cache
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "wait cache lock poisoned"))?;
        if let Some(exit_code) = *cache {
            return Ok(exit_code);
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(watcher) = inner.exit_watcher.as_ref() {
        let exit_code = watcher.wait()?;
        let _ = inner
            .child
            .lock()
            .map(|mut child| child.as_mut().and_then(|child| child.try_wait().ok()));
        return cache_exit(inner, exit_code);
    }

    let mut child_guard = inner
        .child
        .lock()
        .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "child lock poisoned"))?;
    let child = child_guard
        .as_mut()
        .ok_or_else(|| PtyxError::new(PTYX_STATUS_CLOSED, "child handle is closed"))?;

    match child.wait() {
        Ok(status) => cache_exit(inner, exit_code(status)),
        Err(e) if is_no_child_error(&e) => Err(PtyxError::new(
            PTYX_STATUS_WAIT_FAILED,
            "child exit status was reaped before ptyx could observe it",
        )),
        Err(e) => Err(PtyxError::io(PTYX_STATUS_WAIT_FAILED, e)),
    }
}

fn exit_code(exit: portable_pty::ExitStatus) -> i32 {
    exit.exit_code() as i32
}

#[cfg(target_os = "macos")]
fn exit_code_from_wait_status(status: libc::c_int) -> i32 {
    if status <= u8::MAX as libc::c_int {
        status
    } else if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        0
    }
}

fn cache_exit(inner: &SessionInner, exit_code: i32) -> Result<i32, PtyxError> {
    let mut cache = inner
        .wait_cache
        .lock()
        .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "wait cache lock poisoned"))?;
    *cache = Some(exit_code);
    Ok(exit_code)
}

fn is_no_child_error(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ECHILD)
}
