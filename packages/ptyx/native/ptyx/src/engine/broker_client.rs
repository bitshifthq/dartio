use crate::engine::control::{fail_wake_socket, wake_socket};
#[cfg(target_os = "macos")]
use crate::engine::dup_cloexec;
use crate::engine::integrated::Command;
use crate::engine::oneshot::{self, Sender as ReplySender};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAGIC: u32 = 0x4258_5450;
const VERSION: u16 = 1;
const HEADER: usize = 32;
const MAX_PAYLOAD: usize = 64 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const SPAWN_V2: u32 = 0x5854_5950;
const CONTROL_FD: RawFd = 3;
const REAPER_CAPACITY: usize = 16;
static REAPER_HEALTHY: AtomicBool = AtomicBool::new(true);
#[cfg(target_os = "macos")]
const POSIX_SPAWN_CLOEXEC_DEFAULT: libc::c_int = 0x4000;

const HELLO: u16 = 1;
const SPAWN: u16 = 2;
const SPAWN_OK: u16 = 3;
const ERROR: u16 = 4;
const CLOSE: u16 = 5;
const CLOSE_RESULT: u16 = 6;
const CLOSE_KILLED: u32 = 1;
const CLOSE_ALREADY_EXITED: u32 = 2;
const CLOSE_STALE: u32 = 3;
const EXIT: u16 = 7;
const RELEASE: u16 = 8;
const RELEASE_RESULT: u16 = 9;
const SHUTDOWN: u16 = 12;
const SHUTDOWN_RESULT: u16 = 13;
const SIGNAL: u16 = 14;
const SIGNAL_RESULT: u16 = 15;

const REQUEST_CAPACITY: usize = 128;
const REQUEST_QUANTUM: usize = 16;

fn hello_payload() -> Vec<u8> {
    format!(
        "{}|{}|{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        2
    )
    .into_bytes()
}

#[derive(Clone, Debug)]
struct Frame {
    kind: u16,
    request: u32,
    session: u64,
    code: i32,
    aux: u32,
    payload: Vec<u8>,
}

impl Frame {
    fn new(kind: u16) -> Self {
        Self {
            kind,
            request: 0,
            session: 0,
            code: 0,
            aux: 0,
            payload: Vec::new(),
        }
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "broker payload exceeds bound",
            ));
        }
        let mut bytes = vec![0; HEADER + self.payload.len()];
        bytes[0..4].copy_from_slice(&MAGIC.to_ne_bytes());
        bytes[4..6].copy_from_slice(&VERSION.to_ne_bytes());
        bytes[6..8].copy_from_slice(&self.kind.to_ne_bytes());
        bytes[8..12].copy_from_slice(&(self.payload.len() as u32).to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.request.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.session.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.code.to_ne_bytes());
        bytes[28..32].copy_from_slice(&self.aux.to_ne_bytes());
        bytes[HEADER..].copy_from_slice(&self.payload);
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short broker frame",
            ));
        }
        let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        let version = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        let length = u32::from_ne_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if magic != MAGIC || version != VERSION || length > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker header rejected",
            ));
        }
        if bytes.len() != HEADER + length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker frame length mismatch",
            ));
        }
        Ok(Self {
            kind: u16::from_ne_bytes(bytes[6..8].try_into().unwrap()),
            request: u32::from_ne_bytes(bytes[12..16].try_into().unwrap()),
            session: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
            code: i32::from_ne_bytes(bytes[24..28].try_into().unwrap()),
            aux: u32::from_ne_bytes(bytes[28..32].try_into().unwrap()),
            payload: bytes[HEADER..].to_vec(),
        })
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz_protocol_frame(bytes: &[u8]) {
    let _ = Frame::decode(bytes);
}

fn send_frame(fd: RawFd, frame: &Frame) -> io::Result<()> {
    let bytes = frame.encode()?;
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let mut offset = 0;
    while offset < bytes.len() {
        let written = unsafe {
            libc::send(
                fd,
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if written > 0 {
            offset += written as usize;
            continue;
        }
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                wait_for_io(fd, libc::POLLOUT, deadline)?;
                continue;
            }
            return Err(error);
        }
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "broker write returned zero",
        ));
    }
    Ok(())
}

fn receive_frame(fd: RawFd) -> io::Result<Option<(Frame, Option<OwnedFd>)>> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let mut bytes = vec![0_u8; HEADER];
    let mut offset = 0;
    let mut received_fds = Vec::new();
    while offset < bytes.len() {
        let mut iovec = libc::iovec {
            iov_base: bytes[offset..].as_mut_ptr().cast(),
            iov_len: bytes.len() - offset,
        };
        let mut control = [0_usize; 8];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iovec;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control) as _;
        #[cfg(target_os = "linux")]
        let receive_flags = libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC;
        #[cfg(target_os = "macos")]
        let receive_flags = libc::MSG_DONTWAIT;
        let received = unsafe { libc::recvmsg(fd, &mut message, receive_flags) };
        if received == 0 {
            return if offset == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial broker frame",
                ))
            };
        }
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                wait_for_io(fd, libc::POLLIN, deadline)?;
                continue;
            }
            return Err(error);
        }
        collect_received_fds(&message, &mut received_fds)?;
        offset += received as usize;
        if offset == HEADER {
            let length = u32::from_ne_bytes(bytes[8..12].try_into().unwrap()) as usize;
            if length > MAX_PAYLOAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "broker payload exceeds bound",
                ));
            }
            bytes.resize(HEADER + length, 0);
        }
    }
    if received_fds.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker sent multiple file descriptors",
        ));
    }
    Ok(Some((Frame::decode(&bytes)?, received_fds.pop())))
}

fn collect_received_fds(message: &libc::msghdr, received_fds: &mut Vec<OwnedFd>) -> io::Result<()> {
    let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !header.is_null() {
        if unsafe { (*header).cmsg_level != libc::SOL_SOCKET }
            || unsafe { (*header).cmsg_type != libc::SCM_RIGHTS }
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected broker ancillary data",
            ));
        }
        let data_length = (unsafe { (*header).cmsg_len } as usize)
            .saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
        if data_length == 0 || !data_length.is_multiple_of(size_of::<RawFd>()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid broker descriptor payload",
            ));
        }
        for offset in (0..data_length).step_by(size_of::<RawFd>()) {
            let mut raw = -1;
            unsafe {
                ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(header).add(offset),
                    (&mut raw as *mut RawFd).cast::<u8>(),
                    size_of::<RawFd>(),
                );
            }
            if raw >= 0 {
                let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
                set_cloexec(descriptor.as_raw_fd())?;
                received_fds.push(descriptor);
            }
        }
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    if message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated broker frame or ancillary data",
        ));
    }
    Ok(())
}

fn wait_for_io(fd: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "broker frame deadline exceeded",
            ));
        }
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let timeout = remaining.as_millis().max(1).min(libc::c_int::MAX as u128) as libc::c_int;
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready > 0 {
            if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "broker control socket failed",
                ));
            }
            return Ok(());
        }
        if ready == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut sockets = [-1; 2];
    #[cfg(target_os = "macos")]
    let socket_type = libc::SOCK_STREAM;
    #[cfg(target_os = "linux")]
    let socket_type = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    if unsafe { libc::socketpair(libc::AF_UNIX, socket_type, 0, sockets.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let first = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
    let second = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
    set_cloexec(first.as_raw_fd())?;
    set_cloexec(second.as_raw_fd())?;
    Ok((first, second))
}

#[cfg(target_os = "macos")]
struct SpawnAttrs(libc::posix_spawnattr_t);

#[cfg(target_os = "macos")]
impl Drop for SpawnAttrs {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawnattr_destroy(&mut self.0);
        }
    }
}

#[cfg(target_os = "macos")]
struct FileActions(libc::posix_spawn_file_actions_t);

#[cfg(target_os = "macos")]
impl Drop for FileActions {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawn_file_actions_destroy(&mut self.0);
        }
    }
}

struct BrokerProcessGuard {
    pid: libc::pid_t,
    armed: bool,
}

impl BrokerProcessGuard {
    fn new(pid: libc::pid_t) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(mut self) -> libc::pid_t {
        self.armed = false;
        self.pid
    }
}

impl Drop for BrokerProcessGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        terminate_process(self.pid);
        reap_process(self.pid);
    }
}

#[cfg(target_os = "macos")]
fn spawn_code(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

#[cfg(target_os = "macos")]
fn launch_broker_at(path: &CStr) -> io::Result<(OwnedFd, libc::pid_t)> {
    let (controller, broker) = socket_pair()?;
    let broker_duplicate = if broker.as_raw_fd() == CONTROL_FD {
        Some(dup_cloexec(broker.as_raw_fd())?)
    } else {
        None
    };
    let broker_fd = broker_duplicate.as_ref().unwrap_or(&broker).as_raw_fd();
    let mut attrs_raw = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
    spawn_code(unsafe { libc::posix_spawnattr_init(attrs_raw.as_mut_ptr()) })?;
    let mut attrs = SpawnAttrs(unsafe { attrs_raw.assume_init() });
    let mut actions_raw = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    spawn_code(unsafe { libc::posix_spawn_file_actions_init(actions_raw.as_mut_ptr()) })?;
    let mut actions = FileActions(unsafe { actions_raw.assume_init() });

    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let empty = unsafe { empty.assume_init() };
    spawn_code(unsafe { libc::posix_spawnattr_setsigmask(&mut attrs.0, &empty) })?;
    let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(defaults.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut defaults = unsafe { defaults.assume_init() };
    if unsafe { libc::sigaddset(&mut defaults, libc::SIGCHLD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    spawn_code(unsafe { libc::posix_spawnattr_setsigdefault(&mut attrs.0, &defaults) })?;
    #[cfg(target_os = "macos")]
    let flags =
        POSIX_SPAWN_CLOEXEC_DEFAULT | libc::POSIX_SPAWN_SETSIGMASK | libc::POSIX_SPAWN_SETSIGDEF;
    #[cfg(target_os = "linux")]
    let flags = libc::POSIX_SPAWN_SETSIGMASK;
    let flags = flags as libc::c_short;
    spawn_code(unsafe { libc::posix_spawnattr_setflags(&mut attrs.0, flags) })?;
    // Make replacement of an inherited descriptor explicit. In particular,
    // hosted processes may already use fd 3 for an unrelated non-CLOEXEC
    // channel; relying on dup2's implicit close interacted inconsistently
    // with POSIX_SPAWN_CLOEXEC_DEFAULT on macOS x64.
    if unsafe { libc::fcntl(CONTROL_FD, libc::F_GETFD) } >= 0 {
        spawn_code(unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, CONTROL_FD) })?;
    }
    spawn_code(unsafe {
        libc::posix_spawn_file_actions_adddup2(&mut actions.0, broker_fd, CONTROL_FD)
    })?;
    spawn_code(unsafe { libc::posix_spawn_file_actions_addclose(&mut actions.0, broker_fd) })?;

    let broker_arg = CString::new("--broker").unwrap();
    let mut argv = [
        path.as_ptr().cast_mut(),
        broker_arg.as_ptr().cast_mut(),
        ptr::null_mut(),
    ];
    let mut pid = 0;
    let code = unsafe {
        libc::posix_spawn(
            &mut pid,
            path.as_ptr(),
            &actions.0,
            &attrs.0,
            argv.as_mut_ptr(),
            environ,
        )
    };
    spawn_code(code)?;
    let broker_process = BrokerProcessGuard::new(pid);
    drop(broker_duplicate);
    drop(broker);
    let (hello, passed) = receive_frame(controller.as_raw_fd())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "broker handshake EOF"))?;
    if hello.kind != HELLO
        || hello.aux != VERSION as u32
        || hello.payload != hello_payload()
        || passed.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker handshake rejected",
        ));
    }
    Ok((controller, broker_process.disarm()))
}

#[cfg(target_os = "linux")]
fn launch_broker_at(path: &CStr) -> io::Result<(OwnedFd, libc::pid_t)> {
    let (controller, broker) = socket_pair()?;
    let mut error_pipe = [-1; 2];
    if unsafe { libc::pipe2(error_pipe.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let error_read = unsafe { OwnedFd::from_raw_fd(error_pipe[0]) };
    let error_write = unsafe { OwnedFd::from_raw_fd(error_pipe[1]) };
    set_nonblocking(error_read.as_raw_fd())?;
    let broker_arg = CString::new("--broker").unwrap();
    let argv = [path.as_ptr(), broker_arg.as_ptr(), ptr::null()];
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let empty = unsafe { empty.assume_init() };
    let mut limit = MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let maximum_fd = unsafe { limit.assume_init() }.rlim_cur.min(1_048_576) as RawFd;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            launch_broker_child(
                path,
                &argv,
                broker.as_raw_fd(),
                error_write.as_raw_fd(),
                &empty,
                maximum_fd,
            );
        }
    }
    let broker_process = BrokerProcessGuard::new(pid);
    drop(broker);
    drop(error_write);
    if let Some(code) = read_child_error(error_read.as_raw_fd())? {
        return Err(io::Error::from_raw_os_error(code));
    }
    let handshake = receive_frame(controller.as_raw_fd());
    let (hello, passed) = match handshake {
        Ok(Some(frame)) => frame,
        Ok(None) => {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "broker handshake EOF",
            ));
        }
        Err(error) => return Err(error),
    };
    if hello.kind != HELLO
        || hello.aux != VERSION as u32
        || hello.payload != hello_payload()
        || passed.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker handshake rejected",
        ));
    }
    Ok((controller, broker_process.disarm()))
}

#[cfg(target_os = "linux")]
unsafe fn launch_broker_child(
    path: &CStr,
    argv: &[*const libc::c_char; 3],
    broker_fd: RawFd,
    error_fd: RawFd,
    empty_mask: &libc::sigset_t,
    maximum_fd: RawFd,
) -> ! {
    if libc::dup2(broker_fd, CONTROL_FD) < 0
        || libc::fcntl(CONTROL_FD, libc::F_SETFD, 0) < 0
        || (error_fd != 4 && libc::dup3(error_fd, 4, libc::O_CLOEXEC) < 0)
        || libc::sigprocmask(libc::SIG_SETMASK, empty_mask, ptr::null_mut()) < 0
    {
        launch_child_fail(if error_fd == 4 { error_fd } else { 4 });
    }
    let close_result = libc::syscall(libc::SYS_close_range, 5_u32, u32::MAX, 0_u32);
    if close_result < 0 && current_errno() == libc::ENOSYS {
        for fd in 5..maximum_fd {
            libc::close(fd);
        }
    } else if close_result < 0 {
        launch_child_fail(4);
    }
    libc::execve(path.as_ptr(), argv.as_ptr(), environ.cast());
    launch_child_fail(4)
}

#[cfg(target_os = "linux")]
unsafe fn launch_child_fail(error_fd: RawFd) -> ! {
    let bytes = current_errno().to_ne_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let written = libc::write(
            error_fd,
            bytes[offset..].as_ptr().cast(),
            bytes.len() - offset,
        );
        if written > 0 {
            offset += written as usize;
        } else if written < 0 && current_errno() == libc::EINTR {
            continue;
        } else {
            break;
        }
    }
    libc::_exit(127)
}

#[cfg(target_os = "linux")]
unsafe fn current_errno() -> libc::c_int {
    *libc::__errno_location()
}

#[cfg(target_os = "linux")]
fn read_child_error(fd: RawFd) -> io::Result<Option<libc::c_int>> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let mut bytes = [0_u8; size_of::<libc::c_int>()];
    let mut offset = 0;
    loop {
        let read = unsafe {
            libc::read(
                fd,
                bytes[offset..].as_mut_ptr().cast(),
                bytes.len() - offset,
            )
        };
        if read > 0 {
            offset += read as usize;
            if offset == bytes.len() {
                return Ok(Some(libc::c_int::from_ne_bytes(bytes)));
            }
            continue;
        }
        if read == 0 {
            return if offset == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial broker exec failure record",
                ))
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock {
            wait_for_io(fd, libc::POLLIN, deadline)?;
            continue;
        }
        return Err(error);
    }
}

pub(crate) struct BrokerSession {
    pub(crate) id: u64,
    pub(crate) pid: libc::pid_t,
    pub(crate) master: OwnedFd,
}

use crate::engine::spawn::BrokerSpawn;
use crate::error::{Operation, OperationError};

enum Request {
    Spawn {
        config: BrokerSpawn,
        reply: ReplySender<io::Result<BrokerSession>>,
    },
    CloseDetached {
        session: u64,
        handle: u64,
    },
    GracefulSignal {
        session: u64,
        handle: u64,
    },
    ForceClose {
        session: u64,
        handle: u64,
    },
    Release {
        session: u64,
        handle: u64,
    },
    Abort {
        session: u64,
        reply: ReplySender<io::Result<()>>,
    },
    AbortDetached {
        session: u64,
    },
    Signal {
        session: u64,
        signal: i32,
        reply: ReplySender<Result<bool, OperationError>>,
    },
    Shutdown,
}

struct Shared {
    requests: SyncSender<Request>,
    wake: OwnedFd,
}

#[derive(Clone)]
pub(crate) struct BrokerClient {
    shared: Arc<Shared>,
}

pub(crate) struct BrokerOwner {
    client: BrokerClient,
    pid: libc::pid_t,
    thread: Mutex<Option<JoinHandle<bool>>>,
}

impl BrokerOwner {
    pub(crate) fn launch(
        path: &CStr,
        reactor_commands: SyncSender<Command>,
        reactor_wake: OwnedFd,
    ) -> io::Result<Self> {
        if !REAPER_HEALTHY.load(Ordering::Acquire) {
            return Err(io::Error::other("broker reaper is unavailable"));
        }
        let (control, broker_pid) = launch_broker_at(path)?;
        let broker_process = BrokerProcessGuard::new(broker_pid);
        let (wake_read, wake_write) = socket_pair()?;
        set_cloexec(wake_read.as_raw_fd())?;
        set_cloexec(wake_write.as_raw_fd())?;
        set_nonblocking(wake_read.as_raw_fd())?;
        set_nonblocking(wake_write.as_raw_fd())?;
        let worker_wake_fd = unsafe { libc::dup(wake_write.as_raw_fd()) };
        if worker_wake_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let worker_wake_write = unsafe { OwnedFd::from_raw_fd(worker_wake_fd) };
        set_cloexec(worker_wake_write.as_raw_fd())?;
        set_nonblocking(worker_wake_write.as_raw_fd())?;
        let (request_sender, request_receiver) = mpsc::sync_channel(REQUEST_CAPACITY);
        let shared = Arc::new(Shared {
            requests: request_sender,
            wake: wake_write,
        });
        let thread = thread::Builder::new()
            .name("ptyx-broker-controller".to_owned())
            .spawn(move || {
                run_worker(
                    control,
                    broker_pid,
                    wake_read,
                    worker_wake_write,
                    request_receiver,
                    reactor_commands,
                    reactor_wake,
                )
            })?;
        broker_process.disarm();
        Ok(Self {
            client: BrokerClient { shared },
            pid: broker_pid,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn client(&self) -> BrokerClient {
        self.client.clone()
    }

    pub(crate) fn shutdown(&self) -> bool {
        let Some(thread) = self.thread.lock().ok().and_then(|mut value| value.take()) else {
            return true;
        };
        if self.client.send(Request::Shutdown).is_err() {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
        }
        thread.join().unwrap_or(false)
    }
}

impl Drop for BrokerOwner {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl BrokerClient {
    pub(crate) fn spawn(&self, config: BrokerSpawn) -> io::Result<BrokerSession> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Spawn { config, reply })?;
        response
            .recv()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    pub(crate) fn close_async(&self, session: u64, handle: u64) -> io::Result<()> {
        self.send(Request::CloseDetached { session, handle })
    }

    pub(crate) fn graceful_signal_async(&self, session: u64, handle: u64) -> io::Result<()> {
        self.send(Request::GracefulSignal { session, handle })
    }

    pub(crate) fn force_close_async(&self, session: u64, handle: u64) -> io::Result<()> {
        self.send(Request::ForceClose { session, handle })
    }

    pub(crate) fn release_async(&self, session: u64, handle: u64) -> io::Result<()> {
        self.send(Request::Release { session, handle })
    }

    pub(crate) fn abort(&self, session: u64) -> io::Result<()> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Abort { session, reply })?;
        response
            .recv()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    pub(crate) fn abort_async(&self, session: u64) -> io::Result<()> {
        self.send(Request::AbortDetached { session })
    }

    pub(crate) fn signal_async(
        &self,
        session: u64,
        signal: i32,
        reply: ReplySender<Result<bool, OperationError>>,
    ) -> io::Result<()> {
        self.send(Request::Signal {
            session,
            signal,
            reply,
        })
    }

    fn send(&self, request: Request) -> io::Result<()> {
        self.shared
            .requests
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "broker request queue is full")
                }
                TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped")
                }
            })?;
        // Queue insertion is the admission linearization point. A wake error
        // cannot turn an already-owned request back into a rejection; the
        // worker's control-socket path will report infrastructure loss if the
        // worker has actually stopped.
        if wake_socket(self.shared.wake.as_raw_fd()).is_err() {
            fail_wake_socket(self.shared.wake.as_raw_fd());
        }
        Ok(())
    }
}

struct Worker {
    control: OwnedFd,
    broker_pid: libc::pid_t,
    next_request: u32,
    reactor_commands: SyncSender<Command>,
    reactor_wake: OwnedFd,
    exits: HashMap<u64, i32>,
}

impl Worker {
    fn request_id(&mut self) -> u32 {
        let request = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        request
    }

    fn receive(&mut self) -> io::Result<(Frame, Option<OwnedFd>)> {
        receive_frame(self.control.as_raw_fd())?
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "broker control EOF"))
    }

    fn dispatch_exit(&self, frame: &Frame) {
        if self
            .reactor_commands
            .send(Command::BrokerExit {
                broker_session: frame.session,
                status: frame.code,
            })
            .is_ok()
        {
            let _ = wake_socket(self.reactor_wake.as_raw_fd());
        }
    }

    fn record_exit(&mut self, frame: &Frame) {
        self.exits.insert(frame.session, frame.code);
        self.dispatch_exit(frame);
    }

    fn receive_for(&mut self, request: u32, kind: u16) -> io::Result<(Frame, Option<OwnedFd>)> {
        loop {
            let (frame, fd) = self.receive()?;
            if frame.kind == EXIT {
                self.record_exit(&frame);
                continue;
            }
            if frame.request == request && frame.kind == kind {
                return Ok((frame, fd));
            }
            if frame.request == request && frame.kind == ERROR {
                return Err(io::Error::from_raw_os_error(frame.code));
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected broker response",
            ));
        }
    }

    fn spawn(&mut self, config: BrokerSpawn) -> io::Result<BrokerSession> {
        config.validate()?;
        let request = self.request_id();
        let mut payload = Vec::new();
        payload.extend_from_slice(&0_u32.to_ne_bytes());
        payload.extend_from_slice(&SPAWN_V2.to_ne_bytes());
        payload.extend_from_slice(&((config.arguments.len() + 1) as u32).to_ne_bytes());
        payload.extend_from_slice(
            &config
                .environment
                .as_ref()
                .map_or(u32::MAX, |environment| environment.len() as u32)
                .to_ne_bytes(),
        );
        for value in [
            config.rows,
            config.columns,
            config.pixel_width,
            config.pixel_height,
        ] {
            payload.extend_from_slice(&value.to_ne_bytes());
        }
        payload.extend_from_slice(
            &(config
                .cwd
                .as_ref()
                .map_or(0, |cwd| cwd.as_os_str().as_bytes().len()) as u32)
                .to_ne_bytes(),
        );
        for argument in std::iter::once(&config.executable).chain(config.arguments.iter()) {
            let bytes = argument.as_os_str().as_bytes();
            payload.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
            payload.extend_from_slice(bytes);
        }
        if let Some(environment) = &config.environment {
            for (key, value) in environment {
                let key = key.as_os_str().as_bytes();
                let value = value.as_os_str().as_bytes();
                let length = key.len().saturating_add(1).saturating_add(value.len());
                let length = u32::try_from(length).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "environment entry is too large",
                    )
                })?;
                payload.extend_from_slice(&length.to_ne_bytes());
                payload.extend_from_slice(key);
                payload.push(b'=');
                payload.extend_from_slice(value);
            }
        }
        if let Some(cwd) = &config.cwd {
            payload.extend_from_slice(cwd.as_os_str().as_bytes());
        }
        let mut frame = Frame::new(SPAWN);
        frame.request = request;
        frame.payload = payload;
        send_frame(self.control.as_raw_fd(), &frame)?;
        let (response, master) = self.receive_for(request, SPAWN_OK)?;
        let Some(master) = master else {
            let error = io::Error::new(io::ErrorKind::InvalidData, "broker omitted PTY master");
            let _ = self.abort(response.session);
            return Err(error);
        };
        if let Err(error) =
            set_cloexec(master.as_raw_fd()).and_then(|()| set_nonblocking(master.as_raw_fd()))
        {
            let _ = self.abort(response.session);
            return Err(error);
        }
        Ok(BrokerSession {
            id: response.session,
            pid: response.code,
            master,
        })
    }

    fn close(&mut self, session: u64) -> io::Result<()> {
        let request = self.request_id();
        let mut frame = Frame::new(CLOSE);
        frame.request = request;
        frame.session = session;
        send_frame(self.control.as_raw_fd(), &frame)?;
        let (response, passed) = self.receive_for(request, CLOSE_RESULT)?;
        drop(passed);
        close_status(response.aux)
    }

    fn release(&mut self, session: u64) -> io::Result<()> {
        let request = self.request_id();
        let mut frame = Frame::new(RELEASE);
        frame.request = request;
        frame.session = session;
        send_frame(self.control.as_raw_fd(), &frame)?;
        let (response, passed) = self.receive_for(request, RELEASE_RESULT)?;
        drop(passed);
        if response.aux == 1 {
            self.exits.remove(&session);
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "broker session is not releasable",
            ))
        }
    }

    fn abort(&mut self, session: u64) -> io::Result<()> {
        match self.close(session) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        if self.exits.remove(&session).is_none() {
            loop {
                let (frame, passed) = self.receive()?;
                drop(passed);
                if frame.kind == EXIT && frame.session == session {
                    break;
                }
                if frame.kind == EXIT {
                    self.record_exit(&frame);
                }
            }
        }
        self.release(session)
    }

    fn signal(&mut self, session: u64, signal: i32) -> io::Result<bool> {
        let request = self.request_id();
        let mut frame = Frame::new(SIGNAL);
        frame.request = request;
        frame.session = session;
        frame.code = signal;
        send_frame(self.control.as_raw_fd(), &frame)?;
        let (response, passed) = self.receive_for(request, SIGNAL_RESULT)?;
        drop(passed);
        Ok(response.aux == 1)
    }

    fn shutdown(&mut self) -> bool {
        let request = self.request_id();
        let mut frame = Frame::new(SHUTDOWN);
        frame.request = request;
        let clean = send_frame(self.control.as_raw_fd(), &frame).is_ok()
            && self.receive_for(request, SHUTDOWN_RESULT).is_ok();
        if !clean {
            terminate_process(self.broker_pid);
        }
        reap_process(self.broker_pid)
    }
}

fn close_status(status: u32) -> io::Result<()> {
    match status {
        CLOSE_KILLED | CLOSE_ALREADY_EXITED => Ok(()),
        CLOSE_STALE => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "broker session is stale",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker returned an unknown close result",
        )),
    }
}

fn run_worker(
    control: OwnedFd,
    broker_pid: libc::pid_t,
    wake: OwnedFd,
    worker_wake: OwnedFd,
    requests: Receiver<Request>,
    reactor_commands: SyncSender<Command>,
    reactor_wake: OwnedFd,
) -> bool {
    let mut worker = Worker {
        control,
        broker_pid,
        next_request: 1,
        reactor_commands,
        reactor_wake,
        exits: HashMap::new(),
    };
    loop {
        let mut poll = [
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: worker.control.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as _, -1) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if poll[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match worker.receive() {
                Ok((frame, passed)) => {
                    drop(passed);
                    if frame.kind == EXIT {
                        worker.record_exit(&frame);
                    }
                }
                Err(_) => break,
            }
        }
        if poll[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }
        if poll[0].revents & libc::POLLIN != 0 {
            drain(wake.as_raw_fd());
            let mut processed = 0;
            for _ in 0..REQUEST_QUANTUM {
                let Ok(request) = requests.try_recv() else {
                    break;
                };
                processed += 1;
                match request {
                    Request::Spawn { config, reply } => {
                        let _ = reply.send(worker.spawn(config));
                    }
                    Request::CloseDetached { session, handle } => {
                        let succeeded = worker.close(session).is_ok();
                        let _ = worker
                            .reactor_commands
                            .send(Command::CloseResult { handle, succeeded });
                        let _ = wake_socket(worker.reactor_wake.as_raw_fd());
                    }
                    Request::GracefulSignal { session, handle } => {
                        let result = worker.signal(session, libc::SIGTERM).ok();
                        let _ = worker
                            .reactor_commands
                            .send(Command::GracefulSignalResult { handle, result });
                        let _ = wake_socket(worker.reactor_wake.as_raw_fd());
                    }
                    Request::ForceClose { session, handle } => {
                        let succeeded = worker.close(session).is_ok();
                        let _ = worker
                            .reactor_commands
                            .send(Command::ForceCloseResult { handle, succeeded });
                        let _ = wake_socket(worker.reactor_wake.as_raw_fd());
                    }
                    Request::Release { session, handle } => {
                        let succeeded =
                            worker.release(session).is_ok() || worker.abort(session).is_ok();
                        let _ = worker
                            .reactor_commands
                            .send(Command::ReleaseResult { handle, succeeded });
                        let _ = wake_socket(worker.reactor_wake.as_raw_fd());
                    }
                    Request::Abort { session, reply } => {
                        let _ = reply.send(worker.abort(session));
                    }
                    Request::AbortDetached { session } => {
                        let _ = worker.abort(session);
                    }
                    Request::Signal {
                        session,
                        signal,
                        reply,
                    } => {
                        let result = worker
                            .signal(session, signal)
                            .map_err(|error| OperationError::from_io(Operation::Terminate, &error));
                        let _ = reply.send(result);
                    }
                    Request::Shutdown => {
                        return worker.shutdown();
                    }
                }
            }
            if processed == REQUEST_QUANTUM {
                let _ = wake_socket(worker_wake.as_raw_fd());
            }
            if processed != 0 {
                // A lifecycle request rejected while this bounded queue was
                // full is retried by the reactor after the worker makes room.
                let _ = wake_socket(worker.reactor_wake.as_raw_fd());
            }
        }
    }
    terminate_process(worker.broker_pid);
    let reaped = reap_process(worker.broker_pid);
    if worker.reactor_commands.send(Command::BrokerLost).is_ok() {
        let _ = wake_socket(worker.reactor_wake.as_raw_fd());
    }
    reaped
}

fn terminate_process(pid: libc::pid_t) {
    loop {
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            return;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return;
    }
}

fn reap_process(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    loop {
        let result = unsafe { libc::waitpid(pid, ptr::null_mut(), libc::WNOHANG) };
        if result == pid {
            return true;
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return error.raw_os_error() == Some(libc::ECHILD);
        }
        if Instant::now() >= deadline {
            // Preserve eventual exact reaping without allowing runtime
            // shutdown or a guard destructor to block indefinitely.
            queue_reap(pid);
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn reaper_sender() -> Option<&'static SyncSender<libc::pid_t>> {
    static REAPER: OnceLock<Option<SyncSender<libc::pid_t>>> = OnceLock::new();
    REAPER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(REAPER_CAPACITY);
            let thread = thread::Builder::new()
                .name("ptyx-broker-reaper".to_owned())
                .spawn(move || {
                    let mut pids = Vec::with_capacity(REAPER_CAPACITY);
                    loop {
                        if pids.len() < REAPER_CAPACITY {
                            match receiver.recv_timeout(Duration::from_millis(5)) {
                                Ok(pid) => pids.push(pid),
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) if pids.is_empty() => {
                                    return;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => {}
                            }
                        } else {
                            thread::sleep(Duration::from_millis(5));
                        }
                        pids.retain(|pid| {
                            let result =
                                unsafe { libc::waitpid(*pid, ptr::null_mut(), libc::WNOHANG) };
                            if result == *pid {
                                return false;
                            }
                            if result == 0 {
                                return true;
                            }
                            let error = io::Error::last_os_error();
                            if error.kind() == io::ErrorKind::Interrupted {
                                return true;
                            }
                            if error.raw_os_error() != Some(libc::ECHILD) {
                                REAPER_HEALTHY.store(false, Ordering::Release);
                            }
                            false
                        });
                    }
                });
            match thread {
                Ok(_) => Some(sender),
                Err(_) => {
                    REAPER_HEALTHY.store(false, Ordering::Release);
                    None
                }
            }
        })
        .as_ref()
}

fn queue_reap(pid: libc::pid_t) {
    let queued = reaper_sender().is_some_and(|reaper| reaper.try_send(pid).is_ok());
    if !queued {
        REAPER_HEALTHY.store(false, Ordering::Release);
    }
}

fn drain(fd: RawFd) {
    let mut bytes = [0_u8; 128];
    loop {
        if unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) } <= 0 {
            return;
        }
    }
}

unsafe extern "C" {
    static mut environ: *mut *mut libc::c_char;
}

#[cfg(test)]
mod tests {
    use super::{close_status, CLOSE_ALREADY_EXITED, CLOSE_KILLED, CLOSE_STALE};
    use std::io::ErrorKind;

    #[test]
    fn close_status_accepts_killed_and_already_exited() {
        assert!(close_status(CLOSE_KILLED).is_ok());
        assert!(close_status(CLOSE_ALREADY_EXITED).is_ok());
    }

    #[test]
    fn close_status_preserves_stale_and_unknown_results() {
        assert_eq!(
            close_status(CLOSE_STALE).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(close_status(99).unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
