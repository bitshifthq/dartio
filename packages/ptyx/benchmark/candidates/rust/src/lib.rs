//! Direct Unix PTY candidate used only for architecture comparison.
//!
//! The intentionally small ABI is shared with the Zig candidate. It includes
//! the Dart FFI crossing, PTY allocation, spawn, byte transfer, direct-child
//! wait, generation-checked handles, and cleanup. It is not the production
//! API: calls are synchronous and backpressure is supplied by the kernel PTY.

#![cfg(unix)]

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

const ABI_VERSION: u32 = 1;
const OK: i32 = 0;
const INVALID_ARGUMENT: i32 = 1;
const OS_ERROR: i32 = 2;
const STALE_HANDLE: i32 = 3;

thread_local! {
    static LAST_OS_ERROR: Cell<i32> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
struct Session {
    master: RawFd,
    pid: libc::pid_t,
    waited: bool,
    exit_code: i32,
}

#[derive(Clone, Copy)]
struct Entry {
    generation: u32,
    session: Session,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<u32, Entry>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static NEXT_SLOT: AtomicU32 = AtomicU32::new(1);
static NEXT_GENERATION: AtomicU32 = AtomicU32::new(1);

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn handle(slot: u32, generation: u32) -> u64 {
    (u64::from(generation) << 32) | u64::from(slot)
}

fn handle_parts(handle: u64) -> (u32, u32) {
    (handle as u32, (handle >> 32) as u32)
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn remember_errno() {
    LAST_OS_ERROR.set(last_errno());
}

fn lookup(handle: u64) -> Result<Session, i32> {
    let (slot, generation) = handle_parts(handle);
    let registry = registry().lock().map_err(|_| OS_ERROR)?;
    let entry = registry.entries.get(&slot).ok_or(STALE_HANDLE)?;
    if entry.generation != generation {
        return Err(STALE_HANDLE);
    }
    Ok(entry.session)
}

fn update(handle: u64, session: Session) -> Result<(), i32> {
    let (slot, generation) = handle_parts(handle);
    let mut registry = registry().lock().map_err(|_| OS_ERROR)?;
    let entry = registry.entries.get_mut(&slot).ok_or(STALE_HANDLE)?;
    if entry.generation != generation {
        return Err(STALE_HANDLE);
    }
    entry.session = session;
    Ok(())
}

#[no_mangle]
pub extern "C" fn ptyx_candidate_abi() -> u32 {
    ABI_VERSION
}

#[no_mangle]
/// Spawns `/bin/sh -c <script>` attached to a new PTY.
///
/// # Safety
///
/// `script` must reference `script_length` readable bytes and `out_handle`
/// must reference writable storage for one `u64`.
pub unsafe extern "C" fn ptyx_candidate_spawn(
    script: *const u8,
    script_length: usize,
    out_handle: *mut u64,
) -> i32 {
    if script.is_null() || script_length == 0 || out_handle.is_null() {
        return INVALID_ARGUMENT;
    }

    let script = match CString::new(slice::from_raw_parts(script, script_length)) {
        Ok(script) => script,
        Err(_) => return INVALID_ARGUMENT,
    };

    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if libc::openpty(
        &mut master,
        &mut slave,
        ptr::null_mut(),
        ptr::null_mut(),
        &mut size,
    ) != 0
    {
        remember_errno();
        return OS_ERROR;
    }

    let pid = libc::fork();
    if pid < 0 {
        remember_errno();
        libc::close(master);
        libc::close(slave);
        return OS_ERROR;
    }
    if pid == 0 {
        libc::close(master);
        if libc::setsid() < 0
            || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0
            || libc::dup2(slave, libc::STDIN_FILENO) < 0
            || libc::dup2(slave, libc::STDOUT_FILENO) < 0
            || libc::dup2(slave, libc::STDERR_FILENO) < 0
        {
            libc::_exit(126);
        }
        if slave > libc::STDERR_FILENO {
            libc::close(slave);
        }
        let shell = c"/bin/sh";
        let dash_c = c"-c";
        libc::execl(
            shell.as_ptr(),
            shell.as_ptr(),
            dash_c.as_ptr(),
            script.as_ptr(),
            ptr::null::<libc::c_char>(),
        );
        libc::_exit(127);
    }

    libc::close(slave);
    let flags = libc::fcntl(master, libc::F_GETFD);
    if flags >= 0 {
        libc::fcntl(master, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }

    let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let candidate_handle = handle(slot, generation);
    let entry = Entry {
        generation,
        session: Session {
            master,
            pid,
            waited: false,
            exit_code: 0,
        },
    };
    let inserted = registry()
        .lock()
        .map(|mut registry| registry.entries.insert(slot, entry).is_none())
        .unwrap_or(false);
    if !inserted {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        libc::close(master);
        libc::waitpid(pid, ptr::null_mut(), 0);
        return OS_ERROR;
    }
    out_handle.write(candidate_handle);
    OK
}

#[no_mangle]
/// Reads up to `capacity` bytes from the candidate PTY.
///
/// # Safety
///
/// `bytes` must reference `capacity` writable bytes.
pub unsafe extern "C" fn ptyx_candidate_read(handle: u64, bytes: *mut u8, capacity: usize) -> i64 {
    if bytes.is_null() || capacity == 0 {
        return -i64::from(INVALID_ARGUMENT);
    }
    let session = match lookup(handle) {
        Ok(session) => session,
        Err(status) => return -i64::from(status),
    };
    loop {
        let result = libc::read(session.master, bytes.cast(), capacity);
        if result >= 0 {
            return result as i64;
        }
        let error = last_errno();
        if error == libc::EINTR {
            continue;
        }
        if error == libc::EIO {
            return 0;
        }
        LAST_OS_ERROR.set(error);
        return -i64::from(OS_ERROR);
    }
}

#[no_mangle]
/// Writes up to `length` bytes to the candidate PTY.
///
/// # Safety
///
/// `bytes` must reference `length` readable bytes.
pub unsafe extern "C" fn ptyx_candidate_write(handle: u64, bytes: *const u8, length: usize) -> i64 {
    if bytes.is_null() || length == 0 {
        return -i64::from(INVALID_ARGUMENT);
    }
    let session = match lookup(handle) {
        Ok(session) => session,
        Err(status) => return -i64::from(status),
    };
    loop {
        let result = libc::write(session.master, bytes.cast(), length);
        if result >= 0 {
            return result as i64;
        }
        let error = last_errno();
        if error == libc::EINTR {
            continue;
        }
        LAST_OS_ERROR.set(error);
        return -i64::from(OS_ERROR);
    }
}

#[no_mangle]
/// Waits for the direct child and stores its normalized exit code.
///
/// # Safety
///
/// `out_exit_code` must reference writable storage for one `i32`.
pub unsafe extern "C" fn ptyx_candidate_wait(handle: u64, out_exit_code: *mut i32) -> i32 {
    if out_exit_code.is_null() {
        return INVALID_ARGUMENT;
    }
    let mut session = match lookup(handle) {
        Ok(session) => session,
        Err(status) => return status,
    };
    if !session.waited {
        let mut status = 0;
        loop {
            let result = libc::waitpid(session.pid, &mut status, 0);
            if result == session.pid {
                session.waited = true;
                session.exit_code = if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else if libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status)
                } else {
                    status
                };
                if update(handle, session).is_err() {
                    return STALE_HANDLE;
                }
                break;
            }
            if result < 0 && last_errno() == libc::EINTR {
                continue;
            }
            remember_errno();
            return OS_ERROR;
        }
    }
    out_exit_code.write(session.exit_code);
    OK
}

#[no_mangle]
/// Removes and closes a generation-checked candidate session.
///
/// # Safety
///
/// The integer handle may be untrusted. The function validates it before
/// accessing session state.
pub unsafe extern "C" fn ptyx_candidate_close(handle: u64) -> i32 {
    let (slot, generation) = handle_parts(handle);
    let entry = {
        let mut registry = match registry().lock() {
            Ok(registry) => registry,
            Err(_) => return OS_ERROR,
        };
        let Some(entry) = registry.entries.get(&slot) else {
            return STALE_HANDLE;
        };
        if entry.generation != generation {
            return STALE_HANDLE;
        }
        registry
            .entries
            .remove(&slot)
            .expect("entry was just present")
    };

    libc::close(entry.session.master);
    if !entry.session.waited {
        libc::kill(-entry.session.pid, libc::SIGHUP);
        libc::kill(entry.session.pid, libc::SIGHUP);
        let mut status = 0;
        loop {
            let result = libc::waitpid(entry.session.pid, &mut status, 0);
            if result == entry.session.pid || (result < 0 && last_errno() == libc::ECHILD) {
                break;
            }
            if result < 0 && last_errno() == libc::EINTR {
                continue;
            }
            remember_errno();
            return OS_ERROR;
        }
    }
    OK
}

#[no_mangle]
pub extern "C" fn ptyx_candidate_last_os_error() -> i32 {
    LAST_OS_ERROR.get()
}
