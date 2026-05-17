//! Child process lifecycle helpers.
//!
//! This module centralizes exit observation and signaling so the session never
//! signals a stale PID after the child has been reaped.

#[cfg(not(target_os = "linux"))]
use crate::config::EnvironmentMode;
use crate::config::SpawnConfig;
use crate::error::{PtyxError, PtyxErrorKind};
use crate::session::{Session, SessionInner};
#[cfg(not(target_os = "linux"))]
use portable_pty::CommandBuilder;
use portable_pty::{Child, SlavePty};
use std::ffi::CStr;
#[cfg(not(target_os = "linux"))]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Condvar, Mutex};
use std::thread;

pub(crate) struct WaitState {
    result: Mutex<Option<Result<i32, PtyxError>>>,
    ready: Condvar,
}

impl WaitState {
    pub(crate) fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn is_ready(&self) -> Result<bool, PtyxError> {
        let result = self
            .result
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "wait state lock poisoned"))?;
        Ok(result.is_some())
    }

    fn wait(&self) -> Result<i32, PtyxError> {
        let mut result = self
            .result
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "wait state lock poisoned"))?;
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            result = self
                .ready
                .wait(result)
                .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "wait state lock poisoned"))?;
        }
    }

    fn complete(&self, next: Result<i32, PtyxError>) {
        if let Ok(mut result) = self.result.lock() {
            if result.is_none() {
                *result = Some(next);
            }
            self.ready.notify_all();
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct ExitWatcher {
    kqueue: OwnedFd,
}

#[cfg(target_os = "macos")]
impl ExitWatcher {
    pub(crate) fn new(pid: u32) -> Result<Self, PtyxError> {
        // The watcher lets macOS report fast child exits even when other code
        // may otherwise consume the child status first.
        let kqueue = unsafe { libc::kqueue() };
        if kqueue < 0 {
            return Err(PtyxError::io(
                PtyxErrorKind::NativeError,
                std::io::Error::last_os_error(),
            ));
        }
        let kqueue = unsafe { OwnedFd::from_raw_fd(kqueue) };

        let event = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // `event` is a fully initialized registration for this child PID.
        let result = unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            return Err(PtyxError::io(PtyxErrorKind::NativeError, error));
        }

        Ok(Self { kqueue })
    }

    fn wait(&self) -> Result<i32, PtyxError> {
        // `event` is initialized by `kevent` before it is read.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::kevent(
                self.kqueue.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(PtyxError::io(
                PtyxErrorKind::WaitFailed,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(exit_code_from_wait_status(event.data as libc::c_int))
    }
}

pub(crate) fn spawn_child(
    config: &SpawnConfig,
    slave: &(dyn SlavePty + Send),
    tty_name: Option<&CStr>,
) -> Result<Box<dyn Child + Send + Sync>, PtyxError> {
    #[cfg(target_os = "linux")]
    {
        let _ = slave;
        let tty_name = tty_name
            .ok_or_else(|| PtyxError::new(PtyxErrorKind::Unsupported, "TTY name unavailable"))?;
        crate::process_linux::spawn(config, tty_name)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = tty_name;
        let command = command(config)?;
        slave
            .spawn_command(command)
            .map_err(|e| PtyxError::io(PtyxErrorKind::SpawnFailed, e))
    }
}

#[cfg(not(target_os = "linux"))]
fn command(config: &SpawnConfig) -> Result<CommandBuilder, PtyxError> {
    let mut command = CommandBuilder::new(&config.executable);
    command.args(config.argv.iter().cloned());
    if !config.cwd.is_empty() {
        command.cwd(&config.cwd);
    }
    apply_env(&mut command, config.env_mode, &config.env_items)?;
    Ok(command)
}

#[cfg(not(target_os = "linux"))]
fn apply_env(
    command: &mut CommandBuilder,
    mode: EnvironmentMode,
    items: &[OsString],
) -> Result<(), PtyxError> {
    match mode {
        EnvironmentMode::Inherit => return Ok(()),
        EnvironmentMode::Overlay => {}
        EnvironmentMode::Replace | EnvironmentMode::Clear => command.env_clear(),
    }
    if mode == EnvironmentMode::Clear {
        return Ok(());
    }
    for item in items {
        let text = item.to_string_lossy();
        let Some((key, value)) = text.split_once('=') else {
            return Err(PtyxError::new(
                PtyxErrorKind::InvalidArgument,
                "environment entry must be KEY=VALUE",
            ));
        };
        if key.is_empty() {
            return Err(PtyxError::new(
                PtyxErrorKind::InvalidArgument,
                "environment key must not be empty",
            ));
        }
        if text.contains('\0') {
            return Err(PtyxError::new(
                PtyxErrorKind::InvalidArgument,
                "environment key and value must not contain NUL",
            ));
        }
        command.env(key, value);
    }
    Ok(())
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
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "child killer lock poisoned"))?;
        killer
            .kill()
            .map_err(|e| PtyxError::io(PtyxErrorKind::NativeError, e))?;
        Ok(true)
    }
}

#[cfg(unix)]
pub(crate) fn force_kill_signal() -> i32 {
    libc::SIGKILL
}

#[cfg(all(unix, test))]
pub(crate) fn terminate_signal_for_test() -> i32 {
    libc::SIGTERM
}

#[cfg(not(unix))]
pub(crate) fn force_kill_signal() -> i32 {
    0
}

fn has_cached_exit(inner: &SessionInner) -> Result<bool, PtyxError> {
    inner.wait_state.is_ready()
}

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
        return Err(PtyxError::io(PtyxErrorKind::NativeError, error));
    }
    Ok(true)
}

pub(crate) fn wait_exit(inner: &SessionInner) -> Result<i32, PtyxError> {
    inner.wait_state.wait()
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

pub(crate) fn start_exit_waiter(session: &Session) -> Result<(), PtyxError> {
    let inner = std::sync::Arc::clone(&session.inner);
    let handle = thread::Builder::new()
        .name("ptyx-child-wait".to_string())
        .spawn(move || {
            let result = wait_child(inner.as_ref());
            inner.wait_state.complete(result);
        })
        .map_err(|error| PtyxError::io(PtyxErrorKind::NativeError, error))?;

    let mut slot = session
        .child_waiter
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "child waiter lock poisoned"))?;
    *slot = Some(handle);
    Ok(())
}

pub(crate) fn join_exit_waiter(session: &Session) -> Result<(), PtyxError> {
    let handle = session
        .child_waiter
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "child waiter lock poisoned"))?
        .take();
    if let Some(handle) = handle {
        handle
            .join()
            .map_err(|_| PtyxError::new(PtyxErrorKind::NativeError, "child waiter panicked"))?;
    }
    Ok(())
}

fn wait_child(inner: &SessionInner) -> Result<i32, PtyxError> {
    let mut child = take_child(inner)?;

    #[cfg(target_os = "macos")]
    if let Some(watcher) = inner.exit_watcher.as_ref() {
        let exit_code = watcher.wait()?;
        let _ = child.try_wait();
        return Ok(exit_code);
    }

    match child.wait() {
        Ok(status) => Ok(exit_code(status)),
        Err(e) if is_no_child_error(&e) => Err(PtyxError::new(
            PtyxErrorKind::WaitFailed,
            "child exit status was reaped before ptyx could observe it",
        )),
        Err(e) => Err(PtyxError::io(PtyxErrorKind::WaitFailed, e)),
    }
}

fn take_child(inner: &SessionInner) -> Result<Box<dyn Child + Send + Sync>, PtyxError> {
    inner
        .child
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "child lock poisoned"))?
        .take()
        .ok_or_else(|| PtyxError::new(PtyxErrorKind::Closed, "child handle is closed"))
}

fn is_no_child_error(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ECHILD)
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn environment_entries_reject_missing_separator() {
        let items = [OsString::from("INVALID")];

        let error = command_with_env(&items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_empty_key() {
        let items = [OsString::from("=value")];

        let error = command_with_env(&items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_nul_value() {
        let items = [OsString::from("INVALID=bad\0value")];

        let error = command_with_env(&items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    fn command_with_env(items: &[OsString]) -> Result<CommandBuilder, PtyxError> {
        let mut command = CommandBuilder::new("/usr/bin/env");
        apply_env(&mut command, EnvironmentMode::Overlay, items)?;
        Ok(command)
    }
}
