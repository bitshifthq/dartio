#![cfg(target_os = "macos")]

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;

const MAX_NOTICE_GENERATION: u32 = ((i64::MAX as u64 >> 3) >> 32) as u32;

mod broker_client;
mod broker_materializer;
mod ffi;
mod integrated;

pub use integrated::{IntegratedRuntime, Notice, RuntimeCounters, WaitResult};

pub struct BoundedInput {
    capacity: usize,
    queued_bytes: usize,
    chunks: VecDeque<Vec<u8>>,
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub struct GenerationRegistry<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> GenerationRegistry<T> {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub fn insert(&mut self, value: T) -> u64 {
        let index = self.free.pop().unwrap_or_else(|| {
            self.slots.push(Slot {
                generation: 1,
                value: None,
            });
            self.slots.len() - 1
        });
        let slot = &mut self.slots[index];
        slot.value = Some(value);
        ((slot.generation as u64) << 32) | (index as u64 + 1)
    }

    pub fn get(&self, handle: u64) -> Option<&T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub fn get_mut(&mut self, handle: u64) -> Option<&mut T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }

    pub fn handles(&self) -> Vec<u64> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.value
                    .as_ref()
                    .map(|_| ((slot.generation as u64) << 32) | (index as u64 + 1))
            })
            .collect()
    }

    pub fn remove(&mut self, handle: u64) -> Option<T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        if slot.generation < MAX_NOTICE_GENERATION {
            slot.generation += 1;
            self.free.push(index);
        }
        Some(value)
    }
}

impl<T> Default for GenerationRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_handle(handle: u64) -> Option<(usize, u32)> {
    let index = (handle as u32).checked_sub(1)? as usize;
    let generation = (handle >> 32) as u32;
    (generation != 0).then_some((index, generation))
}

pub struct DirectSession {
    pid: libc::pid_t,
    master: OwnedFd,
}

impl DirectSession {
    pub fn spawn(executable: &CStr, arguments: &[CString]) -> io::Result<Self> {
        let mut argv: Vec<*const libc::c_char> = Vec::with_capacity(arguments.len() + 2);
        argv.push(executable.as_ptr());
        argv.extend(arguments.iter().map(|argument| argument.as_ptr()));
        argv.push(ptr::null());

        let (master, slave) = allocate_pty()?;
        let mut error_pipe = [-1; 2];
        if unsafe { libc::pipe(error_pipe.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let error_read = unsafe { OwnedFd::from_raw_fd(error_pipe[0]) };
        let error_write = unsafe { OwnedFd::from_raw_fd(error_pipe[1]) };
        set_cloexec(error_read.as_raw_fd())?;
        set_cloexec(error_write.as_raw_fd())?;
        let maximum_fd = current_descriptor_limit()?;

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                libc::close(error_read.as_raw_fd());
                exec_child(
                    executable,
                    &argv,
                    slave.as_raw_fd(),
                    error_write.as_raw_fd(),
                    maximum_fd,
                );
            }
        }

        drop(error_write);
        drop(slave);
        match read_exec_error(error_read.as_raw_fd()) {
            Ok(None) => Ok(Self { pid, master }),
            Ok(Some(code)) => {
                reap(pid);
                Err(io::Error::from_raw_os_error(code))
            }
            Err(error) => {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
                reap(pid);
                Err(error)
            }
        }
    }

    pub fn read_to_end(self) -> io::Result<Vec<u8>> {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let amount = unsafe {
                libc::read(
                    self.master.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if amount > 0 {
                output.extend_from_slice(&buffer[..amount as usize]);
                continue;
            }
            if amount == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::EIO) {
                break;
            }
            return Err(error);
        }
        reap(self.pid);
        Ok(output)
    }
}

fn allocate_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_cloexec(master.as_raw_fd())?;
    set_cloexec(slave.as_raw_fd())?;
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut termios = unsafe { termios.assume_init() };
    unsafe { libc::cfmakeraw(&mut termios) };
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn current_descriptor_limit() -> io::Result<libc::c_int> {
    let mut limit = MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let limit = unsafe { limit.assume_init() };
    Ok(limit.rlim_cur.clamp(4, libc::c_int::MAX as libc::rlim_t) as libc::c_int)
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_exec_error(fd: RawFd) -> io::Result<Option<i32>> {
    let mut bytes = [0_u8; std::mem::size_of::<i32>()];
    let mut offset = 0;
    loop {
        let amount = unsafe {
            libc::read(
                fd,
                bytes[offset..].as_mut_ptr().cast(),
                bytes.len() - offset,
            )
        };
        if amount > 0 {
            offset += amount as usize;
            if offset == bytes.len() {
                return Ok(Some(i32::from_ne_bytes(bytes)));
            }
            continue;
        }
        if amount == 0 {
            return if offset == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial exec error",
                ))
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

fn reap(pid: libc::pid_t) {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return;
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

unsafe fn exec_child(
    executable: &CStr,
    argv: &[*const libc::c_char],
    slave: RawFd,
    error_fd: RawFd,
    maximum_fd: libc::c_int,
) -> ! {
    if error_fd != 3 && libc::dup2(error_fd, 3) < 0 {
        libc::_exit(127);
    }
    let flags = libc::fcntl(3, libc::F_GETFD);
    if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
        libc::_exit(127);
    }
    let empty: libc::sigset_t = std::mem::zeroed();
    libc::sigprocmask(libc::SIG_SETMASK, &empty, ptr::null_mut());
    for signal in [
        libc::SIGCHLD,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGALRM,
        libc::SIGPIPE,
    ] {
        libc::signal(signal, libc::SIG_DFL);
    }
    if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
        child_fail(3);
    }
    if libc::dup2(slave, libc::STDIN_FILENO) < 0
        || libc::dup2(slave, libc::STDOUT_FILENO) < 0
        || libc::dup2(slave, libc::STDERR_FILENO) < 0
    {
        child_fail(3);
    }
    for fd in 4..maximum_fd {
        libc::close(fd);
    }
    libc::execve(
        executable.as_ptr(),
        argv.as_ptr(),
        environ as *const *const libc::c_char,
    );
    child_fail(3)
}

unsafe fn child_fail(error_fd: RawFd) -> ! {
    let error = *libc::__error();
    libc::write(
        error_fd,
        (&error as *const i32).cast(),
        std::mem::size_of::<i32>(),
    );
    libc::_exit(127)
}

unsafe extern "C" {
    static mut environ: *mut *mut libc::c_char;
}

impl BoundedInput {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            queued_bytes: 0,
            chunks: VecDeque::new(),
        }
    }

    pub fn try_push(&mut self, bytes: Vec<u8>) -> Result<(), Vec<u8>> {
        if self.queued_bytes.saturating_add(bytes.len()) > self.capacity {
            return Err(bytes);
        }
        self.queued_bytes += bytes.len();
        self.chunks.push_back(bytes);
        Ok(())
    }

    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }
}
