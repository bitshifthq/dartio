#![allow(
    non_camel_case_types,
    reason = "Opaque C ABI handles mirror ptyx headers."
)]

//! Session ownership and process setup.
//!
//! A session owns the PTY master, child handle, reader/writer halves, and the
//! optional runtime threads that move data between the PTY and native ports.

#[cfg(not(unix))]
use portable_pty::ChildKiller;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::{CString, OsString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::abi::{
    fill_string, ptyx_session_options_t, ptyx_size_t, string_array, string_to_os_string,
    to_pty_size, PTYX_ENV_CLEAR, PTYX_ENV_INHERIT, PTYX_ENV_OVERLAY, PTYX_ENV_REPLACE,
    PTYX_STATUS_CLOSED, PTYX_STATUS_ERROR, PTYX_STATUS_INVALID_ARGUMENT, PTYX_STATUS_IO_FAILED,
    PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_OK, PTYX_STATUS_SPAWN_FAILED, PTYX_STATUS_UNSUPPORTED,
};
use crate::error::{ffi_status, PtyxError};
use crate::runtime::SessionRuntime;

pub struct ptyx_session {
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
                crate::abi::PTYX_STATUS_BUSY,
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

struct SpawnConfig {
    executable: OsString,
    argv: Vec<OsString>,
    env_items: Vec<OsString>,
    env_mode: u32,
    cwd: OsString,
    size: PtySize,
}

impl SpawnConfig {
    fn from_options(options: ptyx_session_options_t) -> Result<Self, PtyxError> {
        let executable = string_to_os_string(options.executable)?;
        if executable.is_empty() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "executable must not be empty",
            ));
        }

        Ok(Self {
            executable,
            argv: string_array(options.argv, options.argc)?,
            env_items: string_array(options.env_items, options.env_count)?,
            env_mode: options.env_mode,
            cwd: string_to_os_string(options.cwd)?,
            size: to_pty_size(options.initial_size)?,
        })
    }

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

fn apply_env(command: &mut CommandBuilder, mode: u32, items: &[OsString]) -> Result<(), PtyxError> {
    match mode {
        PTYX_ENV_INHERIT => return Ok(()),
        PTYX_ENV_OVERLAY => {}
        PTYX_ENV_REPLACE | PTYX_ENV_CLEAR => command.env_clear(),
        _ => {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "unknown environment mode",
            ))
        }
    }
    if mode == PTYX_ENV_CLEAR {
        return Ok(());
    }
    for item in items {
        let text = item.to_string_lossy();
        let Some((key, value)) = text.split_once('=') else {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "environment entry must be KEY=VALUE",
            ));
        };
        if key.is_empty() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "environment key must not be empty",
            ));
        }
        if text.contains('\0') {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "environment key and value must not contain NUL",
            ));
        }
        command.env(key, value);
    }
    Ok(())
}

pub(crate) fn free(session: *mut ptyx_session) {
    if session.is_null() {
        return;
    }
    unsafe {
        // `session` was returned by `Box::into_raw` in `runtime::spawn`.
        // Reconstructing the box transfers ownership back to Rust exactly once.
        let boxed = Box::from_raw(session);
        boxed.inner.closed.store(true, Ordering::SeqCst);
        crate::process::kill(boxed.inner.as_ref(), force_kill_signal()).ok();
        crate::runtime::close_runtime(&boxed).ok();
    }
}

pub(crate) fn resize(session: *mut ptyx_session, size: ptyx_size_t) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if session.inner.closed.load(Ordering::SeqCst) {
            return Err(PtyxError::new(PTYX_STATUS_CLOSED, "session is closed"));
        }
        let size = to_pty_size(size)?;
        let master = session
            .inner
            .master
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "master lock poisoned"))?;
        master
            .resize(size)
            .map_err(|e| PtyxError::io(PTYX_STATUS_NATIVE_ERROR, e))?;
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn get_size(session: *mut ptyx_session, out_size: *mut ptyx_size_t) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if out_size.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "out_size must not be null",
            ));
        }
        let master = session
            .inner
            .master
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "master lock poisoned"))?;
        let size = master
            .get_size()
            .map_err(|e| PtyxError::io(PTYX_STATUS_NATIVE_ERROR, e))?;
        unsafe {
            // `out_size` is non-null and points to caller-owned writable memory.
            *out_size = ptyx_size_t {
                rows: size.rows as u32,
                columns: size.cols as u32,
                pixel_width: size.pixel_width as u32,
                pixel_height: size.pixel_height as u32,
            };
        }
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn get_child_pid(session: *mut ptyx_session, out_pid: *mut u64) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if out_pid.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "out_pid must not be null",
            ));
        }
        let pid = session
            .inner
            .child_pid
            .map(u64::from)
            .ok_or_else(|| PtyxError::new(PTYX_STATUS_UNSUPPORTED, "child pid unavailable"))?;
        // `out_pid` is non-null and points to caller-owned writable memory.
        unsafe { *out_pid = pid };
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn get_tty_name(
    session: *mut ptyx_session,
    buffer: *mut c_char,
    inout_len: *mut usize,
) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if inout_len.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "inout_len must not be null",
            ));
        }
        let tty_name = session
            .inner
            .tty_name
            .as_deref()
            .ok_or_else(|| PtyxError::new(PTYX_STATUS_UNSUPPORTED, "TTY name unavailable"))?;
        fill_string(tty_name, buffer, inout_len)
    })
}

pub(crate) fn kill(session: *mut ptyx_session, signal: i32) -> bool {
    let Ok(session) = session_ref(session) else {
        return false;
    };
    crate::process::kill(session.inner.as_ref(), signal).unwrap_or(false)
}

pub(crate) fn spawn(options: ptyx_session_options_t) -> Result<Box<ptyx_session>, PtyxError> {
    let config = SpawnConfig::from_options(options)?;

    crate::process::ensure_child_exit_status_is_waitable();
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(config.size)
        .map_err(|e| PtyxError::io(PTYX_STATUS_SPAWN_FAILED, e))?;
    let cmd = config.command()?;

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| PtyxError::io(PTYX_STATUS_SPAWN_FAILED, e))?;
    let child_pid = child.process_id();
    #[cfg(target_os = "macos")]
    let exit_watcher = child_pid.and_then(|pid| crate::process::ExitWatcher::new(pid).ok());
    #[cfg(not(unix))]
    let killer = child.clone_killer();
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| PtyxError::io(PTYX_STATUS_IO_FAILED, e))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| PtyxError::io(PTYX_STATUS_IO_FAILED, e))?;

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

    Ok(Box::new(ptyx_session {
        inner: Arc::new(inner),
        runtime: Mutex::new(None),
    }))
}

pub(crate) fn free_failed_session(session: *mut ptyx_session) {
    if session.is_null() {
        return;
    }
    unsafe {
        // Used only before the raw pointer has been returned to the caller.
        let boxed = Box::from_raw(session);
        boxed.inner.closed.store(true, Ordering::SeqCst);
        crate::process::kill(boxed.inner.as_ref(), force_kill_signal()).ok();
        crate::runtime::close_runtime(&boxed).ok();
    }
}

#[cfg(unix)]
fn force_kill_signal() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn force_kill_signal() -> i32 {
    0
}

pub(crate) fn session_ref(ptr: *mut ptyx_session) -> Result<&'static ptyx_session, PtyxError> {
    if ptr.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "session must not be null",
        ));
    }
    // The C API owns session lifetime. Callers must not use the pointer after
    // `ptyx_close`.
    Ok(unsafe { &*ptr })
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
        let items = [OsString::from("PTYX_INVALID")];

        let error = apply_env(&mut command, PTYX_ENV_OVERLAY, &items).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn environment_entries_reject_empty_key() {
        let mut command = CommandBuilder::new("/usr/bin/env");
        let items = [OsString::from("=value")];

        let error = apply_env(&mut command, PTYX_ENV_OVERLAY, &items).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_INVALID_ARGUMENT);
    }

    #[test]
    fn environment_entries_reject_nul_value() {
        let mut command = CommandBuilder::new("/usr/bin/env");
        let items = [OsString::from("PTYX_INVALID=bad\0value")];

        let error = apply_env(&mut command, PTYX_ENV_OVERLAY, &items).unwrap_err();

        assert_eq!(error.status, PTYX_STATUS_INVALID_ARGUMENT);
    }

    #[cfg(unix)]
    #[test]
    fn failed_runtime_start_cleanup_releases_session() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("sleep 10").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "sleep 10".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());
        free_failed_session(session);
    }

    #[cfg(unix)]
    #[test]
    fn core_spawn_read_and_exit_code() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("printf rust-ptyx").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "printf rust-ptyx".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());

        let mut buffer = [0_u8; 32];
        let session_ref = session_ref(session).unwrap();
        let mut reader = session_ref.inner.reader.lock().unwrap();
        let read = reader.read(&mut buffer).unwrap();
        drop(reader);
        assert!(std::str::from_utf8(&buffer[..read])
            .unwrap()
            .contains("rust-ptyx"));

        assert_eq!(wait_for_exit(session), 0);

        free(session);
    }

    #[cfg(unix)]
    #[test]
    fn kill_sends_requested_signal_to_child() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let script = "trap 'printf term; exit 42' TERM; while :; do sleep 1; done";
        let arg1 = CString::new(script).unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: script.len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());

        thread::sleep(Duration::from_millis(100));
        assert!(kill(session, libc::SIGTERM));

        let mut buffer = [0_u8; 32];
        let session_ref = session_ref(session).unwrap();
        let mut reader = session_ref.inner.reader.lock().unwrap();
        let read = reader.read(&mut buffer).unwrap();
        drop(reader);
        assert!(std::str::from_utf8(&buffer[..read])
            .unwrap()
            .contains("term"));

        assert_eq!(wait_for_exit(session), 42);

        free(session);
    }

    #[cfg(unix)]
    #[test]
    fn fast_exit_code_is_preserved() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("exit 7").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "exit 7".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());

        assert_eq!(wait_for_exit(session), 7);

        free(session);
    }

    #[cfg(unix)]
    #[test]
    fn kill_returns_false_after_exit_is_cached() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("exit 7").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "exit 7".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());

        assert_eq!(wait_for_exit(session), 7);
        assert!(!kill(session, libc::SIGKILL));

        free(session);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_is_preserved_after_reader_observes_eof() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("exit 7").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "exit 7".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());
        let session_ref = session_ref(session).unwrap();
        let mut reader = session_ref.inner.reader.lock().unwrap();
        let mut buffer = [0_u8; 32];
        let _ = reader.read(&mut buffer);
        drop(reader);

        assert_eq!(wait_for_exit(session), 7);

        free(session);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_is_preserved_with_concurrent_reader() {
        let exe = CString::new("/bin/sh").unwrap();
        let arg0 = CString::new("-c").unwrap();
        let arg1 = CString::new("exit 7").unwrap();
        let argv = [
            crate::abi::ptyx_string_t {
                data: arg0.as_ptr(),
                len: 2,
            },
            crate::abi::ptyx_string_t {
                data: arg1.as_ptr(),
                len: "exit 7".len(),
            },
        ];
        let mut options = crate::abi::default_session_options();
        options.executable = crate::abi::ptyx_string_t {
            data: exe.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.argv = argv.as_ptr();
        options.argc = argv.len();

        let session = Box::into_raw(spawn(options).unwrap());
        let inner = Arc::clone(&session_ref(session).unwrap().inner);
        let waiter = thread::spawn(move || crate::process::wait_exit(inner.as_ref()).unwrap());

        let session_ref = session_ref(session).unwrap();
        let mut reader = session_ref.inner.reader.lock().unwrap();
        let mut buffer = [0_u8; 32];
        let _ = reader.read(&mut buffer);
        drop(reader);

        assert_eq!(waiter.join().unwrap(), 7);

        free(session);
    }

    fn wait_for_exit(session: *mut ptyx_session) -> i32 {
        crate::process::wait_exit(session_ref(session).unwrap().inner.as_ref()).unwrap()
    }
}
