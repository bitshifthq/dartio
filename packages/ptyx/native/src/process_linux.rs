//! Linux child supervision.
//!
//! An embedding host can reap direct children before this library can wait for
//! them. The monitor process becomes the real PTY program's parent, owns its
//! wait status, and reports the exit code over a pipe.

use crate::config::{EnvironmentMode, SpawnConfig};
use crate::error::{PtyxError, PtyxErrorKind};
use portable_pty::{Child, ChildKiller, ExitStatus};
use std::ffi::{CStr, CString, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

const MONITOR_PID: u8 = 1;
const MONITOR_EXIT: u8 = 2;
const MONITOR_EXEC_ERROR: u8 = 3;

pub(crate) fn spawn(
    config: &SpawnConfig,
    tty_name: &CStr,
) -> Result<Box<dyn Child + Send + Sync>, PtyxError> {
    let request = SpawnRequest::new(config, tty_name)?;
    let mut events = [0; 2];
    if unsafe { libc::pipe2(events.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(PtyxError::io(
            PtyxErrorKind::SpawnFailed,
            std::io::Error::last_os_error(),
        ));
    }

    let monitor_pid = unsafe { libc::fork() };
    if monitor_pid < 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(events[0]);
            libc::close(events[1]);
        }
        return Err(PtyxError::io(PtyxErrorKind::SpawnFailed, error));
    }

    if monitor_pid == 0 {
        unsafe {
            libc::close(events[0]);
            run_monitor(&request, events[1]);
        }
    }

    unsafe {
        libc::close(events[1]);
    }
    let events = Fd(unsafe { OwnedFd::from_raw_fd(events[0]) });
    let message = match read_message(events.raw(), PtyxErrorKind::SpawnFailed) {
        Ok(message) => message,
        Err(error) => {
            reap_pid(monitor_pid);
            return Err(error);
        }
    };
    match message.tag {
        MONITOR_PID => Ok(Box::new(SupervisedChild {
            pid: message.value,
            monitor_pid,
            events,
            cached: None,
        })),
        MONITOR_EXEC_ERROR => {
            let error = std::io::Error::from_raw_os_error(message.value);
            reap_pid(monitor_pid);
            Err(PtyxError::io(PtyxErrorKind::SpawnFailed, error))
        }
        _ => {
            reap_pid(monitor_pid);
            Err(PtyxError::new(
                PtyxErrorKind::SpawnFailed,
                "PTY child monitor returned an invalid spawn message",
            ))
        }
    }
}

struct SpawnRequest {
    executable: CString,
    _argv: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    _env: Vec<CString>,
    env_ptrs: Vec<*const libc::c_char>,
    cwd: Option<CString>,
    tty_name: CString,
}

impl SpawnRequest {
    fn new(config: &SpawnConfig, tty_name: &CStr) -> Result<Self, PtyxError> {
        let executable = cstring_os(&config.executable, "executable")?;
        let mut argv = Vec::with_capacity(config.argv.len() + 1);
        argv.push(cstring_os(&config.executable, "executable")?);
        for arg in &config.argv {
            argv.push(cstring_os(arg, "argument")?);
        }
        let argv_ptrs = null_terminated_ptrs(&argv);
        let env = environment(config)?;
        let env_ptrs = null_terminated_ptrs(&env);
        let cwd = if config.cwd.is_empty() {
            None
        } else {
            Some(cstring_os(&config.cwd, "cwd")?)
        };
        Ok(Self {
            executable,
            _argv: argv,
            argv_ptrs,
            _env: env,
            env_ptrs,
            cwd,
            tty_name: tty_name.to_owned(),
        })
    }
}

fn cstring_os(value: &std::ffi::OsStr, name: &str) -> Result<CString, PtyxError> {
    CString::new(value.as_bytes()).map_err(|_| {
        PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            format!("{name} must not contain NUL"),
        )
    })
}

fn null_terminated_ptrs(values: &[CString]) -> Vec<*const libc::c_char> {
    let mut ptrs: Vec<_> = values.iter().map(|value| value.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    ptrs
}

fn environment(config: &SpawnConfig) -> Result<Vec<CString>, PtyxError> {
    let mut env = std::collections::BTreeMap::<OsString, OsString>::new();
    if matches!(
        config.env_mode,
        EnvironmentMode::Inherit | EnvironmentMode::Overlay
    ) {
        env.extend(std::env::vars_os());
    }
    if matches!(
        config.env_mode,
        EnvironmentMode::Overlay | EnvironmentMode::Replace
    ) {
        for item in &config.env_items {
            let bytes = item.as_bytes();
            let Some(eq) = bytes.iter().position(|byte| *byte == b'=') else {
                return Err(PtyxError::new(
                    PtyxErrorKind::InvalidArgument,
                    "environment entry must be KEY=VALUE",
                ));
            };
            if eq == 0 {
                return Err(PtyxError::new(
                    PtyxErrorKind::InvalidArgument,
                    "environment key must not be empty",
                ));
            }
            env.insert(
                OsString::from_vec(bytes[..eq].to_vec()),
                OsString::from_vec(bytes[eq + 1..].to_vec()),
            );
        }
    }

    env.into_iter()
        .map(|(key, value)| {
            let mut item = key.into_vec();
            item.push(b'=');
            item.extend(value.into_vec());
            CString::new(item).map_err(|_| {
                PtyxError::new(
                    PtyxErrorKind::InvalidArgument,
                    "environment key and value must not contain NUL",
                )
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
struct MonitorMessage {
    tag: u8,
    value: i32,
}

#[derive(Debug)]
struct Fd(OwnedFd);

impl Fd {
    fn raw(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[derive(Debug)]
struct SupervisedChild {
    pid: libc::pid_t,
    monitor_pid: libc::pid_t,
    events: Fd,
    cached: Option<ExitStatus>,
}

impl Child for SupervisedChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(status) = self.cached.as_ref() {
            return Ok(Some(status.clone()));
        }
        let mut pollfd = libc::pollfd {
            fd: self.events.raw(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut pollfd, 1, 0) };
        if result == 0 {
            return Ok(None);
        }
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let status = self.read_exit_status()?;
        Ok(Some(status))
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.cached.as_ref() {
            return Ok(status.clone());
        }
        self.read_exit_status()
    }

    fn process_id(&self) -> Option<u32> {
        u32::try_from(self.pid).ok()
    }
}

impl ChildKiller for SupervisedChild {
    fn kill(&mut self) -> std::io::Result<()> {
        kill_pid(self.pid)
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(SupervisedKiller { pid: self.pid })
    }
}

impl SupervisedChild {
    fn read_exit_status(&mut self) -> std::io::Result<ExitStatus> {
        loop {
            let message = read_message(self.events.raw(), PtyxErrorKind::WaitFailed)
                .map_err(|error| std::io::Error::other(error.message))?;
            if message.tag == MONITOR_EXIT {
                let status = ExitStatus::with_exit_code(message.value as u32);
                reap_pid(self.monitor_pid);
                self.cached = Some(status.clone());
                return Ok(status);
            }
        }
    }
}

#[derive(Debug)]
struct SupervisedKiller {
    pid: libc::pid_t,
}

impl ChildKiller for SupervisedKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        kill_pid(self.pid)
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self { pid: self.pid })
    }
}

fn kill_pid(pid: libc::pid_t) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn reap_pid(pid: libc::pid_t) {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
    }
}

fn read_message(fd: RawFd, kind: PtyxErrorKind) -> Result<MonitorMessage, PtyxError> {
    let mut buffer = [0_u8; 8];
    read_exact_fd(fd, &mut buffer).map_err(|error| PtyxError::io(kind, error))?;
    Ok(MonitorMessage {
        tag: buffer[0],
        value: i32::from_ne_bytes(buffer[4..8].try_into().unwrap()),
    })
}

fn read_exact_fd(fd: RawFd, buffer: &mut [u8]) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        let result = unsafe {
            libc::read(
                fd,
                buffer[offset..].as_mut_ptr().cast(),
                buffer.len() - offset,
            )
        };
        if result == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        offset += result as usize;
    }
    Ok(())
}

unsafe fn run_monitor(spawn: &SpawnRequest, event_fd: libc::c_int) -> ! {
    reset_signal_raw(libc::SIGCHLD);

    let mut exec_pipe = [0; 2];
    if libc::pipe2(exec_pipe.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
        write_message_raw(event_fd, MONITOR_EXEC_ERROR, errno());
        libc::_exit(1);
    }

    let child_pid = libc::fork();
    if child_pid < 0 {
        write_message_raw(event_fd, MONITOR_EXEC_ERROR, errno());
        libc::_exit(1);
    }

    if child_pid == 0 {
        libc::close(exec_pipe[0]);
        libc::close(event_fd);
        exec_child(spawn, exec_pipe[1]);
    }

    libc::close(exec_pipe[1]);
    let mut exec_error = 0_i32;
    match read_i32_or_eof_raw(exec_pipe[0], &mut exec_error) {
        1 => {
            write_message_raw(event_fd, MONITOR_EXEC_ERROR, exec_error);
            wait_for_pid_raw(child_pid);
            libc::_exit(1);
        }
        -1 => {
            write_message_raw(event_fd, MONITOR_EXEC_ERROR, errno());
            wait_for_pid_raw(child_pid);
            libc::_exit(1);
        }
        _ => {}
    }
    libc::close(exec_pipe[0]);

    write_message_raw(event_fd, MONITOR_PID, child_pid);
    let exit_code = wait_for_pid_raw(child_pid);
    write_message_raw(event_fd, MONITOR_EXIT, exit_code);
    libc::_exit(0);
}

unsafe fn exec_child(spawn: &SpawnRequest, exec_error_fd: libc::c_int) -> ! {
    for signal in [
        libc::SIGCHLD,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGALRM,
    ] {
        reset_signal_raw(signal);
    }

    let empty_set: libc::sigset_t = std::mem::zeroed();
    libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());

    if libc::setsid() == -1 {
        report_exec_error(exec_error_fd);
    }

    let tty_fd = libc::open(spawn.tty_name.as_ptr(), libc::O_RDWR);
    if tty_fd < 0 {
        report_exec_error(exec_error_fd);
    }
    if libc::ioctl(tty_fd, libc::TIOCSCTTY as _, 0) == -1 {
        report_exec_error(exec_error_fd);
    }
    if libc::dup2(tty_fd, libc::STDIN_FILENO) == -1
        || libc::dup2(tty_fd, libc::STDOUT_FILENO) == -1
        || libc::dup2(tty_fd, libc::STDERR_FILENO) == -1
    {
        report_exec_error(exec_error_fd);
    }
    if tty_fd > libc::STDERR_FILENO {
        libc::close(tty_fd);
    }

    if let Some(cwd) = spawn.cwd.as_ref() {
        if libc::chdir(cwd.as_ptr()) == -1 {
            report_exec_error(exec_error_fd);
        }
    }

    libc::execvpe(
        spawn.executable.as_ptr(),
        spawn.argv_ptrs.as_ptr(),
        spawn.env_ptrs.as_ptr(),
    );
    report_exec_error(exec_error_fd);
}

unsafe fn reset_signal_raw(signal: libc::c_int) {
    let mut action: libc::sigaction = std::mem::zeroed();
    action.sa_sigaction = libc::SIG_DFL;
    action.sa_flags = 0;
    libc::sigemptyset(&mut action.sa_mask);
    libc::sigaction(signal, &action, std::ptr::null_mut());
}

unsafe fn report_exec_error(fd: libc::c_int) -> ! {
    write_i32_raw(fd, errno());
    libc::_exit(127);
}

unsafe fn wait_for_pid_raw(pid: libc::pid_t) -> i32 {
    let mut status = 0;
    loop {
        let result = libc::waitpid(pid, &mut status, 0);
        if result == pid {
            if libc::WIFEXITED(status) {
                return libc::WEXITSTATUS(status);
            }
            if libc::WIFSIGNALED(status) {
                return 128 + libc::WTERMSIG(status);
            }
            return 1;
        }
        if result < 0 && errno() == libc::EINTR {
            continue;
        }
        return 1;
    }
}

unsafe fn read_i32_or_eof_raw(fd: libc::c_int, out: &mut i32) -> i32 {
    let mut buffer = [0_u8; 4];
    let mut offset = 0;
    while offset < buffer.len() {
        let result = libc::read(
            fd,
            buffer[offset..].as_mut_ptr().cast(),
            buffer.len() - offset,
        );
        if result == 0 {
            if offset == 0 {
                return 0;
            }
            return -1;
        }
        if result < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            return -1;
        }
        offset += result as usize;
    }
    *out = i32::from_ne_bytes(buffer);
    1
}

unsafe fn write_message_raw(fd: libc::c_int, tag: u8, value: i32) -> bool {
    let mut buffer = [0_u8; 8];
    buffer[0] = tag;
    buffer[4..8].copy_from_slice(&value.to_ne_bytes());
    write_all_raw(fd, buffer.as_ptr(), buffer.len())
}

unsafe fn write_i32_raw(fd: libc::c_int, value: i32) -> bool {
    let buffer = value.to_ne_bytes();
    write_all_raw(fd, buffer.as_ptr(), buffer.len())
}

unsafe fn write_all_raw(fd: libc::c_int, ptr: *const u8, len: usize) -> bool {
    let mut offset = 0;
    while offset < len {
        let result = libc::write(fd, ptr.add(offset).cast(), len - offset);
        if result < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            return false;
        }
        offset += result as usize;
    }
    true
}

unsafe fn errno() -> i32 {
    *libc::__errno_location()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::PtySize;

    #[test]
    fn environment_entries_reject_missing_separator() {
        let config = config_with_env([OsString::from("INVALID")]);

        let error = environment(&config).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_empty_key() {
        let config = config_with_env([OsString::from("=value")]);

        let error = environment(&config).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn environment_entries_reject_nul_value() {
        let config = config_with_env([OsString::from("INVALID=bad\0value")]);

        let error = environment(&config).unwrap_err();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    fn config_with_env(items: impl IntoIterator<Item = OsString>) -> SpawnConfig {
        SpawnConfig {
            executable: OsString::from("/usr/bin/env"),
            argv: Vec::new(),
            env_items: items.into_iter().collect(),
            env_mode: EnvironmentMode::Overlay,
            cwd: OsString::new(),
            size: PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        }
    }
}
