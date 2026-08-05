use super::{dup_cloexec, set_cloexec, set_nonblocking, GenerationRegistry};
use crate::engine::broker_client::{BrokerClient, BrokerOwner, BrokerSession};
use crate::engine::control::{fail_wake_socket, wake_socket, Control, ControlQueue, WakeGate};
use crate::engine::event::{self, Receiver as EventReceiver, Sender as EventSender};
use crate::engine::oneshot::{self, Sender as ReplySender};
#[cfg(any(feature = "__private_adapter", test))]
use crate::engine::session::AdmissionResult;
use crate::engine::session::{validate_capacities, InputAdmission, SessionCore};
use crate::engine::spawn::BrokerSpawn;
#[cfg(feature = "__private_adapter")]
use crate::engine::Failure;
use crate::engine::{CloseResult, Completion, Notice};
use crate::error::{FailureKind, Operation, OperationError, WriteError, WriteErrorKind};
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::io;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::ptr;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BYTE_QUANTUM: usize = 64 * 1024;
const SYSCALL_QUANTUM: usize = 4;
const INTERACTIVE_BATCH: usize = 256;
const OUTPUT_BATCH: usize = 128 * 1024;
const OUTPUT_DELAY: Duration = Duration::from_millis(1);
const COMMAND_QUANTUM: usize = 64;
const COMMAND_CAPACITY: usize = 1024;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
const MODE_POLL_MIN: Duration = Duration::from_millis(40);
const MODE_POLL_MAX: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseState {
    Needed,
    Pending,
    Complete,
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
    pub notification_failures: u64,
}

struct Session {
    core: SessionCore,
    broker_session: u64,
    pid: libc::pid_t,
    master: OwnedFd,
    read_filter_enabled: Option<bool>,
    write_filter_enabled: Option<bool>,
    mode_deadline: Option<Instant>,
    mode_interval: Duration,
    observed_mode: Option<[bool; 3]>,
    graceful_close_timeout: Duration,
    close_deadline: Option<Instant>,
    close_escalated: bool,
    release: ReleaseState,
    #[cfg(target_os = "linux")]
    readiness_registered: bool,
}

impl Session {
    fn from_broker(
        broker: BrokerSession,
        admission: Arc<InputAdmission>,
        output_capacity: usize,
        graceful_close_timeout: Duration,
    ) -> Self {
        let mut core = SessionCore::new(admission, output_capacity);
        core.activation_deadline = Some(Instant::now() + ACTIVATION_TIMEOUT);
        Self {
            core,
            broker_session: broker.id,
            pid: broker.pid,
            master: broker.master,
            read_filter_enabled: None,
            write_filter_enabled: None,
            mode_deadline: None,
            mode_interval: MODE_POLL_MIN,
            observed_mode: None,
            graceful_close_timeout,
            close_deadline: None,
            close_escalated: false,
            release: ReleaseState::Needed,
            #[cfg(target_os = "linux")]
            readiness_registered: false,
        }
    }

    fn record_release_result(&mut self, succeeded: bool) {
        self.release = ReleaseState::Complete;
        if !succeeded && self.cleanup_failure.is_none() {
            self.cleanup_failure = Some(OperationError::new(
                Operation::Close,
                FailureKind::InfrastructureLost,
                None,
            ));
        }
    }

    fn completed_close_result(&self) -> Option<CloseResult> {
        (self.release == ReleaseState::Complete).then_some(CloseResult {
            input_failure: self.input_failure,
            output_failure: self.output_failure,
            cleanup_failure: self.cleanup_failure,
        })
    }
}

impl Deref for Session {
    type Target = SessionCore;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

pub(crate) enum Command {
    Add {
        broker: BrokerSession,
        input_capacity: usize,
        output_capacity: usize,
        graceful_close_timeout: Duration,
        reply: ReplySender<io::Result<u64>>,
    },
    Activate {
        handle: u64,
        reply: ReplySender<io::Result<()>>,
    },
    Write {
        handle: u64,
        bytes: Bytes,
        admission: Arc<InputAdmission>,
    },
    CancelOutput {
        handle: u64,
        reply: ReplySender<Result<(), OperationError>>,
    },
    Size {
        handle: u64,
        reply: ReplySender<Result<[u32; 4], OperationError>>,
    },
    ProcessId {
        handle: u64,
        reply: ReplySender<Result<i64, OperationError>>,
    },
    TerminalMode {
        handle: u64,
        reply: ReplySender<Result<[bool; 3], OperationError>>,
    },
    TerminalName {
        handle: u64,
        reply: ReplySender<Result<Vec<u8>, OperationError>>,
    },
    Resize {
        handle: u64,
        size: [u32; 4],
        reply: ReplySender<Result<(), OperationError>>,
    },
    ObserveMode {
        handle: u64,
        observe: bool,
        reply: ReplySender<Result<(), OperationError>>,
    },
    Signal {
        handle: u64,
        signal: i32,
        reply: ReplySender<Result<bool, OperationError>>,
    },
    CloseStart {
        handle: u64,
        completion: ReplySender<CloseResult>,
        reply: ReplySender<bool>,
    },
    BrokerExit {
        broker_session: u64,
        status: i32,
    },
    BrokerLost,
    GracefulSignalResult {
        handle: u64,
        result: Option<bool>,
    },
    CloseResult {
        handle: u64,
        succeeded: bool,
    },
    ForceCloseResult {
        handle: u64,
        succeeded: bool,
    },
    ReleaseResult {
        handle: u64,
        succeeded: bool,
    },
    Shutdown,
}

#[derive(Clone)]
struct WakeWriter {
    fd: Arc<OwnedFd>,
    gate: Arc<WakeGate>,
}

struct ReactorWake {
    reader: OwnedFd,
    writer: OwnedFd,
    gate: Arc<WakeGate>,
}

impl WakeWriter {
    fn wake(&self) {
        if self.gate.request() && wake_socket(self.fd.as_raw_fd()).is_err() {
            // A fatal send cannot leave already-committed commands stranded.
            // Closing the wake direction forces the reactor to fail its
            // pending operations and drop the command receiver.
            self.gate.clear();
            fail_wake_socket(self.fd.as_raw_fd());
        }
    }
}

struct SpawnTask {
    config: BrokerSpawn,
    input_capacity: usize,
    output_capacity: usize,
    target: SpawnTarget,
}

enum SpawnTarget {
    Completion(ReplySender<io::Result<u64>>),
    #[cfg(feature = "__private_adapter")]
    Notice {
        request: u64,
    },
}

struct SpawnPool {
    sender: Mutex<Option<SyncSender<SpawnTask>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SpawnPool {
    fn new(
        broker: BrokerClient,
        commands: SyncSender<Command>,
        wake: WakeWriter,
        controls: Arc<ControlQueue>,
        notices: EventSender<Notice>,
    ) -> io::Result<Self> {
        #[cfg(not(feature = "__private_adapter"))]
        let _ = &notices;
        let (sender, receiver) = mpsc::sync_channel::<SpawnTask>(64);
        let thread = thread::Builder::new()
            .name("ptyx-spawn".to_owned())
            .spawn(move || {
                while let Ok(task) = receiver.recv() {
                    let result = stage_spawn(
                        &broker,
                        &commands,
                        &wake,
                        task.config,
                        task.input_capacity,
                        task.output_capacity,
                    );
                    let staged = result.as_ref().ok().copied();
                    let delivered = match task.target {
                        SpawnTarget::Completion(reply) => reply.send(result),
                        #[cfg(feature = "__private_adapter")]
                        SpawnTarget::Notice { request } => {
                            let notice = match result {
                                Ok(handle) => Notice::SpawnReady { request, handle },
                                Err(error) => Notice::SpawnFailed {
                                    request,
                                    failure: Failure::from(&error),
                                },
                            };
                            notices.send(notice.handle(), notice).is_ok()
                        }
                    };
                    if !delivered {
                        if let Some(handle) = staged {
                            controls.push(Control::Abandon { handle });
                            wake.wake();
                        }
                    }
                }
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
        })
    }

    fn submit(&self, task: SpawnTask) -> io::Result<()> {
        let sender = self
            .sender
            .lock()
            .map_err(|_| io::Error::other("spawn queue lock poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "runtime is shutting down"))?;
        sender.try_send(task).map_err(|error| match error {
            TrySendError::Full(_) => {
                io::Error::new(io::ErrorKind::WouldBlock, "spawn queue is full")
            }
            TrySendError::Disconnected(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "spawn worker stopped")
            }
        })
    }

    fn shutdown(&self) -> bool {
        if let Ok(mut sender) = self.sender.lock() {
            sender.take();
        }
        self.thread
            .lock()
            .ok()
            .and_then(|mut thread| thread.take())
            .is_none_or(|thread| thread.join().is_ok())
    }
}

pub struct IntegratedRuntime {
    commands: SyncSender<Command>,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    notices: Mutex<Option<EventReceiver<Notice>>>,
    wake: WakeWriter,
    controls: Arc<ControlQueue>,
    spawn_pool: SpawnPool,
    thread: Mutex<Option<JoinHandle<()>>>,
    broker: BrokerOwner,
}

impl IntegratedRuntime {
    pub fn try_new(broker_path: &Path) -> io::Result<Self> {
        let mut sockets = [-1; 2];
        #[cfg(target_os = "macos")]
        let socket_type = libc::SOCK_STREAM;
        #[cfg(target_os = "linux")]
        let socket_type = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
        if unsafe { libc::socketpair(libc::AF_UNIX, socket_type, 0, sockets.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
        set_cloexec(read.as_raw_fd())?;
        set_cloexec(write.as_raw_fd())?;
        set_nonblocking(read.as_raw_fd())?;
        set_nonblocking(write.as_raw_fd())?;
        let reactor_wake = dup_cloexec(write.as_raw_fd())?;
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (notice_sender, notice_receiver) = event::channel();
        let broker_reactor_wake = dup_cloexec(write.as_raw_fd())?;
        let broker_path = CString::new(broker_path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "NUL broker path"))?;
        let broker =
            BrokerOwner::launch(&broker_path, command_sender.clone(), broker_reactor_wake)?;
        let broker_client = broker.client();
        let wake_gate = Arc::new(WakeGate::new());
        let wake = WakeWriter {
            fd: Arc::new(write),
            gate: Arc::clone(&wake_gate),
        };
        let admissions = Arc::new(Mutex::new(HashMap::new()));
        let controls = Arc::new(ControlQueue::new());
        let spawn_pool = SpawnPool::new(
            broker_client.clone(),
            command_sender.clone(),
            wake.clone(),
            Arc::clone(&controls),
            notice_sender.clone(),
        )?;
        let reactor_admissions = Arc::clone(&admissions);
        let reactor_controls = Arc::clone(&controls);
        let thread = thread::Builder::new()
            .name("ptyx-integrated-reactor".to_owned())
            .spawn(move || {
                reactor(
                    ReactorWake {
                        reader: read,
                        writer: reactor_wake,
                        gate: wake_gate,
                    },
                    command_receiver,
                    notice_sender,
                    broker_client,
                    reactor_admissions,
                    reactor_controls,
                )
            })?;
        Ok(Self {
            commands: command_sender,
            admissions,
            notices: Mutex::new(Some(notice_receiver)),
            wake,
            controls,
            spawn_pool,
            thread: Mutex::new(Some(thread)),
            broker,
        })
    }

    pub fn take_notifications(&self) -> Option<EventReceiver<Notice>> {
        self.notices.lock().ok()?.take()
    }

    pub fn spawn_start(
        &self,
        config: BrokerSpawn,
        input_capacity: usize,
        output_capacity: usize,
    ) -> io::Result<Completion<io::Result<u64>>> {
        validate_capacities(input_capacity, output_capacity)?;
        config.validate()?;
        let (reply, receiver) = oneshot::channel();
        self.spawn_pool.submit(SpawnTask {
            config,
            input_capacity,
            output_capacity,
            target: SpawnTarget::Completion(reply),
        })?;
        Ok(Completion::new(receiver))
    }

    #[cfg(feature = "__private_adapter")]
    pub fn spawn_start_notified(
        &self,
        request: u64,
        config: BrokerSpawn,
        input_capacity: usize,
        output_capacity: usize,
    ) -> io::Result<()> {
        validate_capacities(input_capacity, output_capacity)?;
        config.validate()?;
        self.spawn_pool.submit(SpawnTask {
            config,
            input_capacity,
            output_capacity,
            target: SpawnTarget::Notice { request },
        })
    }

    pub fn activate(&self, handle: u64) -> bool {
        self.activate_result(handle).is_ok()
    }

    fn activate_result(&self, handle: u64) -> io::Result<()> {
        self.request_result(|reply| Command::Activate { handle, reply })?
    }

    pub(crate) fn write(&self, handle: u64, bytes: Bytes) -> Result<(), WriteError> {
        let admission = match self.admissions.lock() {
            Ok(admissions) => match admissions.get(&handle).cloned() {
                Some(admission) => admission,
                None => {
                    return Err(WriteError::new(WriteErrorKind::Closed, bytes, None));
                }
            },
            Err(_) => {
                return Err(WriteError::new(
                    WriteErrorKind::Infrastructure,
                    bytes,
                    Some(OperationError::new(
                        Operation::Write,
                        FailureKind::InfrastructureLost,
                        None,
                    )),
                ));
            }
        };
        admit_owned_write(&self.commands, handle, bytes, admission)?;
        self.wake.wake();
        Ok(())
    }

    #[cfg(feature = "__private_adapter")]
    pub fn write_copy(&self, handle: u64, bytes: &[u8]) -> Result<(), OperationError> {
        let admission = match self.admissions.try_lock() {
            Ok(admissions) => match admissions.get(&handle).cloned() {
                Some(admission) => admission,
                None => {
                    return Err(OperationError::new(
                        Operation::Write,
                        FailureKind::Closed,
                        None,
                    ))
                }
            },
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(OperationError::new(
                    Operation::Write,
                    FailureKind::Backpressure,
                    None,
                ));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(OperationError::new(
                    Operation::Write,
                    FailureKind::InfrastructureLost,
                    None,
                ));
            }
        };
        let result = admit_write_with(
            &self.commands,
            handle,
            bytes.len(),
            Arc::clone(&admission),
            || Bytes::copy_from_slice(bytes),
        );
        match result {
            AdmissionResult::Accepted => {
                self.wake.wake();
                Ok(())
            }
            AdmissionResult::Backpressure => Err(OperationError::new(
                Operation::Write,
                FailureKind::Backpressure,
                None,
            )),
            AdmissionResult::Closed => Err(admission
                .state
                .lock()
                .ok()
                .and_then(|state| state.failure)
                .unwrap_or_else(|| {
                    OperationError::new(Operation::Write, FailureKind::Closed, None)
                })),
            AdmissionResult::Infrastructure => Err(OperationError::new(
                Operation::Write,
                FailureKind::InfrastructureLost,
                None,
            )),
        }
    }

    pub fn credit_async(&self, handle: u64, bytes: usize) -> bool {
        let queued = self.controls.push(Control::Credit { handle, bytes });
        if queued {
            self.wake.wake();
        }
        queued
    }

    pub fn cancel_output(&self, handle: u64) -> Result<(), OperationError> {
        self.request_operation(Operation::Output, |reply| Command::CancelOutput {
            handle,
            reply,
        })?
    }

    pub fn size(&self, handle: u64) -> Result<[u32; 4], OperationError> {
        self.request_operation(Operation::Size, |reply| Command::Size { handle, reply })?
    }

    pub fn process_id(&self, handle: u64) -> Result<i64, OperationError> {
        self.request_operation(Operation::ProcessId, |reply| Command::ProcessId {
            handle,
            reply,
        })?
    }

    pub fn terminal_mode(&self, handle: u64) -> Result<[bool; 3], OperationError> {
        self.request_operation(Operation::TerminalMode, |reply| Command::TerminalMode {
            handle,
            reply,
        })?
    }

    pub fn terminal_name(&self, handle: u64) -> Result<Vec<u8>, OperationError> {
        self.request_operation(Operation::TerminalName, |reply| Command::TerminalName {
            handle,
            reply,
        })?
    }

    pub fn resize(&self, handle: u64, size: [u32; 4]) -> Result<(), OperationError> {
        self.request_operation(Operation::Resize, |reply| Command::Resize {
            handle,
            size,
            reply,
        })?
    }

    pub fn observe_mode(&self, handle: u64, observe: bool) -> Result<(), OperationError> {
        self.request_operation(Operation::TerminalMode, |reply| Command::ObserveMode {
            handle,
            observe,
            reply,
        })?
    }

    pub fn signal(&self, handle: u64, signal: i32) -> Result<bool, OperationError> {
        self.request_operation(Operation::Terminate, |reply| Command::Signal {
            handle,
            signal,
            reply,
        })?
    }

    pub fn close_start(&self, handle: u64) -> io::Result<Completion<CloseResult>> {
        if let Some(admission) = self
            .admissions
            .lock()
            .ok()
            .and_then(|admissions| admissions.get(&handle).cloned())
        {
            admission.close();
        }
        let (completion, receiver) = oneshot::channel();
        let accepted = self.request_result(|reply| Command::CloseStart {
            handle,
            completion,
            reply,
        })?;
        if !accepted {
            return Err(io::Error::new(io::ErrorKind::NotFound, "stale session"));
        }
        Ok(Completion::new(receiver))
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
        let queued = self.controls.push(Control::Abandon { handle });
        if queued {
            self.wake.wake();
        }
        queued
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

    fn request_operation<R>(
        &self,
        operation: Operation,
        command: impl FnOnce(ReplySender<R>) -> Command,
    ) -> Result<R, OperationError> {
        self.request_result(command).map_err(|error| {
            OperationError::new(
                operation,
                FailureKind::InfrastructureLost,
                error.raw_os_error(),
            )
        })
    }
}

impl Drop for IntegratedRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl IntegratedRuntime {
    pub fn shutdown(&self) -> bool {
        let spawn = self.spawn_pool.shutdown();
        let reactor = self.thread.lock().ok().and_then(|mut value| value.take());
        let Some(reactor) = reactor else {
            return spawn && self.broker.shutdown();
        };
        let _ = self.commands.send(Command::Shutdown);
        self.wake.wake();
        let reactor = reactor.join().is_ok();
        spawn && reactor && self.broker.shutdown()
    }
}

fn stage_spawn(
    broker: &BrokerClient,
    commands: &SyncSender<Command>,
    wake: &WakeWriter,
    config: BrokerSpawn,
    input_capacity: usize,
    output_capacity: usize,
) -> io::Result<u64> {
    let graceful_close_timeout = config.graceful_close_timeout;
    let spawned = broker.spawn(config)?;
    let broker_session = spawned.id;
    let (reply, receiver) = oneshot::channel();
    if commands
        .send(Command::Add {
            broker: spawned,
            input_capacity,
            output_capacity,
            graceful_close_timeout,
            reply,
        })
        .is_err()
    {
        let _ = broker.abort(broker_session);
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "ptyx reactor stopped",
        ));
    }
    wake.wake();
    match receiver.recv() {
        Some(result) => result,
        None => {
            let _ = broker.abort(broker_session);
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ptyx reactor stopped",
            ))
        }
    }
}

#[cfg(target_os = "macos")]
fn reactor(
    wake: ReactorWake,
    commands: Receiver<Command>,
    notices: EventSender<Notice>,
    broker: BrokerClient,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    controls: Arc<ControlQueue>,
) {
    let mut sessions: GenerationRegistry<Session> = GenerationRegistry::new();
    let mut pending_broker_exits = HashMap::new();
    let mut counters = RuntimeCounters::default();
    let mut control_scratch = VecDeque::new();
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
            fd: wake.reader.as_raw_fd(),
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
            fail_all(&notices, &mut sessions, &mut counters, &broker, &admissions);
            return;
        }
        counters.reactor_wakeups += 1;
        counters.reactor_events += poll_descriptors
            .iter()
            .filter(|descriptor| descriptor.revents != 0)
            .count() as u64;
        let mut shutdown = false;
        if poll_descriptors[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            fail_all(&notices, &mut sessions, &mut counters, &broker, &admissions);
            return;
        }
        if poll_descriptors[0].revents & libc::POLLIN != 0 {
            drain_wake(wake.reader.as_raw_fd());
            wake.gate.clear();
            counters.command_wakeups += 1;
            process_controls(
                -1,
                &controls,
                &mut control_scratch,
                &notices,
                &mut sessions,
                &mut counters,
                &broker,
            );
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
                let _ = wake_socket(wake.writer.as_raw_fd());
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
        refresh_modes(&notices, &mut sessions, &mut counters);
        escalate_due_closes(&mut sessions, &broker);
        reap_closed(&notices, &mut sessions, &mut counters, &broker, &admissions);
        reap_abandoned(&notices, &mut sessions, &broker, &admissions);
        if shutdown {
            shutdown_all(&mut sessions, &broker, &admissions);
            return;
        }
    }
}

#[cfg(target_os = "linux")]
fn reactor(
    wake: ReactorWake,
    commands: Receiver<Command>,
    notices: EventSender<Notice>,
    broker: BrokerClient,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    controls: Arc<ControlQueue>,
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
            wake.reader.as_raw_fd(),
            &mut wake_event,
        )
    } < 0
    {
        return;
    }

    let mut sessions: GenerationRegistry<Session> = GenerationRegistry::new();
    let mut pending_broker_exits = HashMap::new();
    let mut counters = RuntimeCounters::default();
    let mut control_scratch = VecDeque::new();
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
            fail_all(&notices, &mut sessions, &mut counters, &broker, &admissions);
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
                if event.events & (libc::EPOLLHUP | libc::EPOLLERR) as u32 != 0 {
                    fail_all(&notices, &mut sessions, &mut counters, &broker, &admissions);
                    return;
                }
                drain_wake(wake.reader.as_raw_fd());
                wake.gate.clear();
                counters.command_wakeups += 1;
                process_controls(
                    epoll.as_raw_fd(),
                    &controls,
                    &mut control_scratch,
                    &notices,
                    &mut sessions,
                    &mut counters,
                    &broker,
                );
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
                    let _ = wake_socket(wake.writer.as_raw_fd());
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
        refresh_modes(&notices, &mut sessions, &mut counters);
        escalate_due_closes(&mut sessions, &broker);
        reap_closed(&notices, &mut sessions, &mut counters, &broker, &admissions);
        reap_abandoned(&notices, &mut sessions, &broker, &admissions);
        if shutdown {
            shutdown_all(&mut sessions, &broker, &admissions);
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_commands(
    kqueue: RawFd,
    commands: &Receiver<Command>,
    notices: &EventSender<Notice>,
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
                graceful_close_timeout,
                reply,
            } => {
                let admission = Arc::new(InputAdmission::new(input_capacity));
                let mut session = Session::from_broker(
                    broker,
                    Arc::clone(&admission),
                    output_capacity,
                    graceful_close_timeout,
                );
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
                        session.paused = false;
                        session.activation_deadline = None;
                        if session.input_failure.is_some() {
                            notify_input_failure(handle, session, notices, counters);
                        }
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if let Some(status) =
                        sessions.get(handle).and_then(|session| session.exit_status)
                    {
                        if !send_notice(notices, Notice::Exit { handle, status }, counters) {
                            if let Some(session) = sessions.get_mut(handle) {
                                mark_notice_failure(session);
                            }
                        }
                    }
                    refresh_output(kqueue, handle, notices, sessions, counters);
                } else if let Some(session) = sessions.get_mut(handle) {
                    session.abandoned = true;
                    session.activation_deadline = None;
                    let _ = close_session(handle, session, broker_client);
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
                        admission.release(length, 1);
                        notify_input_failure(handle, session, notices, counters);
                    }
                    accepted
                } else {
                    admission.release(length, 1);
                    false
                };
                if accepted {
                    if let Some(session) = sessions.get_mut(handle).filter(|session| session.active)
                    {
                        let _ = update_write_filter(kqueue, handle, session, true);
                    }
                }
            }
            Command::CancelOutput { handle, reply } => {
                let found = if let Some(session) = sessions.get_mut(handle) {
                    session.discarding = true;
                    session.paused = false;
                    session.clear_output();
                    session.output_deadline = None;
                    Ok(())
                } else {
                    Err(OperationError::new(
                        Operation::Output,
                        FailureKind::WrongState,
                        None,
                    ))
                };
                if found.is_ok() {
                    read_ready(kqueue, handle, notices, sessions, counters);
                }
                let _ = reply.send(found);
            }
            Command::Size { handle, reply } => {
                let result = sessions
                    .get(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::Size, FailureKind::WrongState, None)
                    })
                    .and_then(session_size);
                let _ = reply.send(result);
            }
            Command::ProcessId { handle, reply } => {
                let result = sessions
                    .get(handle)
                    .map(|session| i64::from(session.pid))
                    .ok_or_else(|| {
                        OperationError::new(Operation::ProcessId, FailureKind::WrongState, None)
                    });
                let _ = reply.send(result);
            }
            Command::TerminalMode { handle, reply } => {
                let result = sessions
                    .get(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::TerminalMode, FailureKind::WrongState, None)
                    })
                    .and_then(session_mode);
                let _ = reply.send(result);
            }
            Command::TerminalName { handle, reply } => {
                let result = sessions
                    .get(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::TerminalName, FailureKind::WrongState, None)
                    })
                    .and_then(session_tty_name);
                let _ = reply.send(result);
            }
            Command::Resize {
                handle,
                size,
                reply,
            } => {
                let resized = sessions
                    .get(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::Resize, FailureKind::WrongState, None)
                    })
                    .and_then(|session| {
                        let size = libc::winsize {
                            ws_row: size[0] as _,
                            ws_col: size[1] as _,
                            ws_xpixel: size[2] as _,
                            ws_ypixel: size[3] as _,
                        };
                        if unsafe {
                            libc::ioctl(session.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size)
                        } == 0
                        {
                            Ok(())
                        } else {
                            Err(OperationError::from_io(
                                Operation::Resize,
                                &io::Error::last_os_error(),
                            ))
                        }
                    });
                let _ = reply.send(resized);
            }
            Command::ObserveMode {
                handle,
                observe,
                reply,
            } => {
                let found = if let Some(session) = sessions.get_mut(handle) {
                    if observe {
                        // Observation begins with a mandatory current-state
                        // emission. Sampling here would make a mode change
                        // that raced subscription indistinguishable from the
                        // baseline and permanently suppress it.
                        session.observed_mode = None;
                        session.mode_interval = MODE_POLL_MIN;
                        session.mode_deadline = Some(Instant::now() + MODE_POLL_MIN);
                    } else {
                        session.observed_mode = None;
                        session.mode_deadline = None;
                    }
                    Ok(())
                } else {
                    Err(OperationError::new(
                        Operation::TerminalMode,
                        FailureKind::WrongState,
                        None,
                    ))
                };
                let _ = reply.send(found);
            }
            Command::Signal {
                handle,
                signal,
                reply,
            } => match sessions.get(handle) {
                Some(session) if session.exit_status.is_some() => {
                    let _ = reply.send(Ok(false));
                }
                Some(session) => {
                    if broker_client
                        .signal_async(session.broker_session, signal, reply.clone())
                        .is_err()
                    {
                        let _ = reply.send(Err(OperationError::new(
                            Operation::Terminate,
                            FailureKind::InfrastructureLost,
                            None,
                        )));
                    }
                }
                None => {
                    let _ = reply.send(Err(OperationError::new(
                        Operation::Terminate,
                        FailureKind::WrongState,
                        None,
                    )));
                }
            },
            Command::CloseStart {
                handle,
                completion,
                reply,
            } => {
                let accepted = sessions.get_mut(handle).is_some_and(|session| {
                    session.close_waiters.push(completion);
                    start_graceful_close(handle, session, broker_client)
                });
                let _ = reply.send(accepted);
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
                        session.close_deadline = None;
                        if !session.abandoned {
                            fail_input(
                                handle,
                                session,
                                notices,
                                counters,
                                OperationError::new(Operation::Write, FailureKind::Closed, None),
                            );
                        }
                    }
                    read_ready(kqueue, handle, notices, sessions, counters);
                    if sessions.get(handle).is_some_and(|session| session.active) {
                        let delivered = send_notice(
                            notices,
                            Notice::Exit {
                                handle,
                                status: i64::from(status),
                            },
                            counters,
                        );
                        if !delivered {
                            if let Some(session) = sessions.get_mut(handle) {
                                mark_notice_failure(session);
                            }
                        }
                    }
                } else {
                    pending_broker_exits.insert(broker_session, status);
                }
            }
            Command::BrokerLost => {
                pending_broker_exits.clear();
                fail_all(notices, sessions, counters, broker_client, admissions);
            }
            Command::GracefulSignalResult { handle, result } => {
                if result.is_none() {
                    if let Some(session) = sessions.get_mut(handle) {
                        if record_cleanup_result(session, false) {
                            escalate_close(handle, session, broker_client);
                        }
                    }
                }
            }
            Command::CloseResult { handle, succeeded } => {
                if let Some(session) = sessions.get_mut(handle) {
                    if !succeeded {
                        session.cleanup_failure.get_or_insert_with(|| {
                            OperationError::new(
                                Operation::Close,
                                FailureKind::InfrastructureLost,
                                None,
                            )
                        });
                        escalate_close(handle, session, broker_client);
                    }
                }
            }
            Command::ForceCloseResult { handle, succeeded } => {
                if let Some(session) = sessions.get_mut(handle) {
                    record_cleanup_result(session, succeeded);
                }
            }
            Command::ReleaseResult { handle, succeeded } => {
                if let Some(session) = sessions.get_mut(handle) {
                    session.record_release_result(succeeded);
                }
            }
            Command::Shutdown => return (true, false),
        }
    }
    (false, processed == COMMAND_QUANTUM)
}

fn process_controls(
    kqueue: RawFd,
    controls: &ControlQueue,
    scratch: &mut VecDeque<Control>,
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    broker: &BrokerClient,
) {
    controls.swap_into(scratch);
    for control in scratch.drain(..) {
        match control {
            Control::Credit { handle, bytes } => {
                if sessions
                    .get_mut(handle)
                    .is_some_and(|session| session.credit(bytes))
                    && sessions.get(handle).is_some_and(|session| session.active)
                {
                    refresh_output(kqueue, handle, notices, sessions, counters);
                }
            }
            Control::Abandon { handle } => {
                if let Some(session) = sessions.get_mut(handle) {
                    session.abandoned = true;
                    session.active = false;
                    session.paused = false;
                    session.admission.close();
                    session.discard_input();
                    session.forget_output();
                    let _ = close_session(handle, session, broker);
                    let _ = update_read_filter(kqueue, handle, session, true);
                    let _ = update_write_filter(kqueue, handle, session, false);
                }
            }
        }
    }
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
    notices: &EventSender<Notice>,
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
        && (!session.paused || session.discarding)
        && !session.output_eof
        && (session.discarding || session.output_total() < session.output_capacity)
    {
        let maximum = if session.discarding {
            64 * 1024
        } else {
            session.output_capacity - session.output_total()
        }
        .min(64 * 1024)
        .min(BYTE_QUANTUM - bytes);
        let mut buffer = Vec::<u8>::with_capacity(maximum);
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
            if session.abandoned || session.discarding {
                continue;
            }
            if session.output_bytes == 0 {
                session.output_deadline = Some(Instant::now() + OUTPUT_DELAY);
            }
            // `read` initialized exactly this prefix.
            unsafe {
                buffer.set_len(amount);
            }
            session.enqueue_output(Bytes::from(buffer));
            continue;
        }
        if result == 0 {
            session.output_eof = true;
            fail_input(
                handle,
                session,
                notices,
                counters,
                OperationError::new(Operation::Write, FailureKind::Closed, None),
            );
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
            fail_input(
                handle,
                session,
                notices,
                counters,
                OperationError::new(Operation::Write, FailureKind::Closed, None),
            );
        } else {
            session.output_eof = true;
            session
                .output_failure
                .get_or_insert_with(|| OperationError::from_io(Operation::Output, &error));
        }
        break;
    }
    refresh_output(kqueue, handle, notices, sessions, counters);
}

fn write_ready(
    kqueue: RawFd,
    handle: u64,
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let mut bytes = 0;
    let mut syscalls = 0;
    let master = session.master.as_raw_fd();
    while bytes < BYTE_QUANTUM && syscalls < SYSCALL_QUANTUM {
        let Some(front) = session.core.input.front() else {
            break;
        };
        let maximum = (front.bytes.len() - front.offset).min(BYTE_QUANTUM - bytes);
        let result = unsafe {
            libc::write(
                master,
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
            let complete = {
                let front = session
                    .core
                    .input
                    .front_mut()
                    .expect("input front remains present during write");
                front.offset += amount;
                front.offset == front.bytes.len()
            };
            session.core.input_bytes -= amount;
            session
                .core
                .admission
                .release(amount, usize::from(complete));
            if complete {
                session.core.input_entries -= 1;
                session.core.input.pop_front();
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
            fail_input(
                handle,
                session,
                notices,
                counters,
                OperationError::from_io(Operation::Write, &error),
            );
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
    notices: &EventSender<Notice>,
    counters: &mut RuntimeCounters,
    failure: OperationError,
) {
    session.input_failed = true;
    session.admission.close_with_failure(failure);
    let accepted_input_pending = session.discard_input();
    if accepted_input_pending {
        session.input_failure.get_or_insert(failure);
        notify_input_failure(handle, session, notices, counters);
    }
}

fn notify_input_failure(
    handle: u64,
    session: &mut Session,
    notices: &EventSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    let Some(failure) = session.input_failure else {
        return;
    };
    if !session.input_failure_notified {
        session.input_failure_notified = true;
        if !send_notice(notices, Notice::InputFailed { handle, failure }, counters) {
            mark_notice_failure(session);
        }
    }
}

fn refresh_output(
    kqueue: RawFd,
    handle: u64,
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let enabled = !session.paused
        && !session.output_eof
        && (session.discarding || session.output_total() < session.output_capacity);
    let _ = update_read_filter(kqueue, handle, session, enabled);
    let output_ready = session.output_bytes <= INTERACTIVE_BATCH
        || session.output_bytes >= OUTPUT_BATCH
        || session.output_eof
        || session
            .output_deadline
            .is_some_and(|deadline| deadline <= Instant::now());
    if session.active
        && !session.discarding
        && output_ready
        && session.has_output()
        && session.output_lease_bytes == 0
    {
        emit_output_notice(handle, session, notices, counters);
    }
    if session.active
        && session.output_eof
        && !session.has_output()
        && session.output_outstanding == 0
        && !session.output_done_notified
    {
        session.output_done_notified = true;
        let delivered = send_notice(
            notices,
            if let Some(failure) = session.output_failure {
                Notice::OutputFailed { handle, failure }
            } else {
                Notice::OutputDone(handle)
            },
            counters,
        );
        if !delivered {
            mark_notice_failure(session);
        }
    }
}

fn emit_output_notice(
    handle: u64,
    session: &mut Session,
    notices: &EventSender<Notice>,
    counters: &mut RuntimeCounters,
) -> bool {
    let bytes = session.pull_output(OUTPUT_BATCH);
    let amount = bytes.len();
    session.mark_output_leased(amount);
    match notices.send(handle, Notice::Output { handle, bytes }) {
        Ok(()) => {
            counters.notifications += 1;
            true
        }
        Err(_) => {
            let reclaimed = session.credit(amount);
            debug_assert!(reclaimed);
            session.forget_output();
            session.abandoned = true;
            session.active = false;
            false
        }
    }
}

fn output_poll_timeout(sessions: &GenerationRegistry<Session>) -> i32 {
    let output_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter(|session| session.has_output() && session.output_lease_bytes == 0)
        .filter_map(|session| session.output_deadline)
        .min();
    let activation_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter(|session| !session.active && !session.abandoned)
        .filter_map(|session| session.activation_deadline)
        .min();
    let mode_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter_map(|session| session.mode_deadline)
        .min();
    let close_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter_map(|session| session.close_deadline)
        .min();
    let deadline = [
        output_deadline,
        activation_deadline,
        mode_deadline,
        close_deadline,
    ]
    .into_iter()
    .flatten()
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
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    for handle in sessions.handles() {
        refresh_output(kqueue, handle, notices, sessions, counters);
    }
}

fn refresh_modes(
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let now = Instant::now();
    for handle in sessions.handles() {
        let due = sessions
            .get(handle)
            .and_then(|session| session.mode_deadline)
            .is_some_and(|deadline| deadline <= now);
        if !due {
            continue;
        }
        let mode = session_mode(
            sessions
                .get(handle)
                .expect("due session must remain registered"),
        );
        let Some(session) = sessions.get_mut(handle) else {
            continue;
        };
        match mode {
            Ok(modes) if Some(modes) != session.observed_mode => {
                session.observed_mode = Some(modes);
                session.mode_interval = MODE_POLL_MIN;
                if !send_notice(notices, Notice::ModeChanged { handle, modes }, counters) {
                    mark_notice_failure(session);
                }
            }
            Ok(_) => {
                session.mode_interval = (session.mode_interval * 2).min(MODE_POLL_MAX);
            }
            Err(failure) => {
                session.mode_deadline = None;
                if !send_notice(notices, Notice::ModeFailed { handle, failure }, counters) {
                    mark_notice_failure(session);
                }
                continue;
            }
        }
        session.mode_deadline = Some(now + session.mode_interval);
    }
}

fn close_session(handle: u64, session: &mut Session, broker: &BrokerClient) -> bool {
    if session.close_started {
        return true;
    }
    if session.exit_status.is_some() && session.output_eof {
        session.admission.close();
        session.close_started = true;
        return true;
    }
    if broker.close_async(session.broker_session, handle).is_err() {
        return false;
    }
    session.admission.close();
    session.close_started = true;
    true
}

fn start_graceful_close(handle: u64, session: &mut Session, broker: &BrokerClient) -> bool {
    if session.close_started {
        return true;
    }
    session.admission.close();
    session.close_started = true;
    if session.exit_status.is_some() {
        // The direct child can exit while descendants retain the PTY and its
        // output descriptor. Ask the broker to kill the owned process group;
        // otherwise EOF and deterministic release can never converge.
        escalate_close(handle, session, broker);
        return true;
    }
    if session.graceful_close_timeout.is_zero() {
        escalate_close(handle, session, broker);
        return true;
    }
    session.close_deadline = Some(Instant::now() + session.graceful_close_timeout);
    if broker
        .graceful_signal_async(session.broker_session, handle)
        .is_err()
    {
        escalate_close(handle, session, broker);
    }
    true
}

fn record_cleanup_result(session: &mut Session, succeeded: bool) -> bool {
    !succeeded && session.exit_status.is_none()
}

fn escalate_close(handle: u64, session: &mut Session, broker: &BrokerClient) {
    if session.close_escalated {
        session.close_deadline = None;
        return;
    }
    session.close_deadline = None;
    match broker.force_close_async(session.broker_session, handle) {
        Ok(()) => session.close_escalated = true,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            // The bounded broker mailbox is shared by all sessions. Retrying
            // on the next reactor turn preserves fairness without losing the
            // close request.
            session.close_deadline = Some(Instant::now());
        }
        Err(_) => {
            session.close_escalated = true;
            // Broker release is the authoritative cleanup result. A failed
            // intermediate request is recoverable when the child exits
            // naturally and infrastructure loss is reported separately.
        }
    }
}

fn escalate_due_closes(sessions: &mut GenerationRegistry<Session>, broker: &BrokerClient) {
    let now = Instant::now();
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            if session
                .close_deadline
                .is_some_and(|deadline| deadline <= now)
            {
                escalate_close(handle, session, broker);
            }
        }
    }
}

fn request_release(handle: u64, session: &mut Session, broker: &BrokerClient) -> bool {
    match session.release {
        ReleaseState::Complete => true,
        ReleaseState::Pending => false,
        ReleaseState::Needed => match broker.release_async(session.broker_session, handle) {
            Ok(()) => {
                session.release = ReleaseState::Pending;
                false
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(_) => {
                session.record_release_result(false);
                true
            }
        },
    }
}

fn reap_closed(
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    broker: &BrokerClient,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        let complete = sessions.get(handle).is_some_and(|session| {
            !session.abandoned
                && session.close_started
                && session.exit_status.is_some()
                && session.output_eof
                && !session.has_output()
                && session.output_outstanding == 0
        });
        if !complete {
            continue;
        }
        if !sessions
            .get_mut(handle)
            .is_some_and(|session| request_release(handle, session, broker))
        {
            continue;
        }
        let result = sessions
            .get(handle)
            .and_then(Session::completed_close_result)
            .unwrap_or_default();
        if let Some(session) = sessions.get_mut(handle) {
            if !session.close_notified {
                session.close_notified = true;
                for waiter in session.close_waiters.drain(..) {
                    let _ = waiter.send(result);
                }
                send_notice(notices, Notice::Closed { handle, result }, counters);
            }
        }
        if sessions.remove(handle).is_some() {
            notices.retire_session(handle);
            if let Ok(mut values) = admissions.lock() {
                values.remove(&handle);
            }
        }
    }
}

fn reap_abandoned(
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    broker: &BrokerClient,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
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
                session.discard_input();
                session.forget_output();
            }
            if session.abandoned && !session.close_started {
                let _ = close_session(handle, session, broker);
            }
        }
        let removable = sessions.get(handle).is_some_and(|session| {
            session.abandoned
                && session.exit_status.is_some()
                && session.output_eof
                && !session.has_output()
                && session.output_outstanding == 0
        });
        if removable {
            if !sessions
                .get_mut(handle)
                .is_some_and(|session| request_release(handle, session, broker))
            {
                continue;
            }
            if sessions.remove(handle).is_some() {
                notices.retire_session(handle);
                if let Ok(mut values) = admissions.lock() {
                    values.remove(&handle);
                }
            }
        }
    }
}

fn shutdown_all(
    sessions: &mut GenerationRegistry<Session>,
    broker: &BrokerClient,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            close_session(handle, session, broker);
        }
    }
    for handle in sessions.handles() {
        if let Some(session) = sessions.remove(handle) {
            let _ = broker.abort_async(session.broker_session);
        }
    }
    if let Ok(mut values) = admissions.lock() {
        values.clear();
    }
}

fn fail_all(
    notices: &EventSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    broker: &BrokerClient,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        if let Some(mut session) = sessions.remove(handle) {
            session.close_started = true;
            let failure =
                OperationError::new(Operation::Runtime, FailureKind::InfrastructureLost, None);
            fail_input(handle, &mut session, notices, counters, failure);
            force_terminal_group(session.master.as_raw_fd(), session.pid);
            let _ = broker.abort_async(session.broker_session);
            send_notice(notices, Notice::BrokerLost { handle, failure }, counters);
            notices.retire_session(handle);
        }
    }
    if let Ok(mut values) = admissions.lock() {
        values.clear();
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

fn session_size(session: &Session) -> Result<[u32; 4], OperationError> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
    if unsafe {
        libc::ioctl(
            session.master.as_raw_fd(),
            libc::TIOCGWINSZ as _,
            size.as_mut_ptr(),
        )
    } != 0
    {
        return Err(OperationError::from_io(
            Operation::Size,
            &io::Error::last_os_error(),
        ));
    }
    let size = unsafe { size.assume_init() };
    Ok([
        size.ws_row.into(),
        size.ws_col.into(),
        size.ws_xpixel.into(),
        size.ws_ypixel.into(),
    ])
}

fn session_mode(session: &Session) -> Result<[bool; 3], OperationError> {
    let name = session_tty_name(session).map_err(|error| {
        OperationError::new(Operation::TerminalMode, error.kind(), error.native_code())
    })?;
    let name = CString::new(name).map_err(|_| {
        OperationError::new(Operation::TerminalMode, FailureKind::NativeFailure, None)
    })?;
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(OperationError::from_io(
            Operation::TerminalMode,
            &io::Error::last_os_error(),
        ));
    }
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(slave.as_raw_fd(), mode.as_mut_ptr()) } != 0 {
        return Err(OperationError::from_io(
            Operation::TerminalMode,
            &io::Error::last_os_error(),
        ));
    }
    let flags = unsafe { mode.assume_init() }.c_lflag;
    Ok([
        flags & libc::ICANON != 0,
        flags & libc::ECHO != 0,
        flags & libc::ISIG != 0,
    ])
}

fn session_tty_name(session: &Session) -> Result<Vec<u8>, OperationError> {
    let mut name = vec![0 as libc::c_char; 1024];
    let status = unsafe { ptsname_r(session.master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if status != 0 {
        return Err(OperationError::new(
            Operation::TerminalName,
            FailureKind::NativeFailure,
            Some(status),
        ));
    }
    let length = name.iter().position(|byte| *byte == 0).ok_or_else(|| {
        OperationError::new(Operation::TerminalName, FailureKind::NativeFailure, None)
    })?;
    Ok(name[..length]
        .iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect())
}

unsafe extern "C" {
    fn ptsname_r(fd: libc::c_int, buffer: *mut libc::c_char, length: libc::size_t) -> libc::c_int;
}

fn send_notice(
    notices: &EventSender<Notice>,
    notice: Notice,
    counters: &mut RuntimeCounters,
) -> bool {
    let handle = notice.handle();
    let sent = if matches!(notice, Notice::ModeChanged { .. }) {
        notices
            .send_coalesced(handle, notice, |pending| {
                matches!(pending, Notice::ModeChanged { .. })
            })
            .is_ok_and(|inserted| inserted)
    } else {
        notices.send(handle, notice).is_ok()
    };
    if sent {
        counters.notifications += 1;
    }
    if !sent {
        counters.notification_failures += 1;
    }
    sent
}

fn mark_notice_failure(session: &mut Session) {
    session.active = false;
    if !session.close_started {
        session.abandoned = true;
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
fn admit_write(
    commands: &SyncSender<Command>,
    handle: u64,
    bytes: Bytes,
    admission: Arc<InputAdmission>,
) -> AdmissionResult {
    let length = bytes.len();
    admit_write_with(commands, handle, length, admission, || bytes)
}

#[cfg(any(feature = "__private_adapter", test))]
fn admit_write_with(
    commands: &SyncSender<Command>,
    handle: u64,
    length: usize,
    admission: Arc<InputAdmission>,
    make_bytes: impl FnOnce() -> Bytes,
) -> AdmissionResult {
    let mut state = match admission.state.try_lock() {
        Ok(state) => state,
        Err(std::sync::TryLockError::WouldBlock) => return AdmissionResult::Backpressure,
        Err(std::sync::TryLockError::Poisoned(_)) => return AdmissionResult::Infrastructure,
    };
    if !state.open {
        return AdmissionResult::Closed;
    }
    if length == 0 {
        return AdmissionResult::Accepted;
    }
    if state.bytes.saturating_add(length) > admission.capacity
        || state.entries >= admission.entry_capacity()
    {
        return AdmissionResult::Backpressure;
    }
    let bytes = make_bytes();
    debug_assert_eq!(bytes.len(), length);
    state.bytes += length;
    state.entries += 1;
    let command = Command::Write {
        handle,
        bytes,
        admission: Arc::clone(&admission),
    };
    if let Err(error) = commands.try_send(command) {
        state.bytes -= length;
        state.entries -= 1;
        return match error {
            TrySendError::Full(_) => AdmissionResult::Backpressure,
            TrySendError::Disconnected(_) => {
                state.open = false;
                state.failure.get_or_insert(OperationError::new(
                    Operation::Write,
                    FailureKind::InfrastructureLost,
                    None,
                ));
                AdmissionResult::Infrastructure
            }
        };
    }
    AdmissionResult::Accepted
}

fn admit_owned_write(
    commands: &SyncSender<Command>,
    handle: u64,
    bytes: Bytes,
    admission: Arc<InputAdmission>,
) -> Result<(), WriteError> {
    let length = bytes.len();
    let mut state = match admission.state.lock() {
        Ok(state) => state,
        Err(_) => {
            return Err(WriteError::new(
                WriteErrorKind::Infrastructure,
                bytes,
                Some(OperationError::new(
                    Operation::Write,
                    FailureKind::InfrastructureLost,
                    None,
                )),
            ));
        }
    };
    if !state.open {
        return Err(WriteError::new(
            WriteErrorKind::Closed,
            bytes,
            state.failure,
        ));
    }
    if length == 0 {
        return Ok(());
    }
    if state.bytes.saturating_add(length) > admission.capacity
        || state.entries >= admission.entry_capacity()
    {
        return Err(WriteError::new(WriteErrorKind::Backpressure, bytes, None));
    }
    state.bytes += length;
    state.entries += 1;
    let command = Command::Write {
        handle,
        bytes,
        admission: Arc::clone(&admission),
    };
    let error = match commands.try_send(command) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    state.bytes -= length;
    state.entries -= 1;
    match error {
        TrySendError::Full(Command::Write { bytes, .. }) => {
            Err(WriteError::new(WriteErrorKind::Backpressure, bytes, None))
        }
        TrySendError::Disconnected(Command::Write { bytes, .. }) => {
            state.open = false;
            let failure =
                OperationError::new(Operation::Write, FailureKind::InfrastructureLost, None);
            state.failure.get_or_insert(failure);
            Err(WriteError::new(
                WriteErrorKind::Infrastructure,
                bytes,
                Some(failure),
            ))
        }
        TrySendError::Full(_) | TrySendError::Disconnected(_) => {
            unreachable!("owned write admission submits only write commands")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admit_owned_write, admit_write, admit_write_with, emit_output_notice, fail_input,
        notify_input_failure, record_cleanup_result, refresh_modes, send_notice, AdmissionResult,
        InputAdmission, Notice, RuntimeCounters, Session, OUTPUT_BATCH,
    };
    use crate::engine::{broker_client::BrokerSession, event, GenerationRegistry};
    use crate::error::{FailureKind, Operation, OperationError, WriteErrorKind};
    use bytes::Bytes;
    use std::collections::VecDeque;
    use std::fs::File;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    fn session(input_capacity: usize) -> Session {
        Session::from_broker(
            BrokerSession {
                id: 1,
                pid: 1,
                master: File::open("/dev/null").unwrap().into(),
            },
            Arc::new(InputAdmission::new(input_capacity)),
            1024,
            Duration::from_millis(250),
        )
    }

    #[test]
    fn terminal_input_failure_rejects_future_writes() {
        let mut session = session(4);
        assert!(session.enqueue_write(vec![1, 2, 3, 4].into()).is_ok());
        session.input_bytes = 4;
        {
            let mut state = session.admission.state.lock().unwrap();
            state.bytes = 4;
            state.entries = 1;
        }
        let (sender, receiver) = event::channel();
        let mut counters = RuntimeCounters::default();
        let failure = OperationError::new(Operation::Write, FailureKind::Closed, None);

        fail_input(7, &mut session, &sender, &mut counters, failure);

        assert_eq!(
            receiver.try_recv(),
            Ok((7, Notice::InputFailed { handle: 7, failure }))
        );
        assert!(session.enqueue_write(vec![1].into()).is_err());
    }

    #[test]
    fn admitted_write_rejected_during_failure_is_reported_once() {
        let mut session = session(4);
        session.active = true;
        {
            let mut state = session.admission.state.lock().unwrap();
            state.bytes = 1;
            state.entries = 1;
        }
        let (sender, receiver) = event::channel();
        let mut counters = RuntimeCounters::default();
        let failure =
            OperationError::new(Operation::Write, FailureKind::InfrastructureLost, Some(73));

        fail_input(7, &mut session, &sender, &mut counters, failure);
        assert_eq!(
            receiver.try_recv(),
            Ok((7, Notice::InputFailed { handle: 7, failure }))
        );
        assert!(session.enqueue_write(vec![1].into()).is_err());
        session.admission.release(1, 1);
        notify_input_failure(7, &mut session, &sender, &mut counters);
        notify_input_failure(7, &mut session, &sender, &mut counters);

        assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn terminal_input_failure_is_sticky_for_later_owned_writes() {
        let admission = Arc::new(InputAdmission::new(4));
        let failure = OperationError::new(Operation::Write, FailureKind::NativeFailure, Some(73));
        admission.close_with_failure(failure);
        let (sender, _receiver) = mpsc::sync_channel(1);

        let rejected =
            admit_owned_write(&sender, 7, Bytes::from_static(b"x"), Arc::clone(&admission))
                .expect_err("failed input must reject later writes");

        assert_eq!(rejected.kind(), WriteErrorKind::Closed);
        let retained = rejected.failure();
        let bytes = rejected.into_bytes();
        assert_eq!(bytes, Bytes::from_static(b"x"));
        assert_eq!(retained, Some(failure));
    }

    #[test]
    fn disconnected_command_queue_permanently_closes_input() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);

        let result = admit_write(&sender, 7, vec![1].into(), Arc::clone(&admission));

        assert_eq!(result, AdmissionResult::Infrastructure);
        let state = admission.state.lock().unwrap();
        assert!(!state.open);
        assert_eq!(
            state.failure,
            Some(OperationError::new(
                Operation::Write,
                FailureKind::InfrastructureLost,
                None,
            ))
        );
        assert_eq!(state.bytes, 0);
        assert_eq!(state.entries, 0);
    }

    #[test]
    fn full_command_queue_preserves_recoverable_input() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, _receiver) = mpsc::sync_channel(0);

        let result = admit_write(&sender, 7, vec![1].into(), Arc::clone(&admission));

        assert_eq!(result, AdmissionResult::Backpressure);
        let state = admission.state.lock().unwrap();
        assert!(state.open);
        assert_eq!(state.bytes, 0);
        assert_eq!(state.entries, 0);
    }

    #[test]
    fn owned_write_rejection_returns_the_exact_allocation() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, _receiver) = mpsc::sync_channel(0);
        let bytes = Bytes::from(vec![1]);
        let original_pointer = bytes.as_ptr();

        let rejected = admit_owned_write(&sender, 7, bytes, admission)
            .expect_err("full command queue must reject the write");

        assert_eq!(rejected.kind(), WriteErrorKind::Backpressure);
        let bytes = rejected.into_bytes();
        assert_eq!(bytes.as_ptr(), original_pointer);
    }

    #[test]
    fn owned_write_acceptance_moves_the_exact_allocation() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, receiver) = mpsc::sync_channel(1);
        let bytes = Bytes::from(vec![1]);
        let original_pointer = bytes.as_ptr();

        admit_owned_write(&sender, 7, bytes, admission).expect("write must be accepted");

        let super::Command::Write { bytes, .. } = receiver.recv().expect("accepted write command")
        else {
            panic!("owned write admission must submit a write command");
        };
        assert_eq!(bytes.as_ptr(), original_pointer);
    }

    #[test]
    fn empty_owned_write_is_a_noop() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, receiver) = mpsc::sync_channel(0);

        admit_owned_write(&sender, 7, Bytes::new(), Arc::clone(&admission))
            .expect("empty writes are successful no-ops");

        assert!(receiver.try_recv().is_err());
        let state = admission.state.lock().expect("admission state");
        assert_eq!(state.bytes, 0);
        assert_eq!(state.entries, 0);
    }

    #[test]
    fn rejected_admission_does_not_construct_input_storage() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut constructed = false;

        let result = admit_write_with(&sender, 7, 5, admission, || {
            constructed = true;
            Bytes::from_static(b"large")
        });

        assert_eq!(result, AdmissionResult::Backpressure);
        assert!(!constructed);
    }

    #[test]
    fn contended_copy_admission_returns_backpressure_without_copying() {
        let admission = Arc::new(InputAdmission::new(4));
        let held_state = admission.state.lock().expect("admission state");
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut constructed = false;

        let result = admit_write_with(&sender, 7, 1, Arc::clone(&admission), || {
            constructed = true;
            Bytes::from_static(b"x")
        });

        assert_eq!(result, AdmissionResult::Backpressure);
        assert!(!constructed);
        drop(held_state);
    }

    #[test]
    fn poisoned_input_admission_is_an_infrastructure_failure() {
        let admission = Arc::new(InputAdmission::new(4));
        let poisoned = Arc::clone(&admission);
        let _ = thread::spawn(move || {
            let _state = poisoned.state.lock().unwrap();
            panic!("poison input admission");
        })
        .join();
        let (sender, _receiver) = mpsc::sync_channel(1);

        let result = admit_write(&sender, 7, vec![1].into(), admission);

        assert_eq!(result, AdmissionResult::Infrastructure);
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
            session.enqueue_output(bytes.into());
        }

        let mut actual = Vec::new();
        let mut outstanding = Vec::new();
        while session.output_bytes != 0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let maximum = 1 + state as usize % 193;
            let bytes = session.pull_output(maximum);
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
        session.enqueue_output(bytes.into());

        let pulled = session.pull_output(OUTPUT_BATCH);

        assert_eq!(pulled.as_ptr(), pointer);
        assert_eq!(session.output_bytes, 0);
        assert_eq!(session.output_outstanding, OUTPUT_BATCH);
    }

    #[test]
    fn output_rejected_by_a_closed_session_route_is_reclaimed() {
        let (sender, receiver) = event::channel();
        let route = receiver.session(7).expect("register session receiver");
        drop(route);
        let mut session = session(4096);
        session.active = true;
        session.enqueue_output(Bytes::from(vec![0x5a; OUTPUT_BATCH + 1]));
        let mut counters = RuntimeCounters::default();

        assert!(!emit_output_notice(7, &mut session, &sender, &mut counters));

        assert!(session.abandoned);
        assert!(!session.active);
        assert_eq!(session.output_outstanding, 0);
        assert_eq!(session.output_lease_bytes, 0);
        assert!(!session.has_output());
        assert_eq!(counters.notifications, 0);
    }

    #[test]
    fn natural_exit_ignores_a_late_cleanup_failure() {
        let mut session = session(4096);
        session.exit_status = Some(0);

        record_cleanup_result(&mut session, false);

        assert!(session.cleanup_failure.is_none());
    }

    #[test]
    fn close_completion_waits_for_release_and_reports_release_failure() {
        let mut session = session(4096);
        session.close_started = true;
        session.exit_status = Some(0);
        session.output_eof = true;

        assert_eq!(session.completed_close_result(), None);
        session.release = super::ReleaseState::Pending;
        assert_eq!(session.completed_close_result(), None);

        session.record_release_result(false);
        assert_eq!(
            session.completed_close_result(),
            Some(crate::engine::CloseResult {
                cleanup_failure: Some(OperationError::new(
                    Operation::Close,
                    FailureKind::InfrastructureLost,
                    None,
                )),
                ..crate::engine::CloseResult::default()
            })
        );
    }

    #[test]
    fn successful_release_retains_an_earlier_cleanup_failure() {
        let mut session = session(4096);
        let failure =
            OperationError::new(Operation::Close, FailureKind::InfrastructureLost, Some(74));
        session.cleanup_failure = Some(failure);

        session.record_release_result(true);

        assert_eq!(session.cleanup_failure, Some(failure));
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
                session.input_entries -= 1;
                assert_eq!(queued.offset, 0);
                assert_eq!(Some(&queued.bytes), expected.pop_front().as_ref());
                continue;
            }

            let length = 1 + state as usize % 64;
            let bytes = Bytes::from(
                (0..length)
                    .map(|index| (step + index) as u8)
                    .collect::<Vec<_>>(),
            );
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
            assert_eq!(session.input_entries, session.input.len());
        }

        for queued in &session.input {
            assert_eq!(Some(&queued.bytes), expected.pop_front().as_ref());
        }
        assert!(expected.is_empty());

        session.close_started = true;
        assert!(session.enqueue_write(vec![1].into()).is_err());
    }

    #[test]
    fn tiny_writes_have_a_bounded_admission_count() {
        let admission = Arc::new(InputAdmission::new(64 * 1024 * 1024));
        let limit = admission.entry_capacity();
        let (sender, _receiver) = mpsc::sync_channel(limit + 1);

        for sequence in 0..limit {
            assert_eq!(
                admit_write(
                    &sender,
                    7,
                    Bytes::from(vec![(sequence & 0xff) as u8]),
                    Arc::clone(&admission),
                ),
                AdmissionResult::Accepted
            );
        }
        assert_eq!(
            admit_write(&sender, 7, Bytes::from_static(b"x"), Arc::clone(&admission)),
            AdmissionResult::Backpressure
        );

        let state = admission.state.lock().unwrap();
        assert_eq!(state.bytes, limit);
        assert_eq!(state.entries, limit);
        assert_eq!(limit, 4096);
    }

    #[test]
    fn pending_mode_observations_coalesce_to_the_current_state() {
        let (sender, receiver) = event::channel();
        let mut counters = RuntimeCounters::default();
        send_notice(
            &sender,
            Notice::ModeChanged {
                handle: 7,
                modes: [false, false, false],
            },
            &mut counters,
        );
        send_notice(&sender, Notice::OutputDone(7), &mut counters);
        for sequence in 0..100_000 {
            send_notice(
                &sender,
                Notice::ModeChanged {
                    handle: 7,
                    modes: [sequence & 1 != 0, true, false],
                },
                &mut counters,
            );
        }

        assert_eq!(
            receiver.try_recv(),
            Ok((
                7,
                Notice::ModeChanged {
                    handle: 7,
                    modes: [true, true, false],
                }
            ))
        );
        assert_eq!(receiver.try_recv(), Ok((7, Notice::OutputDone(7))));
        assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(counters.notifications, 2);
    }

    #[test]
    fn mode_observation_reports_native_failure_and_stops_polling() {
        let (sender, receiver) = event::channel();
        let mut sessions = GenerationRegistry::new();
        let mut value = session(4096);
        value.mode_deadline = Some(Instant::now());
        let handle = sessions.insert(value);
        let mut counters = RuntimeCounters::default();

        refresh_modes(&sender, &mut sessions, &mut counters);

        let (_, Notice::ModeFailed { failure, .. }) =
            receiver.try_recv().expect("mode failure event")
        else {
            panic!("invalid controller must report mode failure");
        };
        assert_eq!(failure.operation(), Operation::TerminalMode);
        assert_eq!(failure.kind(), FailureKind::NativeFailure);
        assert!(failure.native_code().is_some());
        assert!(sessions
            .get(handle)
            .expect("session remains owned")
            .mode_deadline
            .is_none());
    }
}
