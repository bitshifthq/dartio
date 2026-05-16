//! Session ownership and process setup.
//!
//! A session owns the PTY master, child handle, reader/writer halves, and the
//! optional runtime threads that move data between the PTY and native ports.

#[cfg(not(unix))]
use portable_pty::ChildKiller;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::{CStr, CString, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::{EnvironmentMode, SpawnConfig};
use crate::error::{PtyxError, PtyxErrorKind};
use crate::runtime::SessionRuntime;

pub(crate) struct Session {
    pub(crate) inner: Arc<SessionInner>,
    pub(crate) runtime: Mutex<Option<SessionRuntime>>,
}

/// Shared process and PTY handles used by runtime threads.
///
/// The child exit code is cached once observed. That cache is also used by
/// signal delivery so cleanup never targets a PID after the child is reaped.
pub(crate) struct SessionInner {
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) reader: Mutex<Box<dyn std::io::Read + Send>>,
    pub(crate) writer: Mutex<Box<dyn std::io::Write + Send>>,
    pub(crate) child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    #[cfg(not(unix))]
    pub(crate) killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    #[cfg(target_os = "macos")]
    pub(crate) exit_watcher: Option<crate::process::ExitWatcher>,
    pub(crate) wait_cache: Mutex<Option<i32>>,
    pub(crate) read_busy: AtomicBool,
    pub(crate) write_busy: AtomicBool,
    pub(crate) closed: AtomicBool,
    pub(crate) child_pid: Option<u32>,
    pub(crate) tty_name: Option<CString>,
}

pub(crate) struct BusyGuard<'a> {
    busy: &'a AtomicBool,
}

impl<'a> BusyGuard<'a> {
    /// Marks an operation as active until the returned guard is dropped.
    pub(crate) fn enter(busy: &'a AtomicBool) -> Result<Self, PtyxError> {
        if busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(PtyxError::new(
                PtyxErrorKind::Busy,
                "operation is already active",
            ));
        }
        Ok(Self { busy })
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::SeqCst);
    }
}

impl SpawnConfig {
    fn command(&self) -> Result<CommandBuilder, PtyxError> {
        let mut command = CommandBuilder::new(&self.executable);
        command.args(self.argv.iter().cloned());
        if !self.cwd.is_empty() {
            command.cwd(&self.cwd);
        }
        apply_env(&mut command, self.env_mode, &self.env_items)?;
        Ok(command)
    }
}

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

pub(crate) fn close(session: &Session) {
    session.inner.closed.store(true, Ordering::SeqCst);
    crate::process::kill(session.inner.as_ref(), crate::process::force_kill_signal()).ok();
    crate::runtime::close_runtime(session).ok();
}

pub(crate) fn resize(session: &Session, size: PtySize) -> Result<(), PtyxError> {
    if session.inner.closed.load(Ordering::SeqCst) {
        return Err(PtyxError::new(PtyxErrorKind::Closed, "session is closed"));
    }
    let master = session
        .inner
        .master
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "master lock poisoned"))?;
    master
        .resize(size)
        .map_err(|e| PtyxError::io(PtyxErrorKind::NativeError, e))?;
    Ok(())
}

pub(crate) fn get_size(session: &Session) -> Result<PtySize, PtyxError> {
    let master = session
        .inner
        .master
        .lock()
        .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "master lock poisoned"))?;
    let size = master
        .get_size()
        .map_err(|e| PtyxError::io(PtyxErrorKind::NativeError, e))?;
    Ok(size)
}

pub(crate) fn get_child_pid(session: &Session) -> Result<u64, PtyxError> {
    session
        .inner
        .child_pid
        .map(u64::from)
        .ok_or_else(|| PtyxError::new(PtyxErrorKind::Unsupported, "child pid unavailable"))
}

pub(crate) fn get_tty_name(session: &Session) -> Result<&CStr, PtyxError> {
    session
        .inner
        .tty_name
        .as_deref()
        .ok_or_else(|| PtyxError::new(PtyxErrorKind::Unsupported, "TTY name unavailable"))
}

pub(crate) fn kill(session: &Session, signal: i32) -> bool {
    crate::process::kill(session.inner.as_ref(), signal).unwrap_or(false)
}

pub(crate) fn spawn(config: SpawnConfig) -> Result<Box<Session>, PtyxError> {
    crate::process::ensure_child_exit_status_is_waitable();
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(config.size)
        .map_err(|e| PtyxError::io(PtyxErrorKind::SpawnFailed, e))?;
    let cmd = config.command()?;

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| PtyxError::io(PtyxErrorKind::SpawnFailed, e))?;
    let child_pid = child.process_id();
    #[cfg(target_os = "macos")]
    let exit_watcher = child_pid.and_then(|pid| crate::process::ExitWatcher::new(pid).ok());
    #[cfg(not(unix))]
    let killer = child.clone_killer();
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| PtyxError::io(PtyxErrorKind::IoFailed, e))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| PtyxError::io(PtyxErrorKind::IoFailed, e))?;

    #[cfg(unix)]
    let tty_name = pair
        .master
        .tty_name()
        .and_then(|p| CString::new(p.to_string_lossy().as_bytes()).ok());
    #[cfg(not(unix))]
    let tty_name = None;

    let inner = SessionInner {
        master: Mutex::new(pair.master),
        reader: Mutex::new(reader),
        writer: Mutex::new(writer),
        child: Mutex::new(Some(child)),
        #[cfg(not(unix))]
        killer: Mutex::new(killer),
        #[cfg(target_os = "macos")]
        exit_watcher,
        wait_cache: Mutex::new(None),
        read_busy: AtomicBool::new(false),
        write_busy: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        child_pid,
        tty_name,
    };

    Ok(Box::new(Session {
        inner: Arc::new(inner),
        runtime: Mutex::new(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn environment_entries_reject_missing_separator() {
        let mut command = CommandBuilder::new("/usr/bin/env");
        let items = [OsString::from("INVALID")];

        let error = apply_env(&mut command, EnvironmentMode::Overlay, &items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_empty_key() {
        let mut command = CommandBuilder::new("/usr/bin/env");
        let items = [OsString::from("=value")];

        let error = apply_env(&mut command, EnvironmentMode::Overlay, &items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_nul_value() {
        let mut command = CommandBuilder::new("/usr/bin/env");
        let items = [OsString::from("INVALID=bad\0value")];

        let error = apply_env(&mut command, EnvironmentMode::Overlay, &items).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[cfg(unix)]
    #[test]
    fn failed_runtime_start_cleanup_releases_session() {
        let session = shell_session("sleep 10");

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn core_spawn_read_and_exit_code() {
        let session = shell_session("printf rust-ptyx");

        let mut buffer = [0_u8; 32];
        let mut reader = session.inner.reader.lock().unwrap();
        let read = reader.read(&mut buffer).unwrap();
        drop(reader);
        assert!(std::str::from_utf8(&buffer[..read])
            .unwrap()
            .contains("rust-ptyx"));

        assert_eq!(wait_for_exit(&session), 0);

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn kill_sends_requested_signal_to_child() {
        let script = "trap 'printf term; exit 42' TERM; while :; do sleep 1; done";
        let session = shell_session(script);

        thread::sleep(Duration::from_millis(100));
        assert!(kill(&session, crate::process::terminate_signal_for_test()));

        let mut buffer = [0_u8; 32];
        let mut reader = session.inner.reader.lock().unwrap();
        let read = reader.read(&mut buffer).unwrap();
        drop(reader);
        assert!(std::str::from_utf8(&buffer[..read])
            .unwrap()
            .contains("term"));

        assert_eq!(wait_for_exit(&session), 42);

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn fast_exit_code_is_preserved() {
        let session = shell_session("exit 7");

        assert_eq!(wait_for_exit(&session), 7);

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn kill_returns_false_after_exit_is_cached() {
        let session = shell_session("exit 7");

        assert_eq!(wait_for_exit(&session), 7);
        assert!(!kill(&session, crate::process::force_kill_signal()));

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_is_preserved_after_reader_observes_eof() {
        let session = shell_session("exit 7");
        let mut reader = session.inner.reader.lock().unwrap();
        let mut buffer = [0_u8; 32];
        let _ = reader.read(&mut buffer);
        drop(reader);

        assert_eq!(wait_for_exit(&session), 7);

        close(&session);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_is_preserved_with_concurrent_reader() {
        let session = shell_session("exit 7");
        let inner = Arc::clone(&session.inner);
        let waiter = thread::spawn(move || crate::process::wait_exit(inner.as_ref()).unwrap());

        let mut reader = session.inner.reader.lock().unwrap();
        let mut buffer = [0_u8; 32];
        let _ = reader.read(&mut buffer);
        drop(reader);

        assert_eq!(waiter.join().unwrap(), 7);

        close(&session);
    }

    fn wait_for_exit(session: &Session) -> i32 {
        crate::process::wait_exit(session.inner.as_ref()).unwrap()
    }

    #[cfg(unix)]
    fn shell_session(script: &str) -> Box<Session> {
        spawn(SpawnConfig {
            executable: OsString::from("/bin/sh"),
            argv: vec![OsString::from("-c"), OsString::from(script)],
            env_items: Vec::new(),
            env_mode: EnvironmentMode::Overlay,
            cwd: OsString::new(),
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        })
        .unwrap()
    }
}
