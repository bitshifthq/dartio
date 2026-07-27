use crate::integrated::Command;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const MAGIC: u32 = 0x4258_5450;
const VERSION: u16 = 1;
const HEADER: usize = 32;
const MAX_PAYLOAD: usize = 64 * 1024;
const SPAWN_V2: u32 = 0x5854_5950;
const CONTROL_FD: RawFd = 3;
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
const SHUTDOWN: u16 = 12;
const SHUTDOWN_RESULT: u16 = 13;

const REQUEST_CAPACITY: usize = 128;
const REQUEST_QUANTUM: usize = 16;

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

fn send_frame(fd: RawFd, frame: &Frame) -> io::Result<()> {
    let bytes = frame.encode()?;
    let mut offset = 0;
    while offset < bytes.len() {
        let written = unsafe {
            libc::send(
                fd,
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
                libc::MSG_NOSIGNAL,
            )
        };
        if written > 0 {
            offset += written as usize;
            continue;
        }
        if written < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(if written < 0 {
            io::Error::last_os_error()
        } else {
            io::Error::new(io::ErrorKind::WriteZero, "broker write returned zero")
        });
    }
    Ok(())
}

fn receive_frame(fd: RawFd) -> io::Result<Option<(Frame, Option<OwnedFd>)>> {
    let mut header_bytes = [0_u8; HEADER];
    let mut iovec = libc::iovec {
        iov_base: header_bytes.as_mut_ptr().cast(),
        iov_len: header_bytes.len(),
    };
    let mut control = [0_usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as _;
    loop {
        let received = unsafe { libc::recvmsg(fd, &mut message, libc::MSG_WAITALL) };
        if received == 0 {
            return Ok(None);
        }
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if message.msg_flags & libc::MSG_CTRUNC != 0 || received as usize != HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid broker header or ancillary data",
            ));
        }
        let mut received_fd = None;
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if !header.is_null()
            && unsafe { (*header).cmsg_level == libc::SOL_SOCKET }
            && unsafe { (*header).cmsg_type == libc::SCM_RIGHTS }
            && unsafe { (*header).cmsg_len >= libc::CMSG_LEN(size_of::<RawFd>() as _) }
        {
            let mut raw = -1;
            unsafe {
                ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(header),
                    (&mut raw as *mut RawFd).cast::<u8>(),
                    size_of::<RawFd>(),
                );
            }
            if raw >= 0 {
                set_cloexec(raw)?;
                set_nonblocking(raw)?;
                received_fd = Some(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
        let length = u32::from_ne_bytes(header_bytes[8..12].try_into().unwrap()) as usize;
        if length > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker payload exceeds bound",
            ));
        }
        let mut bytes = Vec::with_capacity(HEADER + length);
        bytes.extend_from_slice(&header_bytes);
        bytes.resize(HEADER + length, 0);
        let mut offset = HEADER;
        while offset < bytes.len() {
            let read = unsafe {
                libc::recv(
                    fd,
                    bytes[offset..].as_mut_ptr().cast(),
                    bytes.len() - offset,
                    0,
                )
            };
            if read > 0 {
                offset += read as usize;
                continue;
            }
            if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(if read == 0 {
                io::Error::new(io::ErrorKind::UnexpectedEof, "partial broker payload")
            } else {
                io::Error::last_os_error()
            });
        }
        return Ok(Some((Frame::decode(&bytes)?, received_fd)));
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
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) } < 0 {
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

fn launch_broker_at(path: &CStr) -> io::Result<(OwnedFd, libc::pid_t)> {
    let (controller, broker) = socket_pair()?;
    let mut attrs_raw = ptr::null_mut();
    spawn_code(unsafe { libc::posix_spawnattr_init(&mut attrs_raw) })?;
    let mut attrs = SpawnAttrs(attrs_raw);
    let mut actions_raw = ptr::null_mut();
    spawn_code(unsafe { libc::posix_spawn_file_actions_init(&mut actions_raw) })?;
    let mut actions = FileActions(actions_raw);

    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let empty = unsafe { empty.assume_init() };
    spawn_code(unsafe { libc::posix_spawnattr_setsigmask(&mut attrs.0, &empty) })?;
    let flags = (POSIX_SPAWN_CLOEXEC_DEFAULT | libc::POSIX_SPAWN_SETSIGMASK) as libc::c_short;
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
    let (hello, passed) = receive_frame(controller.as_raw_fd())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "broker handshake EOF"))?;
    if hello.kind != HELLO || hello.aux != VERSION as u32 || passed.is_some() {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, ptr::null_mut(), 0);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker handshake rejected",
        ));
    }
    Ok((controller, pid))
}

pub(crate) struct BrokerSession {
    pub(crate) id: u64,
    pub(crate) pid: libc::pid_t,
    pub(crate) master: OwnedFd,
}

pub(crate) struct BrokerSpawn {
    pub(crate) executable: CString,
    pub(crate) arguments: Vec<CString>,
    pub(crate) environment: Option<Vec<CString>>,
    pub(crate) cwd: Option<CString>,
    pub(crate) rows: u32,
    pub(crate) columns: u32,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
}

enum Request {
    Spawn {
        config: BrokerSpawn,
        reply: Sender<io::Result<BrokerSession>>,
    },
    Close {
        session: u64,
        reply: Sender<io::Result<()>>,
    },
    Release {
        session: u64,
        reply: Sender<io::Result<()>>,
    },
    Abort {
        session: u64,
        reply: Sender<io::Result<()>>,
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
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl BrokerOwner {
    pub(crate) fn launch(
        path: &CStr,
        reactor_commands: SyncSender<Command>,
        reactor_wake: OwnedFd,
    ) -> io::Result<Self> {
        let (control, broker_pid) = launch_broker_at(path)?;
        let mut wake_pipe = [-1; 2];
        if unsafe { libc::pipe(wake_pipe.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let wake_read = unsafe { OwnedFd::from_raw_fd(wake_pipe[0]) };
        let wake_write = unsafe { OwnedFd::from_raw_fd(wake_pipe[1]) };
        set_cloexec(wake_read.as_raw_fd())?;
        set_cloexec(wake_write.as_raw_fd())?;
        set_nonblocking(wake_read.as_raw_fd())?;
        set_nonblocking(wake_write.as_raw_fd())?;
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
                    request_receiver,
                    reactor_commands,
                    reactor_wake,
                );
            })?;
        Ok(Self {
            client: BrokerClient { shared },
            pid: broker_pid,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn client(&self) -> BrokerClient {
        self.client.clone()
    }

    pub(crate) fn kill_for_test(&self) {
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
    }
}

impl Drop for BrokerOwner {
    fn drop(&mut self) {
        let _ = self.client.send(Request::Shutdown);
        if let Some(thread) = self.thread.lock().ok().and_then(|mut value| value.take()) {
            let _ = thread.join();
        }
    }
}

impl BrokerClient {
    pub(crate) fn spawn(&self, config: BrokerSpawn) -> io::Result<BrokerSession> {
        let (reply, response) = mpsc::channel();
        self.send(Request::Spawn { config, reply })?;
        response
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    pub(crate) fn close(&self, session: u64) -> io::Result<()> {
        let (reply, response) = mpsc::channel();
        self.send(Request::Close { session, reply })?;
        response
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    pub(crate) fn release(&self, session: u64) -> io::Result<()> {
        let (reply, response) = mpsc::channel();
        self.send(Request::Release { session, reply })?;
        response
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    pub(crate) fn abort(&self, session: u64) -> io::Result<()> {
        let (reply, response) = mpsc::channel();
        self.send(Request::Abort { session, reply })?;
        response
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?
    }

    fn send(&self, request: Request) -> io::Result<()> {
        self.shared
            .requests
            .send(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "broker worker stopped"))?;
        let byte = [1_u8];
        unsafe {
            libc::write(
                self.shared.wake.as_raw_fd(),
                byte.as_ptr().cast(),
                byte.len(),
            );
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
            let byte = [1_u8];
            unsafe {
                libc::write(
                    self.reactor_wake.as_raw_fd(),
                    byte.as_ptr().cast(),
                    byte.len(),
                );
            }
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

    fn spawn(&mut self, config: BrokerSpawn) -> io::Result<BrokerSession> {
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
            &(config.cwd.as_ref().map_or(0, |cwd| cwd.as_bytes().len()) as u32).to_ne_bytes(),
        );
        for argument in std::iter::once(&config.executable).chain(config.arguments.iter()) {
            let bytes = argument.as_bytes();
            payload.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
            payload.extend_from_slice(bytes);
        }
        if let Some(environment) = &config.environment {
            for entry in environment {
                let bytes = entry.as_bytes();
                payload.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
                payload.extend_from_slice(bytes);
            }
        }
        if let Some(cwd) = &config.cwd {
            payload.extend_from_slice(cwd.as_bytes());
        }
        let mut frame = Frame::new(SPAWN);
        frame.request = request;
        frame.payload = payload;
        send_frame(self.control.as_raw_fd(), &frame)?;
        let (response, master) = self.receive_for(request, SPAWN_OK)?;
        let master = master.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "broker omitted PTY master")
        })?;
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
        let _ = self.receive_for(request, CLOSE_RESULT)?;
        Ok(())
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
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "broker session is not releasable",
            ))
        }
    }

    fn abort(&mut self, session: u64) -> io::Result<()> {
        self.close(session)?;
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

    fn shutdown(&mut self) {
        let request = self.request_id();
        let mut frame = Frame::new(SHUTDOWN);
        frame.request = request;
        if send_frame(self.control.as_raw_fd(), &frame).is_ok() {
            let _ = self.receive_for(request, SHUTDOWN_RESULT);
        }
        unsafe {
            libc::waitpid(self.broker_pid, ptr::null_mut(), 0);
        }
    }
}

fn run_worker(
    control: OwnedFd,
    broker_pid: libc::pid_t,
    wake: OwnedFd,
    requests: Receiver<Request>,
    reactor_commands: SyncSender<Command>,
    reactor_wake: OwnedFd,
) {
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
        if poll[0].revents & libc::POLLIN != 0 {
            drain(wake.as_raw_fd());
            for _ in 0..REQUEST_QUANTUM {
                let Ok(request) = requests.try_recv() else {
                    break;
                };
                match request {
                    Request::Spawn { config, reply } => {
                        let _ = reply.send(worker.spawn(config));
                    }
                    Request::Close { session, reply } => {
                        let _ = reply.send(worker.close(session));
                    }
                    Request::Release { session, reply } => {
                        let _ = reply.send(worker.release(session));
                    }
                    Request::Abort { session, reply } => {
                        let _ = reply.send(worker.abort(session));
                    }
                    Request::Shutdown => {
                        worker.shutdown();
                        return;
                    }
                }
            }
        }
    }
    unsafe {
        libc::waitpid(worker.broker_pid, ptr::null_mut(), 0);
    }
    if worker.reactor_commands.send(Command::BrokerLost).is_ok() {
        let byte = [1_u8];
        unsafe {
            libc::write(
                worker.reactor_wake.as_raw_fd(),
                byte.as_ptr().cast(),
                byte.len(),
            );
        }
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
