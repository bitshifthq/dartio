use super::{dup_cloexec, set_cloexec, set_nonblocking, GenerationRegistry};
use crate::broker_client::{BrokerClient, BrokerOwner, BrokerSession, BrokerSpawn};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::io;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(target_os = "macos")]
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BYTE_QUANTUM: usize = 64 * 1024;
const SYSCALL_QUANTUM: usize = 4;
const INTERACTIVE_BATCH: usize = 256;
const OUTPUT_BATCH: usize = 64 * 1024;
const OUTPUT_DELAY: Duration = Duration::from_millis(1);
const COMMAND_QUANTUM: usize = 64;
const COMMAND_CAPACITY: usize = 1024;
const NOTICE_CAPACITY: usize = 4096;
const WAITER_CAPACITY: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    Output { handle: u64, bytes: Vec<u8> },
    Capacity { handle: u64, waiter: u64 },
    Flush { handle: u64, waiter: u64 },
    WaitFailed { handle: u64, waiter: u64 },
    InputFailed(u64),
    OutputFailed(u64),
    BrokerLost(u64),
    OutputDone(u64),
    Exit(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitResult {
    Ready,
    Armed,
    Failed,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeCounters {
    pub reactor_wakeups: u64,
    pub reactor_events: u64,
    pub command_wakeups: u64,
    pub read_syscalls: u64,
    pub write_syscalls: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub notifications: u64,
}

struct QueuedInput {
    bytes: Vec<u8>,
    offset: usize,
    sequence: u64,
}

struct QueuedOutput {
    bytes: Vec<u8>,
    offset: usize,
}

struct Session {
    broker_session: u64,
    pid: libc::pid_t,
    master: OwnedFd,
    input_capacity: usize,
    input_bytes: usize,
    input: VecDeque<QueuedInput>,
    capacity_waiters: HashMap<u64, usize>,
    flush_waiters: HashMap<u64, u64>,
    input_failed_from: Option<u64>,
    next_sequence: u64,
    flushed_sequence: u64,
    output_capacity: usize,
    output_bytes: usize,
    output: VecDeque<QueuedOutput>,
    output_outstanding: usize,
    output_deadline: Option<Instant>,
    output_notified: bool,
    output_done_notified: bool,
    output_failed: bool,
    paused: bool,
    output_eof: bool,
    exit_status: Option<i64>,
    close_started: bool,
    active: bool,
    abandoned: bool,
    read_filter_enabled: Option<bool>,
    write_filter_enabled: Option<bool>,
    #[cfg(target_os = "linux")]
    readiness_registered: bool,
}

impl Session {
    fn from_broker(broker: BrokerSession, input_capacity: usize, output_capacity: usize) -> Self {
        Self {
            broker_session: broker.id,
            pid: broker.pid,
            master: broker.master,
            input_capacity,
            input_bytes: 0,
            input: VecDeque::new(),
            capacity_waiters: HashMap::new(),
            flush_waiters: HashMap::new(),
            input_failed_from: None,
            next_sequence: 1,
            flushed_sequence: 0,
            output_capacity,
            output_bytes: 0,
            output: VecDeque::new(),
            output_outstanding: 0,
            output_deadline: None,
            output_notified: false,
            output_done_notified: false,
            output_failed: false,
            paused: true,
            output_eof: false,
            exit_status: None,
            close_started: false,
            active: false,
            abandoned: false,
            read_filter_enabled: None,
            write_filter_enabled: None,
            #[cfg(target_os = "linux")]
            readiness_registered: false,
        }
    }

    fn try_write(&mut self, bytes: Vec<u8>) -> Result<u64, Vec<u8>> {
        if self.close_started
            || self.input_failed_from.is_some()
            || bytes.is_empty()
            || self.input_bytes.saturating_add(bytes.len()) > self.input_capacity
        {
            return Err(bytes);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.input_bytes += bytes.len();
        self.input.push_back(QueuedInput {
            bytes,
            offset: 0,
            sequence,
        });
        Ok(sequence)
    }

    fn wait_capacity(&mut self, required: usize, waiter: u64) -> WaitResult {
        if self.close_started || self.input_failed_from.is_some() || required > self.input_capacity
        {
            return WaitResult::Failed;
        }
        if required <= self.input_capacity - self.input_bytes {
            return WaitResult::Ready;
        }
        if self.capacity_waiters.len() >= WAITER_CAPACITY
            || self.capacity_waiters.contains_key(&waiter)
        {
            return WaitResult::Failed;
        }
        self.capacity_waiters.insert(waiter, required);
        WaitResult::Armed
    }

    fn wait_flush(&mut self, sequence: u64, waiter: u64) -> WaitResult {
        if sequence <= self.flushed_sequence {
            return WaitResult::Ready;
        }
        if self.close_started
            || self
                .input_failed_from
                .is_some_and(|failed| sequence >= failed)
        {
            return WaitResult::Failed;
        }
        if self.flush_waiters.len() >= WAITER_CAPACITY || self.flush_waiters.contains_key(&waiter) {
            return WaitResult::Failed;
        }
        self.flush_waiters.insert(waiter, sequence);
        WaitResult::Armed
    }

    fn pull(&mut self, maximum: usize) -> Vec<u8> {
        let amount = maximum.min(self.output_bytes);
        let mut bytes = Vec::with_capacity(amount);
        while bytes.len() < amount {
            let front = self.output.front_mut().unwrap();
            let available = front.bytes.len() - front.offset;
            let take = available.min(amount - bytes.len());
            bytes.extend_from_slice(&front.bytes[front.offset..front.offset + take]);
            front.offset += take;
            if front.offset == front.bytes.len() {
                self.output.pop_front();
            }
        }
        self.output_bytes -= bytes.len();
        self.output_outstanding += bytes.len();
        if self.output.is_empty() {
            self.output_deadline = None;
        }
        self.output_notified = false;
        bytes
    }

    fn credit(&mut self, bytes: usize) -> bool {
        if bytes > self.output_outstanding {
            return false;
        }
        self.output_outstanding -= bytes;
        if bytes != 0 {
            self.output_notified = false;
        }
        true
    }

    fn exchange(&mut self, credit: usize, maximum: usize) -> Option<Vec<u8>> {
        if !self.credit(credit) {
            return None;
        }
        Some(self.pull(maximum))
    }

    fn output_total(&self) -> usize {
        self.output_bytes + self.output_outstanding
    }
}

pub(crate) enum Command {
    Add {
        broker: BrokerSession,
        input_capacity: usize,
        output_capacity: usize,
        reply: Sender<io::Result<u64>>,
    },
    Activate {
        handle: u64,
        reply: Sender<io::Result<()>>,
    },
    Write {
        handle: u64,
        bytes: Vec<u8>,
        reply: Sender<u64>,
    },
    Pull {
        handle: u64,
        maximum: usize,
        reply: Sender<Option<Vec<u8>>>,
    },
    Credit {
        handle: u64,
        bytes: usize,
        reply: Sender<bool>,
    },
    CreditAsync {
        handle: u64,
        bytes: usize,
    },
    Exchange {
        handle: u64,
        credit: usize,
        maximum: usize,
        reply: Sender<Option<Vec<u8>>>,
    },
    FlushReady {
        handle: u64,
        sequence: u64,
        reply: Sender<bool>,
    },
    WaitCapacity {
        handle: u64,
        required: usize,
        waiter: u64,
        reply: Sender<WaitResult>,
    },
    WaitFlush {
        handle: u64,
        sequence: u64,
        waiter: u64,
        reply: Sender<WaitResult>,
    },
    Pause {
        handle: u64,
        paused: bool,
        reply: Sender<bool>,
    },
    ExitStatus {
        handle: u64,
        reply: Sender<Option<i64>>,
    },
    Pid {
        handle: u64,
        reply: Sender<Option<i32>>,
    },
    Size {
        handle: u64,
        reply: Sender<Option<[u32; 4]>>,
    },
    Resize {
        handle: u64,
        size: [u32; 4],
        reply: Sender<bool>,
    },
    Mode {
        handle: u64,
        reply: Sender<Option<[bool; 3]>>,
    },
    TtyName {
        handle: u64,
        reply: Sender<Option<Vec<u8>>>,
    },
    Signal {
        handle: u64,
        signal: i32,
        reply: Sender<Option<bool>>,
    },
    OutputTotal {
        handle: u64,
        reply: Sender<Option<usize>>,
    },
    OutputDone {
        handle: u64,
        reply: Sender<bool>,
    },
    Close {
        handle: u64,
        reply: Sender<bool>,
    },
    Destroy {
        handle: u64,
        reply: Sender<bool>,
    },
    Abandon {
        handle: u64,
    },
    Counters {
        reply: Sender<RuntimeCounters>,
    },
    BrokerExit {
        broker_session: u64,
        status: i32,
    },
    BrokerLost,
    Shutdown,
}

struct WakeWriter(OwnedFd);

impl WakeWriter {
    fn wake(&self) {
        let byte = [1_u8];
        unsafe {
            libc::write(self.0.as_raw_fd(), byte.as_ptr().cast(), byte.len());
        }
    }
}

pub struct IntegratedRuntime {
    commands: SyncSender<Command>,
    notices: Option<Receiver<Notice>>,
    wake: WakeWriter,
    thread: Mutex<Option<JoinHandle<()>>>,
    broker: BrokerOwner,
}

impl IntegratedRuntime {
    pub fn try_new() -> io::Result<Self> {
        let mut pipe = [-1; 2];
        #[cfg(target_os = "macos")]
        let pipe_result = unsafe { libc::pipe(pipe.as_mut_ptr()) };
        #[cfg(target_os = "linux")]
        let pipe_result =
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if pipe_result < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        set_cloexec(read.as_raw_fd())?;
        set_cloexec(write.as_raw_fd())?;
        set_nonblocking(read.as_raw_fd())?;
        set_nonblocking(write.as_raw_fd())?;
        let reactor_wake = dup_cloexec(write.as_raw_fd())?;
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (notice_sender, notice_receiver) = mpsc::sync_channel(NOTICE_CAPACITY);
        let broker_reactor_wake = dup_cloexec(write.as_raw_fd())?;
        let materialized = crate::broker_materializer::broker_path()?;
        let broker_path = CString::new(materialized.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "NUL broker path"))?;
        let broker =
            BrokerOwner::launch(&broker_path, command_sender.clone(), broker_reactor_wake)?;
        let broker_client = broker.client();
        let thread = thread::Builder::new()
            .name("ptyx-integrated-reactor".to_owned())
            .spawn(move || {
                reactor(
                    read,
                    reactor_wake,
                    command_receiver,
                    notice_sender,
                    broker_client,
                )
            })?;
        Ok(Self {
            commands: command_sender,
            notices: Some(notice_receiver),
            wake: WakeWriter(write),
            thread: Mutex::new(Some(thread)),
            broker,
        })
    }

    pub fn take_notifications(&mut self) -> Option<Receiver<Notice>> {
        self.notices.take()
    }

    pub(crate) fn spawn_staged(
        &self,
        config: BrokerSpawn,
        input_capacity: usize,
        output_capacity: usize,
    ) -> io::Result<u64> {
        if input_capacity == 0
            || input_capacity > 64 * 1024 * 1024
            || output_capacity == 0
            || output_capacity > 64 * 1024 * 1024
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capacities must be nonzero",
            ));
        }
        let broker = self.broker.client().spawn(config)?;
        let broker_session = broker.id;
        match self.request_result(|reply| Command::Add {
            broker,
            input_capacity,
            output_capacity,
            reply,
        }) {
            Ok(result) => result,
            Err(error) => {
                let _ = self.broker.client().abort(broker_session);
                Err(error)
            }
        }
    }

    pub fn activate(&self, handle: u64) -> bool {
        self.activate_result(handle).is_ok()
    }

    fn activate_result(&self, handle: u64) -> io::Result<()> {
        self.request(|reply| Command::Activate { handle, reply })
    }

    pub fn try_write(&self, handle: u64, bytes: Vec<u8>) -> u64 {
        self.request(|reply| Command::Write {
            handle,
            bytes,
            reply,
        })
    }

    pub fn pull(&self, handle: u64, maximum: usize) -> Option<Vec<u8>> {
        self.request(|reply| Command::Pull {
            handle,
            maximum,
            reply,
        })
    }

    pub fn credit(&self, handle: u64, bytes: usize) -> bool {
        self.request(|reply| Command::Credit {
            handle,
            bytes,
            reply,
        })
    }

    pub fn credit_async(&self, handle: u64, bytes: usize) -> bool {
        if self
            .commands
            .try_send(Command::CreditAsync { handle, bytes })
            .is_err()
        {
            return false;
        }
        self.wake.wake();
        true
    }

    pub fn exchange(&self, handle: u64, credit: usize, maximum: usize) -> Option<Vec<u8>> {
        self.request(|reply| Command::Exchange {
            handle,
            credit,
            maximum,
            reply,
        })
    }

    pub fn flush_ready(&self, handle: u64, sequence: u64) -> bool {
        self.request(|reply| Command::FlushReady {
            handle,
            sequence,
            reply,
        })
    }

    pub fn wait_capacity(&self, handle: u64, required: usize, waiter: u64) -> WaitResult {
        self.request(|reply| Command::WaitCapacity {
            handle,
            required,
            waiter,
            reply,
        })
    }

    pub fn wait_flush(&self, handle: u64, sequence: u64, waiter: u64) -> WaitResult {
        self.request(|reply| Command::WaitFlush {
            handle,
            sequence,
            waiter,
            reply,
        })
    }

    pub fn pause(&self, handle: u64, paused: bool) -> bool {
        self.request(|reply| Command::Pause {
            handle,
            paused,
            reply,
        })
    }

    pub fn exit_status(&self, handle: u64) -> Option<i64> {
        self.request(|reply| Command::ExitStatus { handle, reply })
    }

    pub fn pid(&self, handle: u64) -> Option<i32> {
        self.request(|reply| Command::Pid { handle, reply })
    }

    pub fn size(&self, handle: u64) -> Option<[u32; 4]> {
        self.request(|reply| Command::Size { handle, reply })
    }

    pub fn resize(&self, handle: u64, size: [u32; 4]) -> bool {
        self.request(|reply| Command::Resize {
            handle,
            size,
            reply,
        })
    }

    pub fn mode(&self, handle: u64) -> Option<[bool; 3]> {
        self.request(|reply| Command::Mode { handle, reply })
    }

    pub fn tty_name(&self, handle: u64) -> Option<Vec<u8>> {
        self.request(|reply| Command::TtyName { handle, reply })
    }

    pub fn signal(&self, handle: u64, signal: i32) -> Option<bool> {
        self.request(|reply| Command::Signal {
            handle,
            signal,
            reply,
        })
    }

    pub fn output_total(&self, handle: u64) -> Option<usize> {
        self.request(|reply| Command::OutputTotal { handle, reply })
    }

    pub fn output_done(&self, handle: u64) -> bool {
        self.request(|reply| Command::OutputDone { handle, reply })
    }

    pub fn close(&self, handle: u64) -> bool {
        self.request(|reply| Command::Close { handle, reply })
    }

    pub fn destroy(&self, handle: u64) -> bool {
        self.request(|reply| Command::Destroy { handle, reply })
    }

    pub fn try_abandon(&self, handle: u64) -> bool {
        if self.commands.try_send(Command::Abandon { handle }).is_err() {
            return false;
        }
        self.wake.wake();
        true
    }

    pub fn counters(&self) -> RuntimeCounters {
        self.request(|reply| Command::Counters { reply })
    }

    pub fn kill_broker_for_test(&self) {
        self.broker.kill_for_test();
    }

    fn request<R>(&self, command: impl FnOnce(Sender<R>) -> Command) -> R {
        self.request_result(command)
            .expect("ptyx reactor request channel closed")
    }

    fn request_result<R>(&self, command: impl FnOnce(Sender<R>) -> Command) -> io::Result<R> {
        let (sender, receiver) = mpsc::channel();
        self.commands
            .send(command(sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))?;
        self.wake.wake();
        receiver
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))
    }
}

impl Drop for IntegratedRuntime {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        self.wake.wake();
        if let Some(thread) = self.thread.lock().ok().and_then(|mut value| value.take()) {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "macos")]
fn reactor(
    wake: OwnedFd,
    self_wake: OwnedFd,
    commands: Receiver<Command>,
    notices: SyncSender<Notice>,
    broker: BrokerClient,
) {
    let mut sessions: GenerationRegistry<Session> = GenerationRegistry::new();
    let mut counters = RuntimeCounters::default();
    let mut rotation = 0;
    loop {
        let mut handles = sessions.handles();
        if !handles.is_empty() {
            let length = handles.len();
            handles.rotate_left(rotation % length);
            rotation = rotation.wrapping_add(1);
        }
        let mut poll_descriptors = Vec::with_capacity(handles.len() + 1);
        let mut poll_handles = Vec::with_capacity(handles.len());
        poll_descriptors.push(libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        for handle in handles {
            let Some(session) = sessions.get(handle) else {
                continue;
            };
            let mut events = 0;
            if session.read_filter_enabled == Some(true) {
                events |= libc::POLLIN;
            }
            if session.write_filter_enabled == Some(true) {
                events |= libc::POLLOUT;
            }
            if events != 0 {
                poll_descriptors.push(libc::pollfd {
                    fd: session.master.as_raw_fd(),
                    events,
                    revents: 0,
                });
                poll_handles.push(handle);
            }
        }
        let ready = unsafe {
            libc::poll(
                poll_descriptors.as_mut_ptr(),
                poll_descriptors.len() as libc::nfds_t,
                output_poll_timeout(&sessions),
            )
        };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            fail_all(&notices, &mut sessions, &mut counters, &broker);
            return;
        }
        counters.reactor_wakeups += 1;
        counters.reactor_events += poll_descriptors
            .iter()
            .filter(|descriptor| descriptor.revents != 0)
            .count() as u64;
        let mut shutdown = false;
        if poll_descriptors[0].revents & libc::POLLIN != 0 {
            drain_wake(wake.as_raw_fd());
            counters.command_wakeups += 1;
            let (requested_shutdown, more_commands) = process_commands(
                -1,
                &commands,
                &notices,
                &mut sessions,
                &mut counters,
                &broker,
            );
            shutdown = requested_shutdown;
            if more_commands {
                let byte = [1_u8];
                unsafe {
                    libc::write(self_wake.as_raw_fd(), byte.as_ptr().cast(), byte.len());
                }
            }
        }
        for (index, handle) in poll_handles.into_iter().enumerate() {
            let ready = poll_descriptors[index + 1].revents;
            if ready & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                read_ready(-1, handle, &notices, &mut sessions, &mut counters);
            }
            if ready & libc::POLLOUT != 0 {
                write_ready(-1, handle, &notices, &mut sessions, &mut counters);
            }
        }
        refresh_due_outputs(-1, &notices, &mut sessions, &mut counters);
        reap_abandoned(&mut sessions, &broker);
        if shutdown {
            shutdown_all(&mut sessions, &broker);
            return;
        }
    }
}

#[cfg(target_os = "linux")]
fn reactor(
    wake: OwnedFd,
    self_wake: OwnedFd,
    commands: Receiver<Command>,
    notices: SyncSender<Notice>,
    broker: BrokerClient,
) {
    let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll < 0 {
        return;
    }
    let epoll = unsafe { OwnedFd::from_raw_fd(epoll) };
    let mut wake_event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0,
    };
    if unsafe {
        libc::epoll_ctl(
            epoll.as_raw_fd(),
            libc::EPOLL_CTL_ADD,
            wake.as_raw_fd(),
            &mut wake_event,
        )
    } < 0
    {
        return;
    }

    let mut sessions: GenerationRegistry<Session> = GenerationRegistry::new();
    let mut counters = RuntimeCounters::default();
    let mut rotation = 0;
    loop {
        let mut events: [MaybeUninit<libc::epoll_event>; 128] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let ready = unsafe {
            libc::epoll_wait(
                epoll.as_raw_fd(),
                events.as_mut_ptr().cast(),
                events.len() as i32,
                output_poll_timeout(&sessions),
            )
        };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            fail_all(&notices, &mut sessions, &mut counters, &broker);
            return;
        }
        counters.reactor_wakeups += 1;
        counters.reactor_events += ready as u64;
        let mut ready_events: Vec<_> = events[..ready as usize]
            .iter()
            .map(|event| unsafe { event.assume_init() })
            .collect();
        if !ready_events.is_empty() {
            let length = ready_events.len();
            ready_events.rotate_left(rotation % length);
            rotation = rotation.wrapping_add(1);
        }
        let mut shutdown = false;
        for event in ready_events {
            let handle = event.u64;
            if handle == 0 {
                drain_wake(wake.as_raw_fd());
                counters.command_wakeups += 1;
                let (requested_shutdown, more_commands) = process_commands(
                    epoll.as_raw_fd(),
                    &commands,
                    &notices,
                    &mut sessions,
                    &mut counters,
                    &broker,
                );
                shutdown |= requested_shutdown;
                if more_commands {
                    let byte = [1_u8];
                    unsafe {
                        libc::write(self_wake.as_raw_fd(), byte.as_ptr().cast(), byte.len());
                    }
                }
                continue;
            }
            if event.events & (libc::EPOLLIN | libc::EPOLLHUP | libc::EPOLLERR) as u32 != 0 {
                read_ready(
                    epoll.as_raw_fd(),
                    handle,
                    &notices,
                    &mut sessions,
                    &mut counters,
                );
            }
            if event.events & libc::EPOLLOUT as u32 != 0 {
                write_ready(
                    epoll.as_raw_fd(),
                    handle,
                    &notices,
                    &mut sessions,
                    &mut counters,
                );
            }
        }
        refresh_due_outputs(epoll.as_raw_fd(), &notices, &mut sessions, &mut counters);
        reap_abandoned(&mut sessions, &broker);
        if shutdown {
            shutdown_all(&mut sessions, &broker);
            return;
        }
    }
}

fn process_commands(
    kqueue: RawFd,
    commands: &Receiver<Command>,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    broker_client: &BrokerClient,
) -> (bool, bool) {
    let mut processed = 0;
    for _ in 0..COMMAND_QUANTUM {
        let Ok(command) = commands.try_recv() else {
            break;
        };
        processed += 1;
        match command {
            Command::Add {
                broker,
                input_capacity,
                output_capacity,
                reply,
            } => {
                let result = Ok(sessions.insert(Session::from_broker(
                    broker,
                    input_capacity,
                    output_capacity,
                )));
                let _ = reply.send(result);
            }
            Command::Activate { handle, reply } => {
                let result = if let Some(session) = sessions.get_mut(handle) {
                    register_session(kqueue, handle, session)
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "stale session"))
                };
                if result.is_ok() {
                    if let Some(session) = sessions.get_mut(handle) {
                        session.active = true;
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if sessions
                        .get(handle)
                        .is_some_and(|session| session.exit_status.is_some())
                    {
                        send_notice(notices, Notice::Exit(handle), counters);
                    }
                    refresh_output(kqueue, handle, notices, sessions, counters);
                } else if let Some(mut session) = sessions.remove(handle) {
                    let _ = broker_client.abort_async(session.broker_session);
                    session.close_started = true;
                }
                let _ = reply.send(result);
            }
            Command::Write {
                handle,
                bytes,
                reply,
            } => {
                let sequence = sessions
                    .get_mut(handle)
                    .and_then(|session| session.try_write(bytes).ok())
                    .unwrap_or(0);
                if sequence != 0 {
                    if let Some(session) = sessions.get_mut(handle).filter(|session| session.active)
                    {
                        let _ = update_write_filter(kqueue, handle, session, true);
                    }
                }
                let _ = reply.send(sequence);
            }
            Command::Pull {
                handle,
                maximum,
                reply,
            } => {
                let bytes = sessions
                    .get_mut(handle)
                    .map(|session| session.pull(maximum));
                if sessions.get(handle).is_some_and(|session| session.active) {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
                let _ = reply.send(bytes);
            }
            Command::Credit {
                handle,
                bytes,
                reply,
            } => {
                let credited = sessions
                    .get_mut(handle)
                    .is_some_and(|session| session.credit(bytes));
                if sessions.get(handle).is_some_and(|session| session.active) {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
                let _ = reply.send(credited);
            }
            Command::CreditAsync { handle, bytes } => {
                if sessions
                    .get_mut(handle)
                    .is_some_and(|session| session.credit(bytes))
                    && sessions.get(handle).is_some_and(|session| session.active)
                {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
            }
            Command::Exchange {
                handle,
                credit,
                maximum,
                reply,
            } => {
                let bytes = sessions
                    .get_mut(handle)
                    .and_then(|session| session.exchange(credit, maximum));
                if sessions.get(handle).is_some_and(|session| session.active) {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
                let _ = reply.send(bytes);
            }
            Command::FlushReady {
                handle,
                sequence,
                reply,
            } => {
                let ready = sessions
                    .get(handle)
                    .is_some_and(|session| sequence <= session.flushed_sequence);
                let _ = reply.send(ready);
            }
            Command::WaitCapacity {
                handle,
                required,
                waiter,
                reply,
            } => {
                let result = sessions
                    .get_mut(handle)
                    .map_or(WaitResult::Failed, |session| {
                        session.wait_capacity(required, waiter)
                    });
                let _ = reply.send(result);
            }
            Command::WaitFlush {
                handle,
                sequence,
                waiter,
                reply,
            } => {
                let result = sessions
                    .get_mut(handle)
                    .map_or(WaitResult::Failed, |session| {
                        session.wait_flush(sequence, waiter)
                    });
                let _ = reply.send(result);
            }
            Command::Pause {
                handle,
                paused,
                reply,
            } => {
                let found = if let Some(session) = sessions.get_mut(handle) {
                    session.paused = paused;
                    true
                } else {
                    false
                };
                if sessions.get(handle).is_some_and(|session| session.active) {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
                let _ = reply.send(found);
            }
            Command::ExitStatus { handle, reply } => {
                let _ = reply.send(sessions.get(handle).and_then(|session| session.exit_status));
            }
            Command::Pid { handle, reply } => {
                let _ = reply.send(sessions.get(handle).map(|session| session.pid));
            }
            Command::Size { handle, reply } => {
                let size = sessions.get(handle).and_then(session_size);
                let _ = reply.send(size);
            }
            Command::Resize {
                handle,
                size,
                reply,
            } => {
                let resized = sessions.get(handle).is_some_and(|session| {
                    let size = libc::winsize {
                        ws_row: size[0] as _,
                        ws_col: size[1] as _,
                        ws_xpixel: size[2] as _,
                        ws_ypixel: size[3] as _,
                    };
                    unsafe {
                        libc::ioctl(session.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) == 0
                    }
                });
                let _ = reply.send(resized);
            }
            Command::Mode { handle, reply } => {
                let mode = sessions.get(handle).and_then(session_mode);
                let _ = reply.send(mode);
            }
            Command::TtyName { handle, reply } => {
                let name = sessions.get(handle).and_then(session_tty_name);
                let _ = reply.send(name);
            }
            Command::Signal {
                handle,
                signal,
                reply,
            } => {
                match sessions.get(handle) {
                    Some(session) if session.exit_status.is_some() => {
                        let _ = reply.send(Some(false));
                    }
                    Some(session) => {
                        if broker_client
                            .signal_async(session.broker_session, signal, reply)
                            .is_err()
                        {
                            // The receiver observes channel closure as native
                            // signal failure. Keep the reactor available to
                            // make progress for unrelated sessions.
                        }
                    }
                    None => {
                        let _ = reply.send(None);
                    }
                }
            }
            Command::OutputTotal { handle, reply } => {
                let _ = reply.send(sessions.get(handle).map(Session::output_total));
            }
            Command::OutputDone { handle, reply } => {
                let done = sessions.get(handle).is_some_and(|session| {
                    session.output_eof
                        && session.output.is_empty()
                        && session.output_outstanding == 0
                });
                let _ = reply.send(done);
            }
            Command::Close { handle, reply } => {
                let closed = sessions.get_mut(handle).is_some_and(|session| {
                    fail_input_waiters(handle, session, notices, counters);
                    close_session(session, broker_client)
                });
                let _ = reply.send(closed);
            }
            Command::Destroy { handle, reply } => {
                let removable = sessions
                    .get(handle)
                    .is_some_and(|session| session.exit_status.is_some());
                let removed = if removable {
                    sessions.remove(handle).is_some_and(|session| {
                        broker_client.release_async(session.broker_session).is_ok()
                    })
                } else {
                    false
                };
                let _ = reply.send(removed);
            }
            Command::Abandon { handle } => {
                if let Some(session) = sessions.get_mut(handle) {
                    session.abandoned = true;
                    session.active = false;
                    session.paused = false;
                    session.input.clear();
                    session.input_bytes = 0;
                    session.capacity_waiters.clear();
                    session.flush_waiters.clear();
                    session.output.clear();
                    session.output_bytes = 0;
                    session.output_outstanding = 0;
                    let _ = close_session(session, broker_client);
                    let _ = update_read_filter(kqueue, handle, session, true);
                    let _ = update_write_filter(kqueue, handle, session, false);
                }
            }
            Command::Counters { reply } => {
                let _ = reply.send(*counters);
            }
            Command::BrokerExit {
                broker_session,
                status,
            } => {
                if let Some(handle) = sessions.handles().into_iter().find(|handle| {
                    sessions
                        .get(*handle)
                        .is_some_and(|session| session.broker_session == broker_session)
                }) {
                    if let Some(session) = sessions.get_mut(handle) {
                        session.exit_status = Some(i64::from(status));
                        if !session.abandoned {
                            fail_input_waiters(handle, session, notices, counters);
                        }
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if sessions.get(handle).is_some_and(|session| session.active) {
                        send_notice(notices, Notice::Exit(handle), counters);
                    }
                }
            }
            Command::BrokerLost => {
                for handle in sessions.handles() {
                    if let Some(mut session) = sessions.remove(handle) {
                        fail_input_waiters(handle, &mut session, notices, counters);
                        send_notice(notices, Notice::BrokerLost(handle), counters);
                    }
                }
            }
            Command::Shutdown => return (true, false),
        }
    }
    (false, processed == COMMAND_QUANTUM)
}

fn register_session(kqueue: RawFd, handle: u64, session: &mut Session) -> io::Result<()> {
    update_read_filter(kqueue, handle, session, !session.paused)?;
    update_write_filter(kqueue, handle, session, !session.input.is_empty())
}

fn update_read_filter(
    kqueue: RawFd,
    handle: u64,
    session: &mut Session,
    enabled: bool,
) -> io::Result<()> {
    if session.read_filter_enabled == Some(enabled) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if kqueue >= 0 {
        set_filter(
            kqueue,
            session.master.as_raw_fd(),
            libc::EVFILT_READ,
            enabled,
            handle,
        )?;
    }
    session.read_filter_enabled = Some(enabled);
    #[cfg(target_os = "linux")]
    update_epoll_interest(kqueue, handle, session)?;
    Ok(())
}

fn update_write_filter(
    kqueue: RawFd,
    handle: u64,
    session: &mut Session,
    enabled: bool,
) -> io::Result<()> {
    if session.write_filter_enabled == Some(enabled) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if kqueue >= 0 {
        set_filter(
            kqueue,
            session.master.as_raw_fd(),
            libc::EVFILT_WRITE,
            enabled,
            handle,
        )?;
    }
    session.write_filter_enabled = Some(enabled);
    #[cfg(target_os = "linux")]
    update_epoll_interest(kqueue, handle, session)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn update_epoll_interest(epoll: RawFd, handle: u64, session: &mut Session) -> io::Result<()> {
    let mut events = 0_u32;
    if session.read_filter_enabled == Some(true) {
        events |= libc::EPOLLIN as u32;
    }
    if session.write_filter_enabled == Some(true) {
        events |= libc::EPOLLOUT as u32;
    }
    if events == 0 {
        if !session.readiness_registered {
            return Ok(());
        }
        let result = unsafe {
            libc::epoll_ctl(
                epoll,
                libc::EPOLL_CTL_DEL,
                session.master.as_raw_fd(),
                std::ptr::null_mut(),
            )
        };
        if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
            return Err(io::Error::last_os_error());
        }
        session.readiness_registered = false;
        return Ok(());
    }
    let operation = if session.readiness_registered {
        libc::EPOLL_CTL_MOD
    } else {
        libc::EPOLL_CTL_ADD
    };
    let mut event = libc::epoll_event {
        events,
        u64: handle,
    };
    if unsafe { libc::epoll_ctl(epoll, operation, session.master.as_raw_fd(), &mut event) } < 0 {
        return Err(io::Error::last_os_error());
    }
    session.readiness_registered = true;
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_filter(
    kqueue: RawFd,
    fd: RawFd,
    filter: libc::c_short,
    enabled: bool,
    handle: u64,
) -> io::Result<()> {
    let flags = libc::EV_ADD
        | if enabled {
            libc::EV_ENABLE
        } else {
            libc::EV_DISABLE
        };
    submit(kqueue, &event(fd as usize, filter, flags, 0, handle))
}

fn read_ready(
    kqueue: RawFd,
    handle: u64,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let mut bytes = 0;
    let mut syscalls = 0;
    while bytes < BYTE_QUANTUM
        && syscalls < SYSCALL_QUANTUM
        && !session.paused
        && !session.output_eof
        && session.output_total() < session.output_capacity
    {
        let maximum = (session.output_capacity - session.output_total())
            .min(64 * 1024)
            .min(BYTE_QUANTUM - bytes);
        let mut buffer = [0_u8; 64 * 1024];
        let result = unsafe {
            libc::read(
                session.master.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                maximum,
            )
        };
        counters.read_syscalls += 1;
        syscalls += 1;
        if result > 0 {
            let amount = result as usize;
            counters.read_bytes += amount as u64;
            bytes += amount;
            if session.abandoned {
                continue;
            }
            if session.output_bytes == 0 {
                session.output_deadline = Some(Instant::now() + OUTPUT_DELAY);
            }
            session.output_bytes += amount;
            session.output.push_back(QueuedOutput {
                bytes: buffer[..amount].to_vec(),
                offset: 0,
            });
            continue;
        }
        if result == 0 {
            session.output_eof = true;
            fail_input_waiters(handle, session, notices, counters);
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock {
            break;
        }
        if error.raw_os_error() == Some(libc::EIO) {
            session.output_eof = true;
            fail_input_waiters(handle, session, notices, counters);
        } else {
            session.output_eof = true;
            session.output_failed = true;
        }
        break;
    }
    refresh_output(kqueue, handle, notices, sessions, counters);
}

fn write_ready(
    kqueue: RawFd,
    handle: u64,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let mut bytes = 0;
    let mut syscalls = 0;
    let mut progressed = false;
    while bytes < BYTE_QUANTUM && syscalls < SYSCALL_QUANTUM {
        let Some(front) = session.input.front_mut() else {
            break;
        };
        let maximum = (front.bytes.len() - front.offset).min(BYTE_QUANTUM - bytes);
        let result = unsafe {
            libc::write(
                session.master.as_raw_fd(),
                front.bytes[front.offset..front.offset + maximum]
                    .as_ptr()
                    .cast(),
                maximum,
            )
        };
        counters.write_syscalls += 1;
        syscalls += 1;
        if result > 0 {
            let amount = result as usize;
            counters.write_bytes += amount as u64;
            bytes += amount;
            progressed = true;
            front.offset += amount;
            session.input_bytes -= amount;
            if front.offset == front.bytes.len() {
                session.flushed_sequence = front.sequence;
                session.input.pop_front();
            }
            continue;
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                break;
            }
            fail_input_waiters(handle, session, notices, counters);
        }
        break;
    }
    if session.input.is_empty() {
        let _ = update_write_filter(kqueue, handle, session, false);
    }
    if progressed {
        notify_waiters(handle, session, notices, counters);
    }
}

fn fail_input_waiters(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    let first_failure = session.input_failed_from.is_none();
    let accepted_input_pending = !session.input.is_empty()
        || session
            .flush_waiters
            .values()
            .any(|sequence| *sequence > session.flushed_sequence);
    session.input_failed_from.get_or_insert(
        session
            .input
            .front()
            .map_or(session.flushed_sequence.saturating_add(1), |input| {
                input.sequence
            }),
    );
    session.input.clear();
    session.input_bytes = 0;
    let waiters: Vec<_> = session
        .capacity_waiters
        .drain()
        .map(|(waiter, _)| waiter)
        .chain(session.flush_waiters.drain().map(|(waiter, _)| waiter))
        .collect();
    for waiter in waiters {
        send_notice(notices, Notice::WaitFailed { handle, waiter }, counters);
    }
    if first_failure && accepted_input_pending {
        send_notice(notices, Notice::InputFailed(handle), counters);
    }
}

fn notify_waiters(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    let available = session.input_capacity - session.input_bytes;
    let capacity_waiters: Vec<_> = session
        .capacity_waiters
        .iter()
        .filter_map(|(&waiter, &required)| (required <= available).then_some(waiter))
        .collect();
    for waiter in capacity_waiters {
        session.capacity_waiters.remove(&waiter);
        send_notice(notices, Notice::Capacity { handle, waiter }, counters);
    }
    let flush_waiters: Vec<_> = session
        .flush_waiters
        .iter()
        .filter_map(|(&waiter, &sequence)| (sequence <= session.flushed_sequence).then_some(waiter))
        .collect();
    for waiter in flush_waiters {
        session.flush_waiters.remove(&waiter);
        send_notice(notices, Notice::Flush { handle, waiter }, counters);
    }
}

fn refresh_output(
    kqueue: RawFd,
    handle: u64,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let enabled =
        !session.paused && !session.output_eof && session.output_total() < session.output_capacity;
    let _ = update_read_filter(kqueue, handle, session, enabled);
    let output_ready = session.output_bytes <= INTERACTIVE_BATCH
        || session.output_bytes >= OUTPUT_BATCH
        || session.output_eof
        || session
            .output_deadline
            .is_some_and(|deadline| deadline <= Instant::now());
    if session.active && output_ready && !session.output.is_empty() && !session.output_notified {
        let bytes = session.pull(OUTPUT_BATCH);
        session.output_notified = true;
        send_notice(notices, Notice::Output { handle, bytes }, counters);
    }
    if session.active
        && session.output_eof
        && session.output.is_empty()
        && session.output_outstanding == 0
        && !session.output_done_notified
    {
        session.output_done_notified = true;
        send_notice(
            notices,
            if session.output_failed {
                Notice::OutputFailed(handle)
            } else {
                Notice::OutputDone(handle)
            },
            counters,
        );
    }
}

fn output_poll_timeout(sessions: &GenerationRegistry<Session>) -> i32 {
    let deadline = sessions
        .handles()
        .into_iter()
        .filter_map(|handle| sessions.get(handle))
        .filter(|session| !session.output_notified && !session.output.is_empty())
        .filter_map(|session| session.output_deadline)
        .min();
    let Some(deadline) = deadline else {
        return -1;
    };
    let now = Instant::now();
    if deadline <= now {
        return 0;
    }
    let micros = deadline.duration_since(now).as_micros();
    micros.div_ceil(1000).min(i32::MAX as u128) as i32
}

fn refresh_due_outputs(
    kqueue: RawFd,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    for handle in sessions.handles() {
        refresh_output(kqueue, handle, notices, sessions, counters);
    }
}

fn close_session(session: &mut Session, broker: &BrokerClient) -> bool {
    if session.close_started {
        return true;
    }
    session.close_started = true;
    if session.exit_status.is_some() {
        return true;
    }
    broker.close_async(session.broker_session).is_ok()
}

fn reap_abandoned(sessions: &mut GenerationRegistry<Session>, broker: &BrokerClient) {
    for handle in sessions.handles() {
        let removable = sessions.get(handle).is_some_and(|session| {
            session.abandoned
                && session.exit_status.is_some()
                && session.output_eof
                && session.output.is_empty()
                && session.output_outstanding == 0
        });
        if removable {
            if let Some(session) = sessions.remove(handle) {
                let _ = broker.release_async(session.broker_session);
            }
        }
    }
}

fn shutdown_all(sessions: &mut GenerationRegistry<Session>, broker: &BrokerClient) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            close_session(session, broker);
        }
    }
    for handle in sessions.handles() {
        if let Some(session) = sessions.remove(handle) {
            let _ = broker.abort_async(session.broker_session);
        }
    }
}

fn fail_all(
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    broker: &BrokerClient,
) {
    for handle in sessions.handles() {
        if let Some(mut session) = sessions.remove(handle) {
            session.close_started = true;
            fail_input_waiters(handle, &mut session, notices, counters);
            let _ = broker.abort_async(session.broker_session);
            send_notice(notices, Notice::BrokerLost(handle), counters);
        }
    }
}

fn drain_wake(fd: RawFd) {
    let mut buffer = [0_u8; 256];
    loop {
        let result = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if result <= 0 {
            return;
        }
    }
}

fn session_size(session: &Session) -> Option<[u32; 4]> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
    (unsafe {
        libc::ioctl(
            session.master.as_raw_fd(),
            libc::TIOCGWINSZ as _,
            size.as_mut_ptr(),
        )
    } == 0)
        .then(|| {
            let size = unsafe { size.assume_init() };
            [
                size.ws_row.into(),
                size.ws_col.into(),
                size.ws_xpixel.into(),
                size.ws_ypixel.into(),
            ]
        })
}

fn session_mode(session: &Session) -> Option<[bool; 3]> {
    let name = session_tty_name(session)?;
    let name = CString::new(name).ok()?;
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return None;
    }
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    (unsafe { libc::tcgetattr(slave.as_raw_fd(), mode.as_mut_ptr()) } == 0).then(|| {
        let flags = unsafe { mode.assume_init() }.c_lflag;
        [
            flags & libc::ICANON != 0,
            flags & libc::ECHO != 0,
            flags & libc::ISIG != 0,
        ]
    })
}

fn session_tty_name(session: &Session) -> Option<Vec<u8>> {
    let mut name = vec![0 as libc::c_char; 1024];
    if unsafe { ptsname_r(session.master.as_raw_fd(), name.as_mut_ptr(), name.len()) } != 0 {
        return None;
    }
    let length = name.iter().position(|byte| *byte == 0)?;
    Some(name[..length].iter().map(|byte| *byte as u8).collect())
}

unsafe extern "C" {
    fn ptsname_r(fd: libc::c_int, buffer: *mut libc::c_char, length: libc::size_t) -> libc::c_int;
}

fn send_notice(notices: &SyncSender<Notice>, notice: Notice, counters: &mut RuntimeCounters) {
    if notices.send(notice).is_ok() {
        counters.notifications += 1;
    }
}

#[cfg(target_os = "macos")]
fn event(
    ident: usize,
    filter: libc::c_short,
    flags: libc::c_ushort,
    fflags: libc::c_uint,
    handle: u64,
) -> libc::kevent {
    libc::kevent {
        ident: ident as libc::uintptr_t,
        filter,
        flags,
        fflags,
        data: 0,
        udata: handle as usize as *mut libc::c_void,
    }
}

#[cfg(target_os = "macos")]
fn submit(kqueue: RawFd, change: &libc::kevent) -> io::Result<()> {
    let result = unsafe { libc::kevent(kqueue, change, 1, ptr::null_mut(), 0, ptr::null()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fail_input_waiters, notify_waiters, Notice, QueuedOutput, RuntimeCounters, Session,
        WaitResult,
    };
    use crate::broker_client::BrokerSession;
    use std::collections::VecDeque;
    use std::fs::File;
    use std::sync::mpsc;

    fn session(input_capacity: usize) -> Session {
        Session::from_broker(
            BrokerSession {
                id: 1,
                pid: 1,
                master: File::open("/dev/null").unwrap().into(),
            },
            input_capacity,
            1024,
        )
    }

    #[test]
    fn retains_every_concurrent_capacity_and_flush_waiter() {
        let mut session = session(4);
        session.input_bytes = 4;
        session.next_sequence = 2;

        assert_eq!(session.wait_capacity(1, 11), WaitResult::Armed);
        assert_eq!(session.wait_capacity(1, 12), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 21), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 22), WaitResult::Armed);

        assert_eq!(session.capacity_waiters.len(), 2);
        assert_eq!(session.flush_waiters.len(), 2);
    }

    #[test]
    fn rejects_an_impossible_capacity_wait_without_arming_it() {
        let mut session = session(4);

        assert_eq!(session.wait_capacity(5, 31), WaitResult::Failed);
        assert!(session.capacity_waiters.is_empty());
    }

    #[test]
    fn notifies_every_ready_concurrent_waiter() {
        let mut session = session(4);
        assert!(session.try_write(vec![1, 2, 3, 4]).is_ok());
        assert_eq!(session.wait_capacity(1, 11), WaitResult::Armed);
        assert_eq!(session.wait_capacity(1, 12), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 21), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 22), WaitResult::Armed);
        session.input_bytes = 0;
        session.flushed_sequence = 1;
        let (sender, receiver) = mpsc::sync_channel(8);
        let mut counters = RuntimeCounters::default();

        notify_waiters(7, &mut session, &sender, &mut counters);

        let mut notices: Vec<_> = receiver.try_iter().collect();
        notices.sort_by_key(|notice| match notice {
            Notice::Capacity { waiter, .. } | Notice::Flush { waiter, .. } => *waiter,
            _ => 0,
        });
        assert_eq!(
            notices,
            vec![
                Notice::Capacity {
                    handle: 7,
                    waiter: 11,
                },
                Notice::Capacity {
                    handle: 7,
                    waiter: 12,
                },
                Notice::Flush {
                    handle: 7,
                    waiter: 21,
                },
                Notice::Flush {
                    handle: 7,
                    waiter: 22,
                },
            ]
        );
    }

    #[test]
    fn terminal_input_failure_fails_every_waiter_and_rejects_future_writes() {
        let mut session = session(4);
        session.input_bytes = 4;
        assert_eq!(session.wait_capacity(1, 11), WaitResult::Armed);
        assert_eq!(session.wait_capacity(1, 12), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 21), WaitResult::Armed);
        assert_eq!(session.wait_flush(1, 22), WaitResult::Armed);
        let (sender, receiver) = mpsc::sync_channel(8);
        let mut counters = RuntimeCounters::default();

        fail_input_waiters(7, &mut session, &sender, &mut counters);

        let mut input_failed = false;
        let mut waiters = Vec::new();
        for notice in receiver.try_iter() {
            match notice {
                Notice::WaitFailed { handle: 7, waiter } => waiters.push(waiter),
                Notice::InputFailed(7) => input_failed = true,
                other => panic!("unexpected notice: {other:?}"),
            }
        }
        waiters.sort_unstable();
        assert_eq!(waiters, vec![11, 12, 21, 22]);
        assert!(input_failed);
        assert!(session.try_write(vec![1]).is_err());
    }

    #[test]
    fn arbitrary_output_chunking_preserves_every_byte_and_credit() {
        let mut session = session(4096);
        let mut expected = Vec::new();
        let mut state = 0x193a_6c8d_e42f_b751_u64;
        for chunk_index in 0..512 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let length = 1 + state as usize % 97;
            let bytes: Vec<_> = (0..length)
                .map(|index| (chunk_index + index) as u8)
                .collect();
            expected.extend_from_slice(&bytes);
            session.output_bytes += bytes.len();
            session.output.push_back(QueuedOutput { bytes, offset: 0 });
        }

        let mut actual = Vec::new();
        while session.output_bytes != 0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let maximum = 1 + state as usize % 193;
            let bytes = session.pull(maximum);
            actual.extend_from_slice(&bytes);
            assert!(session.output_total() <= expected.len());
            assert!(session.credit(bytes.len()));
        }

        assert_eq!(actual, expected);
        assert_eq!(session.output_total(), 0);
        assert!(!session.credit(1));
    }

    #[test]
    fn generated_input_sequences_are_all_or_reject_and_ordered() {
        let mut session = session(257);
        let mut expected = VecDeque::new();
        let mut accepted_sequences = Vec::new();
        let mut state = 0xa841_3f69_7c2d_50be_u64;

        for step in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if state & 3 == 0 && !session.input.is_empty() {
                let queued = session.input.pop_front().unwrap();
                session.input_bytes -= queued.bytes.len();
                assert_eq!(queued.offset, 0);
                assert_eq!(Some(&queued.bytes), expected.pop_front().as_ref());
                continue;
            }

            let length = 1 + state as usize % 64;
            let bytes: Vec<_> = (0..length).map(|index| (step + index) as u8).collect();
            let before = session.input_bytes;
            match session.try_write(bytes.clone()) {
                Ok(sequence) => {
                    assert_eq!(session.input_bytes, before + bytes.len());
                    expected.push_back(bytes);
                    accepted_sequences.push(sequence);
                }
                Err(rejected) => {
                    assert_eq!(rejected, bytes);
                    assert_eq!(session.input_bytes, before);
                }
            }
            assert!(session.input_bytes <= session.input_capacity);
        }

        assert!(accepted_sequences.windows(2).all(|pair| pair[0] < pair[1]));
        for queued in &session.input {
            assert_eq!(Some(&queued.bytes), expected.pop_front().as_ref());
        }
        assert!(expected.is_empty());

        session.close_started = true;
        assert!(session.try_write(vec![1]).is_err());
    }
}
