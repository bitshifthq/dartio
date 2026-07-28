use super::{dup_cloexec, set_cloexec, set_nonblocking, GenerationRegistry};
use crate::broker_client::{BrokerClient, BrokerOwner, BrokerSession, BrokerSpawn};
use crate::oneshot::{self, Sender as ReplySender};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(target_os = "macos")]
use std::ptr;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
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
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    Output { handle: u64, bytes: Vec<u8> },
    InputFailed(u64),
    OutputFailed(u64),
    BrokerLost(u64),
    OutputDone(u64),
    Exit(u64),
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
}

struct QueuedOutput {
    bytes: Vec<u8>,
    offset: usize,
}

pub(crate) struct InputAdmission {
    capacity: usize,
    state: Mutex<InputAdmissionState>,
}

struct InputAdmissionState {
    bytes: usize,
    open: bool,
}

impl InputAdmission {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(InputAdmissionState {
                bytes: 0,
                open: true,
            }),
        }
    }

    fn release(&self, bytes: usize) {
        if let Ok(mut state) = self.state.lock() {
            debug_assert!(bytes <= state.bytes);
            state.bytes = state.bytes.saturating_sub(bytes);
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.open = false;
        }
    }
}

struct Session {
    broker_session: u64,
    pid: libc::pid_t,
    master: OwnedFd,
    admission: Arc<InputAdmission>,
    input_bytes: usize,
    input: VecDeque<QueuedInput>,
    input_failed: bool,
    input_failure_pending: bool,
    input_failure_notified: bool,
    output_capacity: usize,
    output_bytes: usize,
    output: VecDeque<QueuedOutput>,
    output_outstanding: usize,
    output_deadline: Option<Instant>,
    output_done_notified: bool,
    output_failed: bool,
    paused: bool,
    output_eof: bool,
    exit_status: Option<i64>,
    close_started: bool,
    active: bool,
    activation_deadline: Option<Instant>,
    abandoned: bool,
    read_filter_enabled: Option<bool>,
    write_filter_enabled: Option<bool>,
    #[cfg(target_os = "linux")]
    readiness_registered: bool,
}

impl Session {
    fn from_broker(
        broker: BrokerSession,
        admission: Arc<InputAdmission>,
        output_capacity: usize,
    ) -> Self {
        Self {
            broker_session: broker.id,
            pid: broker.pid,
            master: broker.master,
            admission,
            input_bytes: 0,
            input: VecDeque::new(),
            input_failed: false,
            input_failure_pending: false,
            input_failure_notified: false,
            output_capacity,
            output_bytes: 0,
            output: VecDeque::new(),
            output_outstanding: 0,
            output_deadline: None,
            output_done_notified: false,
            output_failed: false,
            paused: true,
            output_eof: false,
            exit_status: None,
            close_started: false,
            active: false,
            activation_deadline: Some(Instant::now() + ACTIVATION_TIMEOUT),
            abandoned: false,
            read_filter_enabled: None,
            write_filter_enabled: None,
            #[cfg(target_os = "linux")]
            readiness_registered: false,
        }
    }

    fn enqueue_write(&mut self, bytes: Vec<u8>) -> Result<(), Vec<u8>> {
        if self.close_started || self.input_failed || bytes.is_empty() {
            return Err(bytes);
        }
        self.input_bytes += bytes.len();
        self.input.push_back(QueuedInput { bytes, offset: 0 });
        Ok(())
    }

    fn pull(&mut self, maximum: usize) -> Vec<u8> {
        let amount = maximum.min(self.output_bytes);
        if self
            .output
            .front()
            .is_some_and(|front| front.offset == 0 && front.bytes.len() == amount)
        {
            let bytes = self.output.pop_front().unwrap().bytes;
            self.output_bytes -= bytes.len();
            self.output_outstanding += bytes.len();
            if self.output.is_empty() {
                self.output_deadline = None;
            }
            return bytes;
        }
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
        bytes
    }

    fn credit(&mut self, bytes: usize) -> bool {
        if bytes > self.output_outstanding {
            return false;
        }
        self.output_outstanding -= bytes;
        true
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
        reply: ReplySender<io::Result<u64>>,
    },
    Activate {
        handle: u64,
        reply: ReplySender<io::Result<()>>,
    },
    Write {
        handle: u64,
        bytes: Vec<u8>,
        admission: Arc<InputAdmission>,
    },
    CreditAsync {
        handle: u64,
        bytes: usize,
    },
    Pause {
        handle: u64,
        paused: bool,
        reply: ReplySender<bool>,
    },
    ExitStatus {
        handle: u64,
        reply: ReplySender<Option<i64>>,
    },
    Pid {
        handle: u64,
        reply: ReplySender<Option<i32>>,
    },
    Size {
        handle: u64,
        reply: ReplySender<Option<[u32; 4]>>,
    },
    Resize {
        handle: u64,
        size: [u32; 4],
        reply: ReplySender<bool>,
    },
    Mode {
        handle: u64,
        reply: ReplySender<Option<[bool; 3]>>,
    },
    TtyName {
        handle: u64,
        reply: ReplySender<Option<Vec<u8>>>,
    },
    Signal {
        handle: u64,
        signal: i32,
        reply: ReplySender<Option<bool>>,
    },
    Close {
        handle: u64,
        reply: ReplySender<bool>,
    },
    Destroy {
        handle: u64,
        reply: ReplySender<bool>,
    },
    Abandon {
        handle: u64,
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
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    notices: Mutex<Option<Receiver<Notice>>>,
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
        let admissions = Arc::new(Mutex::new(HashMap::new()));
        let reactor_admissions = Arc::clone(&admissions);
        let thread = thread::Builder::new()
            .name("ptyx-integrated-reactor".to_owned())
            .spawn(move || {
                reactor(
                    read,
                    reactor_wake,
                    command_receiver,
                    notice_sender,
                    broker_client,
                    reactor_admissions,
                )
            })?;
        Ok(Self {
            commands: command_sender,
            admissions,
            notices: Mutex::new(Some(notice_receiver)),
            wake: WakeWriter(write),
            thread: Mutex::new(Some(thread)),
            broker,
        })
    }

    pub fn take_notifications(&self) -> Option<Receiver<Notice>> {
        self.notices.lock().ok()?.take()
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
        self.request_result(|reply| Command::Activate { handle, reply })?
    }

    pub fn write(&self, handle: u64, bytes: Vec<u8>) -> i64 {
        let admission = match self
            .admissions
            .lock()
            .ok()
            .and_then(|admissions| admissions.get(&handle).cloned())
        {
            Some(admission) => admission,
            None => return -1,
        };
        let length = bytes.len();
        let mut state = match admission.state.lock() {
            Ok(state) => state,
            Err(_) => return -1,
        };
        if !state.open {
            return -1;
        }
        if length == 0 || state.bytes.saturating_add(length) > admission.capacity {
            return 0;
        }
        state.bytes += length;
        let command = Command::Write {
            handle,
            bytes,
            admission: Arc::clone(&admission),
        };
        if self.commands.try_send(command).is_err() {
            state.bytes -= length;
            return 0;
        }
        drop(state);
        self.wake.wake();
        1
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

    pub fn pause(&self, handle: u64, paused: bool) -> bool {
        self.request_result(|reply| Command::Pause {
            handle,
            paused,
            reply,
        })
        .unwrap_or(false)
    }

    pub fn exit_status(&self, handle: u64) -> Option<i64> {
        self.request_result(|reply| Command::ExitStatus { handle, reply })
            .ok()
            .flatten()
    }

    pub fn pid(&self, handle: u64) -> Option<i32> {
        self.request_result(|reply| Command::Pid { handle, reply })
            .ok()
            .flatten()
    }

    pub fn size(&self, handle: u64) -> Option<[u32; 4]> {
        self.request_result(|reply| Command::Size { handle, reply })
            .ok()
            .flatten()
    }

    pub fn resize(&self, handle: u64, size: [u32; 4]) -> bool {
        self.request_result(|reply| Command::Resize {
            handle,
            size,
            reply,
        })
        .unwrap_or(false)
    }

    pub fn mode(&self, handle: u64) -> Option<[bool; 3]> {
        self.request_result(|reply| Command::Mode { handle, reply })
            .ok()
            .flatten()
    }

    pub fn tty_name(&self, handle: u64) -> Option<Vec<u8>> {
        self.request_result(|reply| Command::TtyName { handle, reply })
            .ok()
            .flatten()
    }

    pub fn signal(&self, handle: u64, signal: i32) -> Option<bool> {
        self.request_result(|reply| Command::Signal {
            handle,
            signal,
            reply,
        })
        .ok()
        .flatten()
    }

    pub fn close(&self, handle: u64) -> bool {
        if let Some(admission) = self
            .admissions
            .lock()
            .ok()
            .and_then(|admissions| admissions.get(&handle).cloned())
        {
            admission.close();
        }
        self.request_result(|reply| Command::Close { handle, reply })
            .unwrap_or(false)
    }

    pub fn destroy(&self, handle: u64) -> bool {
        let destroyed = self
            .request_result(|reply| Command::Destroy { handle, reply })
            .unwrap_or(false);
        if destroyed {
            if let Ok(mut admissions) = self.admissions.lock() {
                admissions.remove(&handle);
            }
        }
        destroyed
    }

    pub fn try_abandon(&self, handle: u64) -> bool {
        if let Some(admission) = self
            .admissions
            .lock()
            .ok()
            .and_then(|admissions| admissions.get(&handle).cloned())
        {
            admission.close();
        }
        if self.commands.try_send(Command::Abandon { handle }).is_err() {
            return false;
        }
        self.wake.wake();
        true
    }

    #[cfg(feature = "test-controls")]
    pub fn kill_broker_for_test(&self) {
        self.broker.kill_for_test();
    }

    fn request_result<R>(&self, command: impl FnOnce(ReplySender<R>) -> Command) -> io::Result<R> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(command(sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))?;
        self.wake.wake();
        receiver
            .recv()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))
    }
}

impl Drop for IntegratedRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl IntegratedRuntime {
    pub(crate) fn shutdown(&self) -> bool {
        let _ = self.commands.send(Command::Shutdown);
        self.wake.wake();
        let reactor = self
            .thread
            .lock()
            .ok()
            .and_then(|mut value| value.take())
            .is_none_or(|thread| thread.join().is_ok());
        reactor && self.broker.shutdown()
    }
}

#[cfg(target_os = "macos")]
fn reactor(
    wake: OwnedFd,
    self_wake: OwnedFd,
    commands: Receiver<Command>,
    notices: SyncSender<Notice>,
    broker: BrokerClient,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    let mut sessions: GenerationRegistry<Session> = GenerationRegistry::new();
    let mut pending_broker_exits = HashMap::new();
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
                &mut pending_broker_exits,
                &mut counters,
                &broker,
                &admissions,
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
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
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
    let mut pending_broker_exits = HashMap::new();
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
                    &mut pending_broker_exits,
                    &mut counters,
                    &broker,
                    &admissions,
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

#[allow(clippy::too_many_arguments)]
fn process_commands(
    kqueue: RawFd,
    commands: &Receiver<Command>,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    pending_broker_exits: &mut HashMap<u64, i32>,
    counters: &mut RuntimeCounters,
    broker_client: &BrokerClient,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
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
                let admission = Arc::new(InputAdmission::new(input_capacity));
                let mut session =
                    Session::from_broker(broker, Arc::clone(&admission), output_capacity);
                session.exit_status = pending_broker_exits
                    .remove(&session.broker_session)
                    .map(i64::from);
                let handle = sessions.insert(session);
                if let Ok(mut values) = admissions.lock() {
                    values.insert(handle, admission);
                }
                let result = Ok(handle);
                let _ = reply.send(result);
            }
            Command::Activate { handle, reply } => {
                let result = if let Some(session) = sessions.get_mut(handle) {
                    if session.abandoned || session.close_started {
                        Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "session activation was abandoned",
                        ))
                    } else {
                        register_session(kqueue, handle, session)
                    }
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "stale session"))
                };
                if result.is_ok() {
                    if let Some(session) = sessions.get_mut(handle) {
                        session.active = true;
                        session.activation_deadline = None;
                        if session.input_failure_pending {
                            notify_input_failure(handle, session, notices, counters);
                        }
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if sessions
                        .get(handle)
                        .is_some_and(|session| session.exit_status.is_some())
                    {
                        send_notice(notices, Notice::Exit(handle), counters);
                    }
                    refresh_output(kqueue, handle, notices, sessions, counters);
                } else if let Some(session) = sessions.get_mut(handle) {
                    session.abandoned = true;
                    session.activation_deadline = None;
                    let _ = close_session(session, broker_client);
                }
                let _ = reply.send(result);
            }
            Command::Write {
                handle,
                bytes,
                admission,
            } => {
                let length = bytes.len();
                let accepted = if let Some(session) = sessions.get_mut(handle) {
                    let accepted = session.enqueue_write(bytes).is_ok();
                    if !accepted {
                        admission.release(length);
                        notify_input_failure(handle, session, notices, counters);
                    }
                    accepted
                } else {
                    admission.release(length);
                    false
                };
                if accepted {
                    if let Some(session) = sessions.get_mut(handle).filter(|session| session.active)
                    {
                        let _ = update_write_filter(kqueue, handle, session, true);
                    }
                }
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
            Command::Close { handle, reply } => {
                let closed = sessions.get_mut(handle).is_some_and(|session| {
                    fail_input(handle, session, notices, counters);
                    close_session(session, broker_client)
                });
                let _ = reply.send(closed);
            }
            Command::Destroy { handle, reply } => {
                let removable = sessions
                    .get(handle)
                    .is_some_and(|session| session.exit_status.is_some());
                let released = removable
                    && sessions.get(handle).is_some_and(|session| {
                        broker_client.release_async(session.broker_session).is_ok()
                    });
                let removed = released && sessions.remove(handle).is_some();
                let _ = reply.send(removed);
            }
            Command::Abandon { handle } => {
                if let Some(session) = sessions.get_mut(handle) {
                    session.abandoned = true;
                    session.active = false;
                    session.paused = false;
                    session.admission.close();
                    session.admission.release(session.input_bytes);
                    session.input.clear();
                    session.input_bytes = 0;
                    session.output.clear();
                    session.output_bytes = 0;
                    session.output_outstanding = 0;
                    let _ = close_session(session, broker_client);
                    let _ = update_read_filter(kqueue, handle, session, true);
                    let _ = update_write_filter(kqueue, handle, session, false);
                }
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
                            fail_input(handle, session, notices, counters);
                        }
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if sessions.get(handle).is_some_and(|session| session.active) {
                        send_notice(notices, Notice::Exit(handle), counters);
                    }
                } else {
                    pending_broker_exits.insert(broker_session, status);
                }
            }
            Command::BrokerLost => {
                pending_broker_exits.clear();
                fail_all(notices, sessions, counters, broker_client);
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
        let mut buffer = MaybeUninit::<[u8; 64 * 1024]>::uninit();
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
                // `read` initialized exactly this prefix.
                bytes: unsafe {
                    std::slice::from_raw_parts(buffer.as_ptr().cast(), amount).to_vec()
                },
                offset: 0,
            });
            continue;
        }
        if result == 0 {
            session.output_eof = true;
            fail_input(handle, session, notices, counters);
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
            fail_input(handle, session, notices, counters);
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
            front.offset += amount;
            session.input_bytes -= amount;
            session.admission.release(amount);
            if front.offset == front.bytes.len() {
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
            fail_input(handle, session, notices, counters);
        }
        break;
    }
    if session.input.is_empty() {
        let _ = update_write_filter(kqueue, handle, session, false);
    }
}

fn fail_input(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    let accepted_input_pending = !session.input.is_empty();
    session.input_failed = true;
    session.admission.close();
    session.admission.release(session.input_bytes);
    session.input.clear();
    session.input_bytes = 0;
    if accepted_input_pending {
        notify_input_failure(handle, session, notices, counters);
    }
}

fn notify_input_failure(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    session.input_failure_pending = true;
    if !session.input_failure_notified {
        session.input_failure_notified = true;
        send_notice(notices, Notice::InputFailed(handle), counters);
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
    if session.active && output_ready && !session.output.is_empty() {
        let bytes = session.pull(OUTPUT_BATCH);
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
    let output_deadline = sessions
        .handles()
        .into_iter()
        .filter_map(|handle| sessions.get(handle))
        .filter(|session| !session.output.is_empty())
        .filter_map(|session| session.output_deadline)
        .min();
    let activation_deadline = sessions
        .handles()
        .into_iter()
        .filter_map(|handle| sessions.get(handle))
        .filter(|session| !session.active && !session.abandoned)
        .filter_map(|session| session.activation_deadline)
        .min();
    let deadline = match (output_deadline, activation_deadline) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
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
    if session.exit_status.is_some() && session.output_eof {
        session.admission.close();
        session.close_started = true;
        return true;
    }
    if broker.close_async(session.broker_session).is_err() {
        return false;
    }
    session.admission.close();
    session.close_started = true;
    true
}

fn reap_abandoned(sessions: &mut GenerationRegistry<Session>, broker: &BrokerClient) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            if !session.active
                && !session.abandoned
                && session
                    .activation_deadline
                    .is_some_and(|deadline| deadline <= Instant::now())
            {
                session.abandoned = true;
                session.activation_deadline = None;
                session.admission.close();
                session.admission.release(session.input_bytes);
                session.input.clear();
                session.input_bytes = 0;
                session.output.clear();
                session.output_bytes = 0;
                session.output_outstanding = 0;
            }
            if session.abandoned && !session.close_started {
                let _ = close_session(session, broker);
            }
        }
        let removable = sessions.get(handle).is_some_and(|session| {
            session.abandoned
                && session.exit_status.is_some()
                && session.output_eof
                && session.output.is_empty()
                && session.output_outstanding == 0
        });
        if removable {
            let released = sessions
                .get(handle)
                .is_some_and(|session| broker.release_async(session.broker_session).is_ok());
            if released {
                let _ = sessions.remove(handle);
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
            fail_input(handle, &mut session, notices, counters);
            force_terminal_group(session.master.as_raw_fd(), session.pid);
            let _ = broker.abort_async(session.broker_session);
            send_notice(notices, Notice::BrokerLost(handle), counters);
        }
    }
}

fn force_terminal_group(master: RawFd, session_leader: libc::pid_t) {
    // TIOCSIG is bound to this PTY object rather than a recyclable numeric
    // process-group identifier. Linux restricts it to job-control signals;
    // Darwin accepts SIGKILL.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::ioctl(master, libc::TIOCSIG, libc::SIGQUIT);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::ioctl(master, libc::TIOCSIG.into(), libc::SIGKILL);
    }
    let signal = libc::SIGKILL;
    let foreground = unsafe { libc::tcgetpgrp(master) };
    if foreground > 0 {
        unsafe {
            libc::kill(-foreground, signal);
        }
    }
    // The controlling terminal retains the OS session identity. Only use the
    // cached leader/group number after this object-bound identity check.
    if unsafe { libc::tcgetsid(master) } == session_leader {
        unsafe {
            libc::kill(-session_leader, signal);
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
    Some(
        name[..length]
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .collect(),
    )
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
        fail_input, notify_input_failure, InputAdmission, Notice, QueuedOutput, RuntimeCounters,
        Session, OUTPUT_BATCH,
    };
    use crate::broker_client::BrokerSession;
    use std::collections::VecDeque;
    use std::fs::File;
    use std::sync::{mpsc, Arc};

    fn session(input_capacity: usize) -> Session {
        Session::from_broker(
            BrokerSession {
                id: 1,
                pid: 1,
                master: File::open("/dev/null").unwrap().into(),
            },
            Arc::new(InputAdmission::new(input_capacity)),
            1024,
        )
    }

    #[test]
    fn terminal_input_failure_rejects_future_writes() {
        let mut session = session(4);
        assert!(session.enqueue_write(vec![1, 2, 3, 4]).is_ok());
        session.input_bytes = 4;
        session.admission.state.lock().unwrap().bytes = 4;
        let (sender, receiver) = mpsc::sync_channel(8);
        let mut counters = RuntimeCounters::default();

        fail_input(7, &mut session, &sender, &mut counters);

        assert_eq!(receiver.try_recv(), Ok(Notice::InputFailed(7)));
        assert!(session.enqueue_write(vec![1]).is_err());
    }

    #[test]
    fn admitted_write_rejected_during_failure_is_reported_once() {
        let mut session = session(4);
        session.active = true;
        session.admission.state.lock().unwrap().bytes = 1;
        let (sender, receiver) = mpsc::sync_channel(8);
        let mut counters = RuntimeCounters::default();

        fail_input(7, &mut session, &sender, &mut counters);
        assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert!(session.enqueue_write(vec![1]).is_err());
        session.admission.release(1);
        notify_input_failure(7, &mut session, &sender, &mut counters);
        notify_input_failure(7, &mut session, &sender, &mut counters);

        assert_eq!(receiver.try_recv(), Ok(Notice::InputFailed(7)));
        assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
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
        let mut outstanding = Vec::new();
        while session.output_bytes != 0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let maximum = 1 + state as usize % 193;
            let bytes = session.pull(maximum);
            outstanding.push(bytes.len());
            actual.extend_from_slice(&bytes);
            assert!(session.output_total() <= expected.len());
        }
        assert_eq!(session.output_outstanding, expected.len());
        for bytes in outstanding {
            assert!(session.credit(bytes));
        }

        assert_eq!(actual, expected);
        assert_eq!(session.output_total(), 0);
        assert!(!session.credit(1));
    }

    #[test]
    fn complete_output_chunk_transfers_ownership_without_copying() {
        let mut session = session(4096);
        let bytes = vec![0x5a; OUTPUT_BATCH];
        let pointer = bytes.as_ptr();
        session.output_bytes = bytes.len();
        session.output.push_back(QueuedOutput { bytes, offset: 0 });

        let pulled = session.pull(OUTPUT_BATCH);

        assert_eq!(pulled.as_ptr(), pointer);
        assert_eq!(session.output_bytes, 0);
        assert_eq!(session.output_outstanding, OUTPUT_BATCH);
    }

    #[test]
    fn generated_input_is_all_or_reject_and_ordered() {
        let mut session = session(257);
        let mut expected = VecDeque::new();
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
            let result = if before + bytes.len() <= session.admission.capacity {
                session.enqueue_write(bytes.clone())
            } else {
                Err(bytes.clone())
            };
            match result {
                Ok(()) => {
                    assert_eq!(session.input_bytes, before + bytes.len());
                    expected.push_back(bytes);
                }
                Err(rejected) => {
                    assert_eq!(rejected, bytes);
                    assert_eq!(session.input_bytes, before);
                }
            }
            assert!(session.input_bytes <= session.admission.capacity);
        }

        for queued in &session.input {
            assert_eq!(Some(&queued.bytes), expected.pop_front().as_ref());
        }
        assert!(expected.is_empty());

        session.close_started = true;
        assert!(session.enqueue_write(vec![1]).is_err());
    }
}
