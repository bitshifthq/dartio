use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString};
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const MAGIC: u32 = 0x4258_5450;
const VERSION: u16 = 1;
const HEADER: usize = 32;
const MAX_PAYLOAD: usize = 64 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const SPAWN_V2: u32 = 0x5854_5950;
const CONTROL_FD: RawFd = 3;
#[cfg(target_os = "macos")]
const POSIX_SPAWN_CLOEXEC_DEFAULT: libc::c_int = 0x4000;

const HELLO: u16 = 1;
const SPAWN: u16 = 2;
const SPAWN_OK: u16 = 3;
const ERROR: u16 = 4;
const CLOSE: u16 = 5;
const CLOSE_RESULT: u16 = 6;
const EXIT: u16 = 7;
const RELEASE: u16 = 8;
const RELEASE_RESULT: u16 = 9;
const STATS: u16 = 10;
const STATS_RESULT: u16 = 11;
const SHUTDOWN: u16 = 12;
const SHUTDOWN_RESULT: u16 = 13;
const SIGNAL: u16 = 14;
const SIGNAL_RESULT: u16 = 15;

const ERROR_PROTOCOL: u32 = 1;
const ERROR_SPAWN: u32 = 2;
const ERROR_POST_EXEC: u32 = 3;

const CLOSE_KILLED: u32 = 1;
const CLOSE_ALREADY_EXITED: u32 = 2;
const CLOSE_STALE: u32 = 3;
const MAX_SIGNAL: i32 = 31;

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
                "protocol payload exceeds bound",
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
                "short protocol frame",
            ));
        }
        let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        let version = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        let length = u32::from_ne_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if magic != MAGIC || version != VERSION || length > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "protocol header rejected",
            ));
        }
        if bytes.len() != HEADER + length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "protocol frame length mismatch",
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

fn send_frame(fd: RawFd, frame: &Frame, passed_fd: Option<RawFd>) -> io::Result<()> {
    let bytes = frame.encode()?;
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let mut offset = 0;
    let mut descriptor_pending = passed_fd;
    while offset < bytes.len() {
        let sent = if let Some(passed_fd) = descriptor_pending {
            let mut iovec = libc::iovec {
                iov_base: bytes[offset..].as_ptr().cast_mut().cast(),
                iov_len: bytes.len() - offset,
            };
            let mut control = [0_usize; 8];
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iovec;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as _) } as _;
            let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
            if header.is_null() {
                return Err(io::Error::other("missing ancillary header"));
            }
            unsafe {
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as _) as _;
                ptr::copy_nonoverlapping(
                    (&passed_fd as *const RawFd).cast::<u8>(),
                    libc::CMSG_DATA(header),
                    size_of::<RawFd>(),
                );
                libc::sendmsg(fd, &message, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL)
            }
        } else {
            unsafe {
                libc::send(
                    fd,
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            }
        };
        if sent > 0 {
            offset += sent as usize;
            descriptor_pending = None;
            continue;
        }
        if sent < 0 {
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
            "control write returned zero",
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
        message.msg_controllen = size_of_val(&control) as _;
        let received = unsafe { libc::recvmsg(fd, &mut message, libc::MSG_DONTWAIT) };
        if received == 0 {
            return if offset == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial protocol frame",
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
                    "protocol payload exceeds bound",
                ));
            }
            bytes.resize(HEADER + length, 0);
        }
    }
    if received_fds.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol sent multiple file descriptors",
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
                "unexpected protocol ancillary data",
            ));
        }
        let data_length = (unsafe { (*header).cmsg_len } as usize)
            .saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
        if data_length == 0 || !data_length.is_multiple_of(size_of::<RawFd>()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid protocol descriptor payload",
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
                let owned = unsafe { OwnedFd::from_raw_fd(raw) };
                set_cloexec(owned.as_raw_fd())?;
                received_fds.push(owned);
            }
        }
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    if message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated protocol frame or ancillary data",
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
                "protocol frame deadline exceeded",
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
                    "protocol control socket failed",
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

fn size_of_val<T>(value: &T) -> usize {
    std::mem::size_of_val(value)
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

struct SpawnAttrs(libc::posix_spawnattr_t);

impl Drop for SpawnAttrs {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawnattr_destroy(&mut self.0);
        }
    }
}

struct FileActions(libc::posix_spawn_file_actions_t);

impl Drop for FileActions {
    fn drop(&mut self) {
        unsafe {
            libc::posix_spawn_file_actions_destroy(&mut self.0);
        }
    }
}

fn spawn_code(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

fn launch_broker_at(path: &CStr) -> io::Result<Client> {
    let (controller, broker) = socket_pair()?;
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
    #[cfg(target_os = "macos")]
    let flags = POSIX_SPAWN_CLOEXEC_DEFAULT | libc::POSIX_SPAWN_SETSIGMASK;
    #[cfg(target_os = "linux")]
    let flags = libc::POSIX_SPAWN_SETSIGMASK;
    let flags = flags as libc::c_short;
    spawn_code(unsafe { libc::posix_spawnattr_setflags(&mut attrs.0, flags) })?;
    spawn_code(unsafe {
        libc::posix_spawn_file_actions_adddup2(&mut actions.0, broker.as_raw_fd(), CONTROL_FD)
    })?;
    if broker.as_raw_fd() != CONTROL_FD {
        spawn_code(unsafe {
            libc::posix_spawn_file_actions_addclose(&mut actions.0, broker.as_raw_fd())
        })?;
    }

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
    drop(broker);
    let mut client = Client {
        control: controller,
        broker_pid: pid,
        next_request: 1,
        exits: HashMap::new(),
        sessions: HashMap::new(),
    };
    let (hello, passed) = client.receive()?;
    if hello.kind != HELLO
        || hello.aux != VERSION as u32
        || hello.payload != hello_payload()
        || passed.is_some()
    {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, ptr::null_mut(), 0);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker handshake rejected",
        ));
    }
    Ok(client)
}

fn launch_broker() -> io::Result<Client> {
    let executable = CString::new(std::env::current_exe()?.as_os_str().as_encoded_bytes()).unwrap();
    launch_broker_at(&executable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFailure {
    SessionClosing,
    BrokerLost,
}

#[derive(Debug)]
struct AcceptedInput {
    sequence: u64,
    bytes: Vec<u8>,
}

struct ClientSession {
    id: u64,
    pid: libc::pid_t,
    master: OwnedFd,
    next_sequence: u64,
    queued: VecDeque<AcceptedInput>,
    failed: HashMap<u64, InputFailure>,
}

impl ClientSession {
    fn accept(&mut self, bytes: &[u8]) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.queued.push_back(AcceptedInput {
            sequence,
            bytes: bytes.to_vec(),
        });
        sequence
    }

    fn fail_accepted(&mut self, failure: InputFailure) {
        while let Some(input) = self.queued.pop_front() {
            let _owned_bytes = input.bytes;
            self.failed.insert(input.sequence, failure);
        }
    }

    fn flush_result(&self, sequence: u64) -> Result<(), InputFailure> {
        match self.failed.get(&sequence) {
            Some(failure) => Err(*failure),
            None if self.queued.iter().any(|input| input.sequence == sequence) => {
                panic!("flush is still pending")
            }
            None => Ok(()),
        }
    }
}

struct Client {
    control: OwnedFd,
    broker_pid: libc::pid_t,
    next_request: u32,
    exits: HashMap<u64, i32>,
    sessions: HashMap<u64, (libc::pid_t, RawFd)>,
}

impl Client {
    fn request_id(&mut self) -> u32 {
        let result = self.next_request;
        self.next_request += 1;
        result
    }

    fn receive(&mut self) -> io::Result<(Frame, Option<OwnedFd>)> {
        receive_frame(self.control.as_raw_fd())?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "broker control socket closed")
        })
    }

    fn receive_for(&mut self, request: u32, kind: u16) -> io::Result<(Frame, Option<OwnedFd>)> {
        loop {
            let (frame, fd) = self.receive()?;
            if frame.kind == EXIT {
                self.exits.insert(frame.session, frame.code);
                continue;
            }
            if frame.request == request && frame.kind == kind {
                return Ok((frame, fd));
            }
            if frame.request == request && frame.kind == ERROR {
                return Err(io::Error::other(format!(
                    "broker error category={} code={}",
                    frame.aux, frame.code
                )));
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected broker response",
            ));
        }
    }

    fn spawn(
        &mut self,
        argv: &[CString],
        inject_post_exec_failure: bool,
    ) -> io::Result<ClientSession> {
        let request = self.request_id();
        let mut payload = Vec::new();
        payload.extend_from_slice(&(inject_post_exec_failure as u32).to_ne_bytes());
        payload.extend_from_slice(&(argv.len() as u32).to_ne_bytes());
        for argument in argv {
            let bytes = argument.as_bytes();
            payload.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
            payload.extend_from_slice(bytes);
        }
        let mut frame = Frame::new(SPAWN);
        frame.request = request;
        frame.payload = payload;
        send_frame(self.control.as_raw_fd(), &frame, None)?;
        let (response, master) = self.receive_for(request, SPAWN_OK)?;
        let master = master.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "spawn response omitted PTY master",
            )
        })?;
        set_nonblocking(master.as_raw_fd())?;
        self.sessions
            .insert(response.session, (response.code, master.as_raw_fd()));
        Ok(ClientSession {
            id: response.session,
            pid: response.code,
            master,
            next_sequence: 1,
            queued: VecDeque::new(),
            failed: HashMap::new(),
        })
    }

    fn expect_spawn_failure(
        &mut self,
        argv: &[CString],
        inject_post_exec_failure: bool,
    ) -> io::Result<Frame> {
        let request = self.request_id();
        let mut payload = Vec::new();
        payload.extend_from_slice(&(inject_post_exec_failure as u32).to_ne_bytes());
        payload.extend_from_slice(&(argv.len() as u32).to_ne_bytes());
        for argument in argv {
            payload.extend_from_slice(&(argument.as_bytes().len() as u32).to_ne_bytes());
            payload.extend_from_slice(argument.as_bytes());
        }
        let mut frame = Frame::new(SPAWN);
        frame.request = request;
        frame.payload = payload;
        send_frame(self.control.as_raw_fd(), &frame, None)?;
        loop {
            let (response, passed) = self.receive()?;
            drop(passed);
            if response.kind == EXIT {
                self.exits.insert(response.session, response.code);
                continue;
            }
            if response.request == request && response.kind == ERROR {
                return Ok(response);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected spawn error",
            ));
        }
    }

    fn close(&mut self, session: &mut ClientSession) -> io::Result<u32> {
        session.fail_accepted(InputFailure::SessionClosing);
        let request = self.request_id();
        let mut frame = Frame::new(CLOSE);
        frame.request = request;
        frame.session = session.id;
        send_frame(self.control.as_raw_fd(), &frame, None)?;
        let (response, passed) = self.receive_for(request, CLOSE_RESULT)?;
        drop(passed);
        Ok(response.aux)
    }

    fn wait_exit(&mut self, session: u64, timeout: Duration) -> io::Result<i32> {
        if let Some(status) = self.exits.remove(&session) {
            return Ok(status);
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut descriptor = libc::pollfd {
                fd: self.control.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                break;
            }
            let (frame, passed) = self.receive()?;
            drop(passed);
            if frame.kind == EXIT {
                if frame.session == session {
                    return Ok(frame.code);
                }
                self.exits.insert(frame.session, frame.code);
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "exit timeout"))
    }

    fn release(&mut self, session: u64) -> io::Result<bool> {
        let request = self.request_id();
        let mut frame = Frame::new(RELEASE);
        frame.request = request;
        frame.session = session;
        send_frame(self.control.as_raw_fd(), &frame, None)?;
        let (response, passed) = self.receive_for(request, RELEASE_RESULT)?;
        drop(passed);
        self.sessions.remove(&session);
        Ok(response.aux == 1)
    }

    fn stats(&mut self) -> io::Result<(u32, u32, u32)> {
        let request = self.request_id();
        let mut frame = Frame::new(STATS);
        frame.request = request;
        send_frame(self.control.as_raw_fd(), &frame, None)?;
        let (response, passed) = self.receive_for(request, STATS_RESULT)?;
        drop(passed);
        if response.payload.len() != 12 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad stats"));
        }
        Ok((
            u32::from_ne_bytes(response.payload[0..4].try_into().unwrap()),
            u32::from_ne_bytes(response.payload[4..8].try_into().unwrap()),
            u32::from_ne_bytes(response.payload[8..12].try_into().unwrap()),
        ))
    }

    fn recover_after_broker_loss(&mut self, sessions: &mut [&mut ClientSession]) {
        for session in sessions {
            session.fail_accepted(InputFailure::BrokerLost);
            unsafe {
                libc::kill(-session.pid, libc::SIGKILL);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ExitStatus {
    code: i32,
}

enum SlotState {
    Vacant,
    Running {
        pid: libc::pid_t,
    },
    Exited {
        exit: ExitStatus,
        process_group: libc::pid_t,
    },
}

struct Slot {
    generation: u32,
    state: SlotState,
}

struct Broker {
    control: OwnedFd,
    #[cfg(target_os = "macos")]
    kqueue: OwnedFd,
    #[cfg(target_os = "linux")]
    signal_fd: OwnedFd,
    slots: Vec<Slot>,
    signal_after_reap: u32,
    injected_cleanups: u32,
}

impl Broker {
    fn run() -> io::Result<()> {
        let control = unsafe { OwnedFd::from_raw_fd(CONTROL_FD) };
        set_cloexec(control.as_raw_fd())?;
        #[cfg(target_os = "macos")]
        let kqueue = unsafe { libc::kqueue() };
        #[cfg(target_os = "macos")]
        if kqueue < 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "macos")]
        let kqueue = unsafe { OwnedFd::from_raw_fd(kqueue) };
        #[cfg(target_os = "macos")]
        set_cloexec(kqueue.as_raw_fd())?;
        #[cfg(target_os = "macos")]
        let change = libc::kevent {
            ident: control.as_raw_fd() as usize,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: 0,
            data: 0,
            udata: ptr::null_mut(),
        };
        #[cfg(target_os = "macos")]
        if unsafe {
            libc::kevent(
                kqueue.as_raw_fd(),
                &change,
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        let signal_fd = create_sigchld_fd()?;
        let mut broker = Self {
            control,
            #[cfg(target_os = "macos")]
            kqueue,
            #[cfg(target_os = "linux")]
            signal_fd,
            slots: Vec::new(),
            signal_after_reap: 0,
            injected_cleanups: 0,
        };
        let mut hello = Frame::new(HELLO);
        hello.aux = VERSION as u32;
        hello.payload = hello_payload();
        send_frame(broker.control.as_raw_fd(), &hello, None)?;
        let result = broker.event_loop();
        broker.cleanup_all();
        result
    }

    #[cfg(target_os = "macos")]
    fn event_loop(&mut self) -> io::Result<()> {
        loop {
            let mut events: [MaybeUninit<libc::kevent>; 32] =
                unsafe { MaybeUninit::uninit().assume_init() };
            let count = unsafe {
                libc::kevent(
                    self.kqueue.as_raw_fd(),
                    ptr::null(),
                    0,
                    events.as_mut_ptr().cast(),
                    events.len() as i32,
                    ptr::null(),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            for event in &events[..count as usize] {
                let event = unsafe { event.assume_init() };
                if event.filter == libc::EVFILT_READ
                    && event.ident == self.control.as_raw_fd() as usize
                {
                    match receive_frame(self.control.as_raw_fd())? {
                        Some((frame, passed)) => {
                            drop(passed);
                            if !self.handle_request(frame)? {
                                return Ok(());
                            }
                        }
                        None => return Ok(()),
                    }
                } else if event.filter == libc::EVFILT_PROC {
                    self.reap_event(event.udata as usize as u64)?;
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn event_loop(&mut self) -> io::Result<()> {
        loop {
            let mut descriptors = [
                libc::pollfd {
                    fd: self.control.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.signal_fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptors[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            if descriptors[0].revents & libc::POLLIN != 0 {
                match receive_frame(self.control.as_raw_fd())? {
                    Some((frame, passed)) => {
                        drop(passed);
                        if !self.handle_request(frame)? {
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                }
            }
            if descriptors[1].revents & libc::POLLIN != 0 {
                drain_sigchld(self.signal_fd.as_raw_fd())?;
                self.reap_available()?;
            }
        }
    }

    fn handle_request(&mut self, frame: Frame) -> io::Result<bool> {
        match frame.kind {
            SPAWN => self.handle_spawn(frame)?,
            CLOSE => self.handle_close(frame)?,
            RELEASE => self.handle_release(frame)?,
            SIGNAL => self.handle_signal(frame)?,
            STATS => self.handle_stats(frame)?,
            SHUTDOWN => {
                self.cleanup_all();
                let mut response = Frame::new(SHUTDOWN_RESULT);
                response.request = frame.request;
                response.aux = self.running_jobs() as u32;
                send_frame(self.control.as_raw_fd(), &response, None)?;
                return Ok(false);
            }
            _ => {
                let mut response = Frame::new(ERROR);
                response.request = frame.request;
                response.aux = ERROR_PROTOCOL;
                response.code = libc::EINVAL;
                send_frame(self.control.as_raw_fd(), &response, None)?;
            }
        }
        Ok(true)
    }

    fn handle_spawn(&mut self, frame: Frame) -> io::Result<()> {
        let request = match decode_spawn(&frame.payload) {
            Ok(value) => value,
            Err(error) => {
                let mut response = Frame::new(ERROR);
                response.request = frame.request;
                response.aux = ERROR_PROTOCOL;
                response.code = error.raw_os_error().unwrap_or(libc::EINVAL);
                return send_frame(self.control.as_raw_fd(), &response, None);
            }
        };
        let spawned = match spawn_target(&request) {
            Ok(value) => value,
            Err(error) => {
                let mut response = Frame::new(ERROR);
                response.request = frame.request;
                response.aux = ERROR_SPAWN;
                response.code = error.raw_os_error().unwrap_or(libc::EIO);
                return send_frame(self.control.as_raw_fd(), &response, None);
            }
        };
        if request.inject {
            kill_and_reap(spawned.pid);
            self.injected_cleanups += 1;
            let mut response = Frame::new(ERROR);
            response.request = frame.request;
            response.aux = ERROR_POST_EXEC;
            response.code = libc::ECANCELED;
            return send_frame(self.control.as_raw_fd(), &response, None);
        }

        let session = self.allocate_slot(spawned.pid);
        if let Err(error) = self.register_process(spawned.pid, session) {
            self.vacate(session);
            kill_and_reap(spawned.pid);
            let mut response = Frame::new(ERROR);
            response.request = frame.request;
            response.aux = ERROR_POST_EXEC;
            response.code = error.raw_os_error().unwrap_or(libc::EIO);
            return send_frame(self.control.as_raw_fd(), &response, None);
        }
        let mut response = Frame::new(SPAWN_OK);
        response.request = frame.request;
        response.session = session;
        response.code = spawned.pid;
        if let Err(error) = send_frame(
            self.control.as_raw_fd(),
            &response,
            Some(spawned.master.as_raw_fd()),
        ) {
            self.kill_running(session);
            self.reap_blocking(session);
            return Err(error);
        }
        Ok(())
    }

    fn handle_close(&mut self, frame: Frame) -> io::Result<()> {
        let (result, status) = match self.lookup(frame.session) {
            Some(SlotState::Running { pid }) => {
                let pid = *pid;
                let signal = unsafe { libc::kill(-pid, libc::SIGKILL) };
                if signal < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    let mut response = Frame::new(ERROR);
                    response.request = frame.request;
                    response.aux = ERROR_SPAWN;
                    response.code = io::Error::last_os_error()
                        .raw_os_error()
                        .unwrap_or(libc::EIO);
                    return send_frame(self.control.as_raw_fd(), &response, None);
                }
                (CLOSE_KILLED, 0)
            }
            Some(SlotState::Exited {
                exit,
                process_group,
            }) => {
                unsafe {
                    libc::kill(-*process_group, libc::SIGKILL);
                }
                (CLOSE_ALREADY_EXITED, exit.code)
            }
            _ => (CLOSE_STALE, 0),
        };
        let mut response = Frame::new(CLOSE_RESULT);
        response.request = frame.request;
        response.session = frame.session;
        response.code = status;
        response.aux = result;
        send_frame(self.control.as_raw_fd(), &response, None)
    }

    fn handle_signal(&mut self, frame: Frame) -> io::Result<()> {
        let signal = frame.code;
        if signal <= 0 || signal > MAX_SIGNAL {
            let mut response = Frame::new(ERROR);
            response.request = frame.request;
            response.aux = ERROR_PROTOCOL;
            response.code = libc::EINVAL;
            return send_frame(self.control.as_raw_fd(), &response, None);
        }
        let delivered = match self.lookup(frame.session) {
            Some(SlotState::Running { pid }) => {
                let result = unsafe { libc::kill(-*pid, signal) };
                if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    let mut response = Frame::new(ERROR);
                    response.request = frame.request;
                    response.aux = ERROR_SPAWN;
                    response.code = io::Error::last_os_error()
                        .raw_os_error()
                        .unwrap_or(libc::EIO);
                    return send_frame(self.control.as_raw_fd(), &response, None);
                }
                result == 0
            }
            Some(SlotState::Exited { .. }) | Some(SlotState::Vacant) | None => false,
        };
        let mut response = Frame::new(SIGNAL_RESULT);
        response.request = frame.request;
        response.aux = u32::from(delivered);
        send_frame(self.control.as_raw_fd(), &response, None)
    }

    fn handle_release(&mut self, frame: Frame) -> io::Result<()> {
        let released = if let Some((index, generation)) = split_session(frame.session) {
            if let Some(slot) = self.slots.get_mut(index) {
                if slot.generation == generation {
                    if let SlotState::Exited { process_group, .. } = &slot.state {
                        unsafe {
                            libc::kill(-*process_group, libc::SIGKILL);
                        }
                        slot.state = SlotState::Vacant;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        let mut response = Frame::new(RELEASE_RESULT);
        response.request = frame.request;
        response.session = frame.session;
        response.aux = released as u32;
        send_frame(self.control.as_raw_fd(), &response, None)
    }

    fn handle_stats(&self, frame: Frame) -> io::Result<()> {
        let mut response = Frame::new(STATS_RESULT);
        response.request = frame.request;
        response
            .payload
            .extend_from_slice(&(self.running_jobs() as u32).to_ne_bytes());
        response
            .payload
            .extend_from_slice(&(count_open_fds() as u32).to_ne_bytes());
        response
            .payload
            .extend_from_slice(&self.signal_after_reap.to_ne_bytes());
        send_frame(self.control.as_raw_fd(), &response, None)
    }

    fn allocate_slot(&mut self, pid: libc::pid_t) -> u64 {
        let index = self
            .slots
            .iter()
            .position(|slot| matches!(slot.state, SlotState::Vacant))
            .unwrap_or_else(|| {
                self.slots.push(Slot {
                    generation: 0,
                    state: SlotState::Vacant,
                });
                self.slots.len() - 1
            });
        let slot = &mut self.slots[index];
        slot.generation = slot.generation.checked_add(1).unwrap_or(1);
        slot.state = SlotState::Running { pid };
        make_session(index, slot.generation)
    }

    fn lookup(&self, session: u64) -> Option<&SlotState> {
        let (index, generation) = split_session(session)?;
        let slot = self.slots.get(index)?;
        (slot.generation == generation).then_some(&slot.state)
    }

    fn vacate(&mut self, session: u64) {
        if let Some((index, generation)) = split_session(session) {
            if let Some(slot) = self.slots.get_mut(index) {
                if slot.generation == generation {
                    slot.state = SlotState::Vacant;
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn register_process(&self, pid: libc::pid_t, session: u64) -> io::Result<()> {
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: session as usize as *mut libc::c_void,
        };
        if unsafe {
            libc::kevent(
                self.kqueue.as_raw_fd(),
                &change,
                1,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn register_process(&self, _pid: libc::pid_t, _session: u64) -> io::Result<()> {
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn reap_event(&mut self, session: u64) -> io::Result<()> {
        let pid = match self.lookup(session) {
            Some(SlotState::Running { pid }) => *pid,
            _ => return Ok(()),
        };
        let status = wait_exact(pid)?;
        let exit = ExitStatus {
            code: decode_wait_status(status),
        };
        if let Some((index, generation)) = split_session(session) {
            let slot = &mut self.slots[index];
            if slot.generation == generation {
                slot.state = SlotState::Exited {
                    exit,
                    process_group: pid,
                };
            }
        }
        let mut frame = Frame::new(EXIT);
        frame.session = session;
        frame.code = exit.code;
        send_frame(self.control.as_raw_fd(), &frame, None)
    }

    #[cfg(target_os = "linux")]
    fn reap_available(&mut self) -> io::Result<()> {
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid == 0 {
                return Ok(());
            }
            if pid < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Ok(());
                }
                return Err(error);
            }
            let Some((index, generation)) =
                self.slots.iter().enumerate().find_map(|(index, slot)| {
                    matches!(slot.state, SlotState::Running { pid: child } if child == pid)
                        .then_some((index, slot.generation))
                })
            else {
                continue;
            };
            let session = make_session(index, generation);
            let exit = ExitStatus {
                code: decode_wait_status(status),
            };
            self.slots[index].state = SlotState::Exited {
                exit,
                process_group: pid,
            };
            let mut frame = Frame::new(EXIT);
            frame.session = session;
            frame.code = exit.code;
            send_frame(self.control.as_raw_fd(), &frame, None)?;
        }
    }

    fn running_jobs(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot.state, SlotState::Running { .. }))
            .count()
    }

    fn kill_running(&mut self, session: u64) {
        if let Some(SlotState::Running { pid }) = self.lookup(session) {
            unsafe {
                libc::kill(-*pid, libc::SIGKILL);
            }
        } else if matches!(self.lookup(session), Some(SlotState::Exited { .. })) {
            self.signal_after_reap += 1;
        }
    }

    fn reap_blocking(&mut self, session: u64) {
        let pid = match self.lookup(session) {
            Some(SlotState::Running { pid }) => *pid,
            _ => return,
        };
        if let Ok(status) = wait_exact(pid) {
            if let Some((index, generation)) = split_session(session) {
                let slot = &mut self.slots[index];
                if slot.generation == generation {
                    slot.state = SlotState::Exited {
                        exit: ExitStatus {
                            code: decode_wait_status(status),
                        },
                        process_group: pid,
                    };
                }
            }
        }
    }

    fn cleanup_all(&mut self) {
        let sessions: Vec<u64> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                (!matches!(slot.state, SlotState::Vacant))
                    .then_some(make_session(index, slot.generation))
            })
            .collect();
        for session in &sessions {
            match self.lookup(*session) {
                Some(SlotState::Running { pid }) => unsafe {
                    libc::kill(-*pid, libc::SIGKILL);
                },
                Some(SlotState::Exited { process_group, .. }) => unsafe {
                    libc::kill(-*process_group, libc::SIGKILL);
                },
                _ => {}
            }
        }
        for session in sessions {
            self.reap_blocking(session);
        }
    }
}

struct Spawned {
    pid: libc::pid_t,
    master: OwnedFd,
}

struct SpawnRequest {
    inject: bool,
    argv: Vec<CString>,
    environment: Option<Vec<CString>>,
    cwd: Option<CString>,
    size: libc::winsize,
}

fn allocate_pty(mut size: libc::winsize) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master = -1;
    let mut slave = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut size,
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
    termios.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG;
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

fn spawn_target(request: &SpawnRequest) -> io::Result<Spawned> {
    if request.argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }
    let (master, slave) = allocate_pty(request.size)?;
    let mut pipe_fds = [-1; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let error_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    let error_write = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
    set_cloexec(error_read.as_raw_fd())?;
    set_cloexec(error_write.as_raw_fd())?;
    let pointers: Vec<*const libc::c_char> = request
        .argv
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect();
    let environment: Option<Vec<*const libc::c_char>> =
        request.environment.as_ref().map(|entries| {
            entries
                .iter()
                .map(|value| value.as_ptr())
                .chain(std::iter::once(ptr::null()))
                .collect()
        });
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            libc::close(error_read.as_raw_fd());
            exec_target(
                request.argv[0].as_c_str(),
                &pointers,
                environment.as_deref(),
                request.cwd.as_deref(),
                master.as_raw_fd(),
                slave.as_raw_fd(),
                error_write.as_raw_fd(),
            );
        }
    }
    drop(error_write);
    drop(slave);
    match read_exec_result(error_read.as_raw_fd()) {
        Ok(None) => Ok(Spawned { pid, master }),
        Ok(Some(code)) => {
            let _ = wait_exact(pid);
            Err(io::Error::from_raw_os_error(code))
        }
        Err(error) => {
            kill_and_reap(pid);
            Err(error)
        }
    }
}

unsafe fn exec_target(
    executable: &CStr,
    argv: &[*const libc::c_char],
    environment: Option<&[*const libc::c_char]>,
    cwd: Option<&CStr>,
    master: RawFd,
    slave: RawFd,
    error_fd: RawFd,
) -> ! {
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    if libc::sigemptyset(empty.as_mut_ptr()) < 0 {
        child_fail(error_fd);
    }
    if libc::sigprocmask(libc::SIG_SETMASK, empty.as_ptr(), ptr::null_mut()) < 0 {
        child_fail(error_fd);
    }
    for signal in [
        libc::SIGCHLD,
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGALRM,
        libc::SIGPIPE,
    ] {
        if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
            child_fail(error_fd);
        }
    }
    if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
        child_fail(error_fd);
    }
    if let Some(cwd) = cwd {
        if libc::chdir(cwd.as_ptr()) < 0 {
            child_fail(error_fd);
        }
    }
    if libc::dup2(slave, libc::STDIN_FILENO) < 0
        || libc::dup2(slave, libc::STDOUT_FILENO) < 0
        || libc::dup2(slave, libc::STDERR_FILENO) < 0
    {
        child_fail(error_fd);
    }
    if master > 2 && master != error_fd {
        libc::close(master);
    }
    if slave > 2 && slave != error_fd {
        libc::close(slave);
    }
    libc::execve(
        executable.as_ptr(),
        argv.as_ptr(),
        environment.map_or(environ as *const *const libc::c_char, |entries| {
            entries.as_ptr()
        }),
    );
    child_fail(error_fd)
}

unsafe fn child_fail(error_fd: RawFd) -> ! {
    let code = current_errno();
    let bytes = code.to_ne_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let wrote = libc::write(
            error_fd,
            bytes[offset..].as_ptr().cast(),
            bytes.len() - offset,
        );
        if wrote > 0 {
            offset += wrote as usize;
        } else if wrote < 0 && current_errno() == libc::EINTR {
            continue;
        } else {
            break;
        }
    }
    libc::_exit(127)
}

#[cfg(target_os = "macos")]
unsafe fn current_errno() -> libc::c_int {
    *libc::__error()
}

#[cfg(target_os = "linux")]
unsafe fn current_errno() -> libc::c_int {
    *libc::__errno_location()
}

fn read_exec_result(fd: RawFd) -> io::Result<Option<i32>> {
    let mut bytes = [0_u8; size_of::<i32>()];
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
                return Ok(Some(i32::from_ne_bytes(bytes)));
            }
            continue;
        }
        if read == 0 {
            return if offset == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial exec failure record",
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

fn decode_spawn(payload: &[u8]) -> io::Result<SpawnRequest> {
    if payload.len() < 8 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short spawn"));
    }
    let inject = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) != 0;
    let marker = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
    if marker == SPAWN_V2 {
        return decode_spawn_v2(inject, payload);
    }
    let count = marker as usize;
    if count == 0 || count > 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad argc"));
    }
    let mut offset = 8;
    let mut argv = Vec::with_capacity(count);
    for _ in 0..count {
        if payload.len().saturating_sub(offset) < 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short arg"));
        }
        let length = u32::from_ne_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if length == 0 || payload.len().saturating_sub(offset) < length {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad arg"));
        }
        argv.push(
            CString::new(&payload[offset..offset + length])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "NUL argument"))?,
        );
        offset += length;
    }
    if offset != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing spawn payload",
        ));
    }
    Ok(SpawnRequest {
        inject,
        argv,
        environment: None,
        cwd: None,
        size: libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    })
}

#[doc(hidden)]
pub fn fuzz_spawn_payload(payload: &[u8]) {
    let _ = decode_spawn(payload);
}

fn decode_spawn_v2(inject: bool, payload: &[u8]) -> io::Result<SpawnRequest> {
    if payload.len() < 36 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short v2 spawn"));
    }
    let argc = read_u32(payload, 8)? as usize;
    let environment_count = read_u32(payload, 12)?;
    let rows = read_u32(payload, 16)?;
    let columns = read_u32(payload, 20)?;
    let pixel_width = read_u32(payload, 24)?;
    let pixel_height = read_u32(payload, 28)?;
    let cwd_length = read_u32(payload, 32)? as usize;
    if argc == 0 || argc > 256 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad argc"));
    }
    if environment_count != u32::MAX && environment_count > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad environment count",
        ));
    }
    let mut offset = 36;
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        argv.push(read_string(payload, &mut offset, false)?);
    }
    let environment = if environment_count == u32::MAX {
        None
    } else {
        let mut environment = Vec::with_capacity(environment_count as usize);
        for _ in 0..environment_count {
            environment.push(read_string(payload, &mut offset, false)?);
        }
        Some(environment)
    };
    let cwd = if cwd_length == 0 {
        None
    } else {
        if payload.len().saturating_sub(offset) < cwd_length {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short cwd"));
        }
        let cwd = CString::new(&payload[offset..offset + cwd_length])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "NUL cwd"))?;
        offset += cwd_length;
        Some(cwd)
    };
    if offset != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing v2 spawn payload",
        ));
    }
    Ok(SpawnRequest {
        inject,
        argv,
        environment,
        cwd,
        size: libc::winsize {
            ws_row: rows.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "rows exceed platform limit")
            })?,
            ws_col: columns.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "columns exceed platform limit")
            })?,
            ws_xpixel: pixel_width.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pixel width exceeds platform limit",
                )
            })?,
            ws_ypixel: pixel_height.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pixel height exceeds platform limit",
                )
            })?,
        },
    })
}

fn read_u32(payload: &[u8], offset: usize) -> io::Result<u32> {
    payload
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_ne_bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short spawn field"))
}

fn read_string(payload: &[u8], offset: &mut usize, allow_empty: bool) -> io::Result<CString> {
    let length = read_u32(payload, *offset)? as usize;
    *offset += 4;
    if (!allow_empty && length == 0) || payload.len().saturating_sub(*offset) < length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad spawn string",
        ));
    }
    let value = CString::new(&payload[*offset..*offset + length])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "NUL spawn string"))?;
    *offset += length;
    Ok(value)
}

fn make_session(index: usize, generation: u32) -> u64 {
    ((generation as u64) << 32) | (index as u64 + 1)
}

fn split_session(session: u64) -> Option<(usize, u32)> {
    let low = session as u32;
    let generation = (session >> 32) as u32;
    if low == 0 || generation == 0 {
        return None;
    }
    Some((low as usize - 1, generation))
}

fn wait_exact(pid: libc::pid_t) -> io::Result<i32> {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(status);
        }
        let error = io::Error::last_os_error();
        if result < 0 && error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

#[cfg(target_os = "linux")]
fn create_sigchld_fd() -> io::Result<OwnedFd> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } < 0
        || unsafe { libc::sigaction(libc::SIGCHLD, &action, ptr::null_mut()) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(mask.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut mask = unsafe { mask.assume_init() };
    if unsafe { libc::sigaddset(&mut mask, libc::SIGCHLD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mask_result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &mask, ptr::null_mut()) };
    if mask_result != 0 {
        return Err(io::Error::from_raw_os_error(mask_result));
    }
    let fd = unsafe { libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn drain_sigchld(fd: RawFd) -> io::Result<()> {
    let mut info = MaybeUninit::<libc::signalfd_siginfo>::uninit();
    loop {
        let read = unsafe {
            libc::read(
                fd,
                info.as_mut_ptr().cast(),
                size_of::<libc::signalfd_siginfo>(),
            )
        };
        if read == size_of::<libc::signalfd_siginfo>() as isize {
            continue;
        }
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "partial SIGCHLD record",
        ));
    }
}

fn decode_wait_status(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        -libc::WTERMSIG(status)
    } else {
        i32::MIN
    }
}

fn kill_and_reap(pid: libc::pid_t) {
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = wait_exact(pid);
}

fn count_open_fds() -> usize {
    let maximum = unsafe { libc::getdtablesize() };
    (0..maximum)
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .count()
}

fn process_exists(pid: libc::pid_t) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn read_pty(fd: RawFd, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut result = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read > 0 {
            result.extend_from_slice(&buffer[..read as usize]);
            continue;
        }
        if read == 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EIO) {
            return Ok(result);
        }
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "PTY read timeout"));
        }
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}

fn self_argv(arguments: &[&str]) -> Vec<CString> {
    let executable = CString::new(
        std::env::current_exe()
            .unwrap()
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    std::iter::once(executable)
        .chain(
            arguments
                .iter()
                .map(|argument| CString::new(*argument).unwrap()),
        )
        .collect()
}

fn child_inspect() {
    let pid = unsafe { libc::getpid() };
    let sid = unsafe { libc::getsid(0) };
    let pgrp = unsafe { libc::getpgrp() };
    let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
    let foreground_error = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let tty = CString::new("/dev/tty").unwrap();
    let tty_fd = unsafe { libc::open(tty.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    let tty_error = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let controlling = foreground == pgrp;
    if tty_fd >= 0 {
        unsafe {
            libc::close(tty_fd);
        }
    }
    let extra = (3..unsafe { libc::getdtablesize() })
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .count();
    print!(
        "controlling={controlling} extra_fds={extra} pid={pid} sid={sid} pgrp={pgrp} \
         foreground={foreground} foreground_errno={foreground_error} tty_fd={tty_fd} \
         tty_errno={tty_error}"
    );
}

fn child_hold() {
    loop {
        unsafe {
            libc::pause();
        }
    }
}

fn child_exit() {}

fn wait_until_gone(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    false
}

static HOST_SIGCHLD: AtomicUsize = AtomicUsize::new(0);

extern "C" fn host_sigchld(_: libc::c_int) {
    HOST_SIGCHLD.fetch_add(1, Ordering::Relaxed);
}

fn install_host_sigchld() -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = host_sigchld as *const () as usize;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } < 0
        || unsafe { libc::sigaction(libc::SIGCHLD, &action, ptr::null_mut()) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn failure_atomic_broker_launch() -> io::Result<()> {
    let before = count_open_fds();
    let missing = CString::new("/definitely/missing/ptyx-broker").unwrap();
    assert!(launch_broker_at(&missing).is_err());
    let after = count_open_fds();
    assert_eq!(before, after);
    println!("broker_launch_failure_atomic fds_before={before} fds_after={after}");
    Ok(())
}

fn run_harness() -> io::Result<()> {
    failure_atomic_broker_launch()?;
    install_host_sigchld()?;
    let controller_baseline = count_open_fds();
    let host_signals_before = HOST_SIGCHLD.load(Ordering::Relaxed);
    let sentinel_path = CString::new("/dev/null").unwrap();
    let sentinel = unsafe { libc::open(sentinel_path.as_ptr(), libc::O_RDONLY) };
    if sentinel < 0 {
        return Err(io::Error::last_os_error());
    }
    let sentinel = unsafe { OwnedFd::from_raw_fd(sentinel) };
    unsafe {
        libc::fcntl(sentinel.as_raw_fd(), libc::F_SETFD, 0);
    }
    let mut client = launch_broker()?;
    let broker_baseline = client.stats()?;

    let running = Arc::new(AtomicBool::new(true));
    let churners: Vec<_> = (0..4)
        .map(|_| {
            let running = Arc::clone(&running);
            thread::spawn(move || {
                let path = CString::new("/dev/null").unwrap();
                while running.load(Ordering::Relaxed) {
                    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
                    if fd >= 0 {
                        unsafe {
                            libc::fcntl(fd, libc::F_SETFD, 0);
                            libc::close(fd);
                        }
                    }
                }
            })
        })
        .collect();
    for _ in 0..40 {
        let session = client.spawn(&self_argv(&["--child-inspect"]), false)?;
        let output = String::from_utf8(read_pty(
            session.master.as_raw_fd(),
            Duration::from_secs(5),
        )?)
        .unwrap();
        assert!(output.contains("controlling=true"), "{output}");
        assert!(output.contains("extra_fds=0"), "{output}");
        assert_eq!(client.wait_exit(session.id, Duration::from_secs(5))?, 0);
        assert!(client.release(session.id)?);
    }
    running.store(false, Ordering::Relaxed);
    for churner in churners {
        churner.join().unwrap();
    }
    drop(sentinel);
    println!("controlling_terminal_and_fd_churn sessions=40 leaked_child_fds=0");

    let missing = vec![CString::new("/definitely/missing/target").unwrap()];
    let missing_error = client.expect_spawn_failure(&missing, false)?;
    assert_eq!(missing_error.aux, ERROR_SPAWN);
    let injected = client.expect_spawn_failure(&self_argv(&["--child-hold"]), true)?;
    assert_eq!(injected.aux, ERROR_POST_EXEC);
    let after_failures = client.stats()?;
    assert_eq!(after_failures.0, 0);
    println!(
        "spawn_failures exec_category={} injected_category={} broker_jobs={}",
        missing_error.aux, injected.aux, after_failures.0
    );

    let exited = client.spawn(&self_argv(&["--child-exit"]), false)?;
    assert_eq!(client.wait_exit(exited.id, Duration::from_secs(5))?, 0);
    let close_result = client.close(&mut ClientSession {
        id: exited.id,
        pid: exited.pid,
        master: exited.master,
        next_sequence: exited.next_sequence,
        queued: exited.queued,
        failed: exited.failed,
    })?;
    assert_eq!(close_result, CLOSE_ALREADY_EXITED);
    let stats_after_reap_close = client.stats()?;
    assert_eq!(stats_after_reap_close.2, 0);
    assert!(client.release(exited.id)?);
    println!("close_after_reap result=already_exited signals_after_reap=0");

    let mut input = client.spawn(&self_argv(&["--child-hold"]), false)?;
    let accepted = input.accept(b"accepted but not written");
    assert_eq!(client.close(&mut input)?, CLOSE_KILLED);
    assert_eq!(
        input.flush_result(accepted),
        Err(InputFailure::SessionClosing)
    );
    assert!(client.wait_exit(input.id, Duration::from_secs(5))? < 0);
    assert!(client.release(input.id)?);
    println!(
        "accepted_input sequence={accepted} flush_failure={:?}",
        InputFailure::SessionClosing
    );
    drop(input);

    let child = client.spawn(&self_argv(&["--child-exit"]), false)?;
    assert_eq!(client.wait_exit(child.id, Duration::from_secs(5))?, 0);
    let mut wait_any_status = 0;
    let waited = unsafe { libc::waitpid(-1, &mut wait_any_status, libc::WNOHANG) };
    assert_eq!(waited, 0);
    assert_eq!(HOST_SIGCHLD.load(Ordering::Relaxed), host_signals_before);
    assert!(client.release(child.id)?);
    println!("host_wait_any_interference target_not_waitable=true host_sigchld_unchanged=true");
    drop(child);

    let before_100 = count_open_fds();
    let mut sessions = Vec::new();
    for _ in 0..100 {
        sessions.push(client.spawn(&self_argv(&["--child-hold"]), false)?);
    }
    let live_100 = count_open_fds();
    let broker_live = client.stats()?;
    assert_eq!(broker_live.0, 100);
    for session in &mut sessions {
        assert_eq!(client.close(session)?, CLOSE_KILLED);
    }
    for session in sessions {
        assert!(client.wait_exit(session.id, Duration::from_secs(5))? < 0);
        assert!(client.release(session.id)?);
    }
    let after_100 = count_open_fds();
    let broker_after = client.stats()?;
    assert_eq!(before_100, after_100);
    assert_eq!(broker_after.0, 0);
    assert_eq!(broker_after.1, broker_baseline.1);
    println!(
        "idle100 controller_fds_before={before_100} live={live_100} after={after_100} \
         broker_fds_before={} broker_fds_after={} broker_jobs_after=0",
        broker_baseline.1, broker_after.1
    );

    let orphan = client.spawn(&self_argv(&["--child-hold"]), false)?;
    let orphan_pid = orphan.pid;
    let broker_pid = client.broker_pid;
    drop(client);
    assert_eq!(wait_exact(broker_pid).map(decode_wait_status)?, 0);
    assert!(wait_until_gone(orphan_pid, Duration::from_secs(5)));
    println!("controller_eof broker_exit=0 child_reclaimed=true");
    drop(orphan);

    let mut lost_client = launch_broker()?;
    let mut lost = lost_client.spawn(&self_argv(&["--child-hold"]), false)?;
    let sequence = lost.accept(b"pending");
    unsafe {
        libc::kill(lost_client.broker_pid, libc::SIGKILL);
    }
    let _ = wait_exact(lost_client.broker_pid)?;
    assert!(lost_client.receive().is_err());
    lost_client.recover_after_broker_loss(&mut [&mut lost]);
    assert_eq!(lost.flush_result(sequence), Err(InputFailure::BrokerLost));
    assert!(wait_until_gone(lost.pid, Duration::from_secs(5)));
    println!(
        "broker_death detected=true child_best_effort_reclaimed=true input_failure={:?}",
        InputFailure::BrokerLost
    );
    drop(lost);
    drop(lost_client);

    let controller_after = count_open_fds();
    assert_eq!(controller_baseline, controller_after);
    println!(
        "final_descriptor_reclamation controller_before={controller_baseline} \
         controller_after={controller_after}"
    );
    Ok(())
}

fn main() -> io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--broker") => Broker::run(),
        Some("--child-inspect") => {
            child_inspect();
            Ok(())
        }
        Some("--child-hold") => {
            child_hold();
            Ok(())
        }
        Some("--child-exit") => {
            child_exit();
            Ok(())
        }
        _ => run_harness(),
    }
}

unsafe extern "C" {
    static mut environ: *mut *mut libc::c_char;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_rejects_oversize_payloads() {
        let mut frame = Frame::new(SPAWN);
        frame.payload = vec![0; MAX_PAYLOAD + 1];
        assert_eq!(
            frame.encode().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn generation_identity_rejects_zero_components() {
        assert_eq!(split_session(0), None);
        assert_eq!(split_session(1), None);
        assert_eq!(split_session(1_u64 << 32), None);
        assert_eq!(split_session(make_session(7, 9)), Some((7, 9)));
    }

    #[test]
    fn accepted_input_receives_a_typed_failure() {
        let path = CString::new("/dev/null").unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        let mut session = ClientSession {
            id: make_session(0, 1),
            pid: 1,
            master: unsafe { OwnedFd::from_raw_fd(fd) },
            next_sequence: 1,
            queued: VecDeque::new(),
            failed: HashMap::new(),
        };
        let sequence = session.accept(b"bytes");
        session.fail_accepted(InputFailure::SessionClosing);
        assert_eq!(
            session.flush_result(sequence),
            Err(InputFailure::SessionClosing)
        );
    }

    #[test]
    fn spawn_decoder_rejects_arbitrary_bytes_without_panicking() {
        let mut state = 0x70f4_5a9d_c2b3_1187_u64;
        for length in 0..=1024 {
            let mut payload = vec![0_u8; length];
            for byte in &mut payload {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            assert!(std::panic::catch_unwind(|| decode_spawn(&payload)).is_ok());
        }
    }

    #[test]
    fn spawn_v2_decoder_round_trips_all_owned_fields() {
        let mut payload = Vec::new();
        for value in [1, SPAWN_V2, 2, 1, 30, 100, 800, 600, 4] {
            payload.extend_from_slice(&value.to_ne_bytes());
        }
        for value in [b"/bin/sh".as_slice(), b"-c", b"TERM=xterm"] {
            payload.extend_from_slice(&(value.len() as u32).to_ne_bytes());
            payload.extend_from_slice(value);
        }
        payload.extend_from_slice(b"/tmp");
        let request = decode_spawn(&payload).unwrap();
        assert!(request.inject);
        assert_eq!(request.argv[0].as_bytes(), b"/bin/sh");
        assert_eq!(request.argv[1].as_bytes(), b"-c");
        assert_eq!(request.environment.unwrap()[0].as_bytes(), b"TERM=xterm");
        assert_eq!(request.cwd.unwrap().as_bytes(), b"/tmp");
        assert_eq!(request.size.ws_row, 30);
        assert_eq!(request.size.ws_col, 100);
        assert_eq!(request.size.ws_xpixel, 800);
        assert_eq!(request.size.ws_ypixel, 600);
    }
}
