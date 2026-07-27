mod handles;
mod spawn;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::ptr::null_mut;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::ResizePseudoConsole;
use windows_sys::Win32::System::JobObjects::TerminateJobObject;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus,
};

use self::handles::{IoOperation, OwnedHandle, OwnedProcessWait, OwnedPseudoConsole};
pub(crate) use self::spawn::BrokerSpawn;
use crate::GenerationRegistry;

const BYTE_QUANTUM: usize = 64 * 1024;
const INTERACTIVE_BATCH: usize = 256;
const OUTPUT_BATCH: usize = 64 * 1024;
const OUTPUT_DELAY: Duration = Duration::from_millis(10);
const COMMAND_QUANTUM: usize = 64;
const COMMAND_CAPACITY: usize = 1024;
const NOTICE_CAPACITY: usize = 4096;
const WAITER_CAPACITY: usize = 1024;
const CLOSER_CAPACITY: usize = 128;
const CLOSER_THREADS: usize = 4;
const EXIT_KEY_TAG: usize = 1_usize << (usize::BITS - 1);
const CLOSE_KEY_TAG: usize = 1_usize << (usize::BITS - 2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Notice {
    Output { handle: u64, bytes: Vec<u8> },
    Capacity { handle: u64, waiter: u64 },
    Flush { handle: u64, waiter: u64 },
    WaitFailed { handle: u64, waiter: u64 },
    InputFailed(u64),
    BrokerLost(u64),
    OutputDone(u64),
    Exit(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Ready,
    Armed,
    Failed,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuntimeCounters {
    pub(crate) reactor_wakeups: u64,
    pub(crate) reactor_events: u64,
    pub(crate) command_wakeups: u64,
    pub(crate) read_syscalls: u64,
    pub(crate) write_syscalls: u64,
    pub(crate) read_bytes: u64,
    pub(crate) write_bytes: u64,
    pub(crate) notifications: u64,
}

struct QueuedInput {
    bytes: Vec<u8>,
    sequence: u64,
}

struct QueuedOutput {
    bytes: Vec<u8>,
    offset: usize,
}

struct Session {
    input_pipe: OwnedHandle,
    output_pipe: OwnedHandle,
    pseudoconsole: Option<OwnedPseudoConsole>,
    process: OwnedHandle,
    process_wait: Option<OwnedProcessWait>,
    job: OwnedHandle,
    pid: u32,
    size: [u32; 4],
    input_capacity: usize,
    input_bytes: usize,
    input: VecDeque<QueuedInput>,
    write: Option<Pin<Box<IoOperation>>>,
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
    read: Option<Pin<Box<IoOperation>>>,
    paused: bool,
    output_eof: bool,
    exit_status: Option<i32>,
    exit_notified: bool,
    close_started: bool,
    pseudoconsole_close_started: bool,
    pseudoconsole_close_done: bool,
    cleanup_failed: bool,
    active: bool,
}

impl Session {
    fn from_spawned(
        spawned: spawn::SpawnedSession,
        input_capacity: usize,
        output_capacity: usize,
    ) -> Self {
        Self {
            input_pipe: spawned.input,
            output_pipe: spawned.output,
            pseudoconsole: Some(spawned.pseudoconsole),
            process: spawned.process,
            process_wait: None,
            job: spawned.job,
            pid: spawned.pid,
            size: spawned.size,
            input_capacity,
            input_bytes: 0,
            input: VecDeque::new(),
            write: None,
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
            read: None,
            paused: true,
            output_eof: false,
            exit_status: None,
            exit_notified: false,
            close_started: false,
            pseudoconsole_close_started: false,
            pseudoconsole_close_done: false,
            cleanup_failed: false,
            active: false,
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
        self.input.push_back(QueuedInput { bytes, sequence });
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
            let front = self.output.front_mut().expect("output byte count is exact");
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
        self.credit(credit).then(|| self.pull(maximum))
    }

    fn output_total(&self) -> usize {
        self.output_bytes + self.output_outstanding
    }

    fn terminal(&self) -> bool {
        self.exit_status.is_some()
            && self.output_eof
            && self.output.is_empty()
            && self.output_outstanding == 0
            && self.read.is_none()
            && self.write.is_none()
            && self.pseudoconsole_close_done
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing an overlapped handle requests cancellation but does not prove
        // that the kernel has stopped using its OVERLAPPED and buffer storage.
        if let Some(read) = self.read.take() {
            std::mem::forget(read);
        }
        if let Some(write) = self.write.take() {
            std::mem::forget(write);
        }
    }
}

enum Command {
    Add {
        spawned: spawn::SpawnedSession,
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
        reply: Sender<Option<i32>>,
    },
    Pid {
        handle: u64,
        reply: Sender<Option<i64>>,
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
        reply: Sender<Option<[bool; 3]>>,
    },
    TtyName {
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
    Counters {
        reply: Sender<RuntimeCounters>,
    },
    Shutdown,
}

#[derive(Clone, Copy)]
struct IocpSender(HANDLE);

unsafe impl Send for IocpSender {}
unsafe impl Sync for IocpSender {}

impl IocpSender {
    fn post_command(self) -> io::Result<()> {
        if unsafe { PostQueuedCompletionStatus(self.0, 0, 0, null_mut()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

struct CloseTask {
    handle: u64,
    pseudoconsole: OwnedPseudoConsole,
    iocp: IocpSender,
}

#[derive(Clone)]
struct CloserPool {
    tasks: SyncSender<CloseTask>,
}

impl CloserPool {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<CloseTask>(CLOSER_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..CLOSER_THREADS {
            let receiver = Arc::clone(&receiver);
            let _ = thread::Builder::new()
                .name(format!("ptyx-conpty-closer-{index}"))
                .spawn(move || loop {
                    let task = {
                        let Ok(receiver) = receiver.lock() else {
                            return;
                        };
                        let Ok(task) = receiver.recv() else {
                            return;
                        };
                        task
                    };
                    task.pseudoconsole.close();
                    unsafe {
                        PostQueuedCompletionStatus(
                            task.iocp.0,
                            0,
                            task.handle as usize | CLOSE_KEY_TAG,
                            null_mut(),
                        );
                    }
                });
        }
        Self { tasks: sender }
    }

    fn submit(&self, task: CloseTask) -> Result<(), CloseTask> {
        self.tasks.try_send(task).map_err(|error| match error {
            mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
        })
    }
}

pub(crate) struct IntegratedRuntime {
    commands: SyncSender<Command>,
    notices: Option<Receiver<Notice>>,
    iocp: IocpSender,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl IntegratedRuntime {
    pub(crate) fn try_new() -> io::Result<Self> {
        spawn::validate_windows_build()?;
        let iocp = OwnedHandle::new(unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1)
        })?;
        let iocp_sender = IocpSender(iocp.raw());
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (notice_sender, notice_receiver) = mpsc::sync_channel(NOTICE_CAPACITY);
        let thread = thread::Builder::new()
            .name("ptyx-windows-iocp".to_owned())
            .spawn(move || reactor(iocp, command_receiver, notice_sender))?;
        Ok(Self {
            commands: command_sender,
            notices: Some(notice_receiver),
            iocp: iocp_sender,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn take_notifications(&mut self) -> Option<Receiver<Notice>> {
        self.notices.take()
    }

    pub(crate) fn spawn_staged(
        &self,
        config: BrokerSpawn,
        input_capacity: usize,
        output_capacity: usize,
    ) -> io::Result<u64> {
        if input_capacity == 0 || output_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capacities must be nonzero",
            ));
        }
        let spawned = spawn::spawn(config)?;
        self.request(|reply| Command::Add {
            spawned,
            input_capacity,
            output_capacity,
            reply,
        })
    }

    pub(crate) fn activate(&self, handle: u64) -> bool {
        self.request(|reply| Command::Activate { handle, reply })
            .is_ok()
    }

    pub(crate) fn try_write(&self, handle: u64, bytes: Vec<u8>) -> u64 {
        self.request(|reply| Command::Write {
            handle,
            bytes,
            reply,
        })
    }

    pub(crate) fn pull(&self, handle: u64, maximum: usize) -> Option<Vec<u8>> {
        self.request(|reply| Command::Pull {
            handle,
            maximum,
            reply,
        })
    }

    pub(crate) fn credit(&self, handle: u64, bytes: usize) -> bool {
        self.request(|reply| Command::Credit {
            handle,
            bytes,
            reply,
        })
    }

    pub(crate) fn credit_async(&self, handle: u64, bytes: usize) -> bool {
        if self
            .commands
            .try_send(Command::CreditAsync { handle, bytes })
            .is_err()
        {
            return false;
        }
        self.iocp.post_command().is_ok()
    }

    pub(crate) fn exchange(&self, handle: u64, credit: usize, maximum: usize) -> Option<Vec<u8>> {
        self.request(|reply| Command::Exchange {
            handle,
            credit,
            maximum,
            reply,
        })
    }

    pub(crate) fn flush_ready(&self, handle: u64, sequence: u64) -> bool {
        self.request(|reply| Command::FlushReady {
            handle,
            sequence,
            reply,
        })
    }

    pub(crate) fn wait_capacity(&self, handle: u64, required: usize, waiter: u64) -> WaitResult {
        self.request(|reply| Command::WaitCapacity {
            handle,
            required,
            waiter,
            reply,
        })
    }

    pub(crate) fn wait_flush(&self, handle: u64, sequence: u64, waiter: u64) -> WaitResult {
        self.request(|reply| Command::WaitFlush {
            handle,
            sequence,
            waiter,
            reply,
        })
    }

    pub(crate) fn pause(&self, handle: u64, paused: bool) -> bool {
        self.request(|reply| Command::Pause {
            handle,
            paused,
            reply,
        })
    }

    pub(crate) fn exit_status(&self, handle: u64) -> Option<i32> {
        self.request(|reply| Command::ExitStatus { handle, reply })
    }

    pub(crate) fn pid(&self, handle: u64) -> Option<i64> {
        self.request(|reply| Command::Pid { handle, reply })
    }

    pub(crate) fn size(&self, handle: u64) -> Option<[u32; 4]> {
        self.request(|reply| Command::Size { handle, reply })
    }

    pub(crate) fn resize(&self, handle: u64, size: [u32; 4]) -> bool {
        self.request(|reply| Command::Resize {
            handle,
            size,
            reply,
        })
    }

    pub(crate) fn mode(&self, _handle: u64) -> Option<[bool; 3]> {
        self.request(|reply| Command::Mode { reply })
    }

    pub(crate) fn tty_name(&self, _handle: u64) -> Option<Vec<u8>> {
        self.request(|reply| Command::TtyName { reply })
    }

    pub(crate) fn signal(&self, handle: u64, signal: i32) -> Option<bool> {
        self.request(|reply| Command::Signal {
            handle,
            signal,
            reply,
        })
    }

    pub(crate) fn output_total(&self, handle: u64) -> Option<usize> {
        self.request(|reply| Command::OutputTotal { handle, reply })
    }

    pub(crate) fn output_done(&self, handle: u64) -> bool {
        self.request(|reply| Command::OutputDone { handle, reply })
    }

    pub(crate) fn close(&self, handle: u64) -> bool {
        self.request(|reply| Command::Close { handle, reply })
    }

    pub(crate) fn destroy(&self, handle: u64) -> bool {
        self.request(|reply| Command::Destroy { handle, reply })
    }

    pub(crate) fn counters(&self) -> RuntimeCounters {
        self.request(|reply| Command::Counters { reply })
    }

    fn request<R>(&self, command: impl FnOnce(Sender<R>) -> Command) -> R {
        let (sender, receiver) = mpsc::channel();
        self.commands.send(command(sender)).unwrap();
        self.iocp.post_command().unwrap();
        receiver.recv().unwrap()
    }
}

impl Drop for IntegratedRuntime {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        let _ = self.iocp.post_command();
        if let Some(thread) = self.thread.lock().ok().and_then(|mut value| value.take()) {
            let _ = thread.join();
        }
    }
}

fn reactor(iocp: OwnedHandle, commands: Receiver<Command>, notices: SyncSender<Notice>) {
    let closer = CloserPool::new();
    let iocp_sender = IocpSender(iocp.raw());
    let mut sessions = GenerationRegistry::<Session>::new();
    let mut counters = RuntimeCounters::default();
    loop {
        refresh_due_outputs(iocp.raw(), &closer, &notices, &mut sessions, &mut counters);
        let timeout = output_timeout(&sessions);
        let mut transferred = 0;
        let mut key = 0;
        let mut overlapped = null_mut();
        let succeeded = unsafe {
            GetQueuedCompletionStatus(
                iocp.raw(),
                &mut transferred,
                &mut key,
                &mut overlapped,
                timeout,
            )
        };
        counters.reactor_wakeups += 1;
        if overlapped.is_null() {
            if succeeded == 0 && unsafe { GetLastError() } == WAIT_TIMEOUT {
                continue;
            }
            if key & CLOSE_KEY_TAG != 0 {
                let handle = (key & !CLOSE_KEY_TAG) as u64;
                if let Some(session) = sessions.get_mut(handle) {
                    session.pseudoconsole_close_done = true;
                    ensure_read(iocp.raw(), handle, session, &notices, &mut counters);
                    refresh_output(handle, session, &notices, &mut counters);
                }
                continue;
            }
            if key == 0 {
                counters.command_wakeups += 1;
                if process_commands(
                    iocp.raw(),
                    iocp_sender,
                    &commands,
                    &closer,
                    &notices,
                    &mut sessions,
                    &mut counters,
                ) {
                    shutdown_all(&mut sessions);
                    return;
                }
                continue;
            }
        }
        counters.reactor_events += 1;
        if key & EXIT_KEY_TAG != 0 {
            let handle = (key & !EXIT_KEY_TAG) as u64;
            handle_process_exit(
                iocp.raw(),
                iocp_sender,
                handle,
                &closer,
                &notices,
                &mut sessions,
                &mut counters,
            );
            continue;
        }
        let handle = key as u64;
        let error = (succeeded == 0).then(io::Error::last_os_error);
        handle_io_completion(
            iocp.raw(),
            handle,
            overlapped,
            transferred,
            error,
            &closer,
            &notices,
            &mut sessions,
            &mut counters,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn process_commands(
    iocp: HANDLE,
    iocp_sender: IocpSender,
    commands: &Receiver<Command>,
    closer: &CloserPool,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) -> bool {
    for _ in 0..COMMAND_QUANTUM {
        let Ok(command) = commands.try_recv() else {
            return false;
        };
        match command {
            Command::Add {
                spawned,
                input_capacity,
                output_capacity,
                reply,
            } => {
                let handle = sessions.insert(Session::from_spawned(
                    spawned,
                    input_capacity,
                    output_capacity,
                ));
                let result = associate_session(
                    iocp,
                    handle,
                    sessions.get_mut(handle).expect("new session is present"),
                )
                .map(|()| handle);
                if result.is_err() {
                    if let Some(session) = sessions.get(handle) {
                        unsafe {
                            TerminateJobObject(session.job.raw(), 1);
                        }
                    }
                    sessions.remove(handle);
                } else if sessions.get(handle).is_some_and(|session| {
                    (unsafe { WaitForSingleObject(session.process.raw(), 0) }) == WAIT_OBJECT_0
                }) {
                    handle_process_exit(
                        iocp,
                        iocp_sender,
                        handle,
                        closer,
                        notices,
                        sessions,
                        counters,
                    );
                }
                let _ = reply.send(result);
            }
            Command::Activate { handle, reply } => {
                let result = sessions
                    .get_mut(handle)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "stale session"))
                    .map(|session| {
                        session.active = true;
                        if session.exit_status.is_some() && !session.exit_notified {
                            session.exit_notified = true;
                            send_notice(notices, Notice::Exit(handle), counters);
                        }
                    });
                if let Some(session) = sessions.get_mut(handle) {
                    ensure_read(iocp, handle, session, notices, counters);
                    ensure_write(iocp, handle, session, notices, counters);
                    refresh_output(handle, session, notices, counters);
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
                    if let Some(session) = sessions.get_mut(handle).filter(|value| value.active) {
                        ensure_write(iocp, handle, session, notices, counters);
                    }
                }
                let _ = reply.send(sequence);
            }
            Command::Pull {
                handle,
                maximum,
                reply,
            } => {
                let value = sessions
                    .get_mut(handle)
                    .map(|session| session.pull(maximum));
                if let Some(session) = sessions.get_mut(handle) {
                    ensure_read(iocp, handle, session, notices, counters);
                    refresh_output(handle, session, notices, counters);
                }
                let _ = reply.send(value);
            }
            Command::Credit {
                handle,
                bytes,
                reply,
            } => {
                let value = sessions
                    .get_mut(handle)
                    .is_some_and(|session| session.credit(bytes));
                if let Some(session) = sessions.get_mut(handle) {
                    ensure_read(iocp, handle, session, notices, counters);
                    refresh_output(handle, session, notices, counters);
                }
                let _ = reply.send(value);
            }
            Command::CreditAsync { handle, bytes } => {
                if let Some(session) = sessions.get_mut(handle) {
                    if session.credit(bytes) {
                        ensure_read(iocp, handle, session, notices, counters);
                        refresh_output(handle, session, notices, counters);
                    }
                }
            }
            Command::Exchange {
                handle,
                credit,
                maximum,
                reply,
            } => {
                let value = sessions
                    .get_mut(handle)
                    .and_then(|session| session.exchange(credit, maximum));
                if let Some(session) = sessions.get_mut(handle) {
                    ensure_read(iocp, handle, session, notices, counters);
                    refresh_output(handle, session, notices, counters);
                }
                let _ = reply.send(value);
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
                    if !paused {
                        start_pseudoconsole_close(
                            iocp_sender,
                            handle,
                            session,
                            closer,
                            notices,
                            counters,
                        );
                        ensure_read(iocp, handle, session, notices, counters);
                    }
                    true
                } else {
                    false
                };
                let _ = reply.send(found);
            }
            Command::ExitStatus { handle, reply } => {
                let _ = reply.send(sessions.get(handle).and_then(|session| session.exit_status));
            }
            Command::Pid { handle, reply } => {
                let _ = reply.send(sessions.get(handle).map(|session| i64::from(session.pid)));
            }
            Command::Size { handle, reply } => {
                let _ = reply.send(sessions.get(handle).map(|session| session.size));
            }
            Command::Resize {
                handle,
                size,
                reply,
            } => {
                let resized = sessions.get_mut(handle).is_some_and(|session| {
                    let Ok(rows) = i16::try_from(size[0]) else {
                        return false;
                    };
                    let Ok(columns) = i16::try_from(size[1]) else {
                        return false;
                    };
                    if rows <= 0 || columns <= 0 {
                        return false;
                    }
                    let Some(pseudoconsole) = session.pseudoconsole.as_ref() else {
                        return false;
                    };
                    let result = unsafe {
                        ResizePseudoConsole(
                            pseudoconsole.raw(),
                            windows_sys::Win32::System::Console::COORD {
                                X: columns,
                                Y: rows,
                            },
                        )
                    };
                    if result >= 0 {
                        session.size = size;
                        true
                    } else {
                        false
                    }
                });
                let _ = reply.send(resized);
            }
            Command::Mode { reply } => {
                let _ = reply.send(None);
            }
            Command::TtyName { reply } => {
                let _ = reply.send(None);
            }
            Command::Signal {
                handle,
                signal,
                reply,
            } => {
                let result = sessions.get(handle).map(|session| {
                    if signal <= 0 || session.exit_status.is_some() {
                        false
                    } else {
                        (unsafe { TerminateJobObject(session.job.raw(), 1) }) != 0
                    }
                });
                let _ = reply.send(result);
            }
            Command::OutputTotal { handle, reply } => {
                let _ = reply.send(sessions.get(handle).map(Session::output_total));
            }
            Command::OutputDone { handle, reply } => {
                let _ = reply.send(sessions.get(handle).is_some_and(|session| {
                    session.output_eof
                        && session.output.is_empty()
                        && session.output_outstanding == 0
                }));
            }
            Command::Close { handle, reply } => {
                let closed = sessions.get_mut(handle).is_some_and(|session| {
                    if !session.close_started {
                        session.close_started = true;
                        session.paused = false;
                        session.output.clear();
                        session.output_bytes = 0;
                        session.output_outstanding = 0;
                        fail_input_waiters(handle, session, notices, counters);
                        unsafe {
                            TerminateJobObject(session.job.raw(), 1);
                        }
                        cancel_write(session);
                    }
                    start_pseudoconsole_close(
                        iocp_sender,
                        handle,
                        session,
                        closer,
                        notices,
                        counters,
                    );
                    ensure_read(iocp, handle, session, notices, counters);
                    true
                });
                let _ = reply.send(closed);
            }
            Command::Destroy { handle, reply } => {
                let removable = sessions.get(handle).is_some_and(Session::terminal);
                let _ = reply.send(removable && sessions.remove(handle).is_some());
            }
            Command::Counters { reply } => {
                let _ = reply.send(*counters);
            }
            Command::Shutdown => return true,
        }
    }
    let _ = iocp_sender.post_command();
    false
}

fn associate_session(iocp: HANDLE, handle: u64, session: &mut Session) -> io::Result<()> {
    for pipe in [&session.input_pipe, &session.output_pipe] {
        let associated = unsafe { CreateIoCompletionPort(pipe.raw(), iocp, handle as usize, 0) };
        if associated.is_null() {
            return Err(io::Error::last_os_error());
        }
    }
    session.process_wait = Some(OwnedProcessWait::register(
        session.process.raw(),
        iocp,
        handle as usize | EXIT_KEY_TAG,
    )?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_process_exit(
    iocp: HANDLE,
    iocp_sender: IocpSender,
    handle: u64,
    closer: &CloserPool,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    if session.exit_status.is_none()
        && unsafe { WaitForSingleObject(session.process.raw(), 0) } == WAIT_OBJECT_0
    {
        let mut status = 0;
        if unsafe { GetExitCodeProcess(session.process.raw(), &mut status) } != 0 {
            session.exit_status = Some(status as i32);
            fail_input_waiters(handle, session, notices, counters);
            cancel_write(session);
        } else {
            session.cleanup_failed = true;
            send_notice(notices, Notice::BrokerLost(handle), counters);
        }
    }
    if session.exit_status.is_some() && session.active && !session.exit_notified {
        session.exit_notified = true;
        send_notice(notices, Notice::Exit(handle), counters);
    }
    start_pseudoconsole_close(iocp_sender, handle, session, closer, notices, counters);
    ensure_read(iocp, handle, session, notices, counters);
}

#[allow(clippy::too_many_arguments)]
fn handle_io_completion(
    iocp: HANDLE,
    handle: u64,
    overlapped: *mut windows_sys::Win32::System::IO::OVERLAPPED,
    transferred: u32,
    error: Option<io::Error>,
    closer: &CloserPool,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    let Some(session) = sessions.get_mut(handle) else {
        return;
    };
    let read_matches = session
        .read
        .as_ref()
        .is_some_and(|operation| operation.overlapped_ptr() == overlapped);
    let write_matches = session
        .write
        .as_ref()
        .is_some_and(|operation| operation.overlapped_ptr() == overlapped);
    if read_matches {
        let operation = session.read.take().expect("read completion owns operation");
        if let Some(error) = error {
            if matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_OPERATION_ABORTED as i32
            ) {
                session.output_eof = true;
            } else {
                session.cleanup_failed = true;
                session.output_eof = true;
                send_notice(notices, Notice::BrokerLost(handle), counters);
            }
        } else if transferred == 0 {
            session.output_eof = true;
        } else {
            let amount = transferred as usize;
            counters.read_bytes += amount as u64;
            if session.close_started {
                session.output_notified = false;
            } else {
                if session.output_bytes == 0 {
                    session.output_deadline = Some(Instant::now() + OUTPUT_DELAY);
                }
                session.output_bytes += amount;
                session.output.push_back(QueuedOutput {
                    bytes: operation.buffer[..amount].to_vec(),
                    offset: 0,
                });
            }
        }
        refresh_output(handle, session, notices, counters);
        ensure_read(iocp, handle, session, notices, counters);
    } else if write_matches {
        let mut operation = session
            .write
            .take()
            .expect("write completion owns operation");
        if error.is_some() || transferred == 0 {
            session.write = Some(operation);
            fail_input_waiters(handle, session, notices, counters);
            session.write.take();
            session.input_bytes = 0;
        } else {
            let amount = transferred as usize;
            counters.write_bytes += amount as u64;
            operation.as_mut().get_mut().offset += amount;
            session.input_bytes = session.input_bytes.saturating_sub(amount);
            if operation.remaining_len() == 0 {
                session.flushed_sequence = operation.sequence;
                notify_waiters(handle, session, notices, counters);
            } else {
                operation.reset_overlapped();
                submit_write_operation(handle, session, operation, notices, counters);
            }
        }
        ensure_write(iocp, handle, session, notices, counters);
    } else {
        session.cleanup_failed = true;
        send_notice(notices, Notice::BrokerLost(handle), counters);
    }
    if session.exit_status.is_some() {
        start_pseudoconsole_close(IocpSender(iocp), handle, session, closer, notices, counters);
    }
}

fn ensure_read(
    _iocp: HANDLE,
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    if !session.active
        || session.read.is_some()
        || session.paused
        || session.output_eof
        || session.output_total() >= session.output_capacity
    {
        return;
    }
    let capacity = (session.output_capacity - session.output_total()).min(BYTE_QUANTUM);
    let mut operation = IoOperation::read(capacity);
    counters.read_syscalls += 1;
    let submitted = unsafe {
        ReadFile(
            session.output_pipe.raw(),
            operation.buffer.as_mut_ptr(),
            capacity as u32,
            null_mut(),
            operation.overlapped_mut(),
        )
    };
    if submitted == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_IO_PENDING {
            if error == ERROR_BROKEN_PIPE {
                session.output_eof = true;
                refresh_output(handle, session, notices, counters);
            } else {
                session.cleanup_failed = true;
                session.output_eof = true;
                send_notice(notices, Notice::BrokerLost(handle), counters);
            }
            return;
        }
    }
    session.read = Some(operation);
}

fn ensure_write(
    _iocp: HANDLE,
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    if !session.active || session.write.is_some() || session.close_started {
        return;
    }
    let Some(input) = session.input.pop_front() else {
        return;
    };
    let operation = IoOperation::write(input.bytes, input.sequence);
    submit_write_operation(handle, session, operation, notices, counters);
}

fn submit_write_operation(
    handle: u64,
    session: &mut Session,
    mut operation: Pin<Box<IoOperation>>,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    counters.write_syscalls += 1;
    let submitted = unsafe {
        WriteFile(
            session.input_pipe.raw(),
            operation.remaining_ptr(),
            operation.remaining_len() as u32,
            null_mut(),
            operation.overlapped_mut(),
        )
    };
    if submitted == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
        session.write = Some(operation);
        fail_input_waiters(handle, session, notices, counters);
        session.write.take();
        session.input_bytes = 0;
        return;
    }
    session.write = Some(operation);
}

fn cancel_write(session: &Session) {
    let Some(operation) = session.write.as_ref() else {
        return;
    };
    if unsafe { CancelIoEx(session.input_pipe.raw(), operation.overlapped_ptr()) } == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_NOT_FOUND {
            let _ = error;
        }
    }
}

fn start_pseudoconsole_close(
    iocp: IocpSender,
    handle: u64,
    session: &mut Session,
    closer: &CloserPool,
    _notices: &SyncSender<Notice>,
    _counters: &mut RuntimeCounters,
) {
    if session.pseudoconsole_close_started
        || session.exit_status.is_none()
        || (session.paused && !session.close_started)
    {
        return;
    }
    let Some(pseudoconsole) = session.pseudoconsole.take() else {
        return;
    };
    let task = CloseTask {
        handle,
        pseudoconsole,
        iocp,
    };
    match closer.submit(task) {
        Ok(()) => session.pseudoconsole_close_started = true,
        Err(task) => {
            session.pseudoconsole = Some(task.pseudoconsole);
            // The bounded closer queue is transient backpressure, not an
            // infrastructure failure. A closer completion wakes IOCP and the
            // reactor retries this session.
        }
    }
}

fn fail_input_waiters(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    let first_failure = session.input_failed_from.is_none();
    let accepted_pending = session.input_bytes != 0
        || session
            .flush_waiters
            .values()
            .any(|sequence| *sequence > session.flushed_sequence);
    let failed_sequence = session.write.as_ref().map_or_else(
        || {
            session
                .input
                .front()
                .map_or(session.flushed_sequence.saturating_add(1), |input| {
                    input.sequence
                })
        },
        |operation| operation.sequence,
    );
    session.input_failed_from.get_or_insert(failed_sequence);
    session.input.clear();
    if session.write.is_none() {
        session.input_bytes = 0;
    }
    let waiters: Vec<_> = session
        .capacity_waiters
        .drain()
        .map(|(waiter, _)| waiter)
        .chain(session.flush_waiters.drain().map(|(waiter, _)| waiter))
        .collect();
    for waiter in waiters {
        send_notice(notices, Notice::WaitFailed { handle, waiter }, counters);
    }
    if first_failure && accepted_pending && session.active {
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
    let capacity: Vec<_> = session
        .capacity_waiters
        .iter()
        .filter_map(|(&waiter, &required)| (required <= available).then_some(waiter))
        .collect();
    for waiter in capacity {
        session.capacity_waiters.remove(&waiter);
        send_notice(notices, Notice::Capacity { handle, waiter }, counters);
    }
    let flush: Vec<_> = session
        .flush_waiters
        .iter()
        .filter_map(|(&waiter, &sequence)| (sequence <= session.flushed_sequence).then_some(waiter))
        .collect();
    for waiter in flush {
        session.flush_waiters.remove(&waiter);
        send_notice(notices, Notice::Flush { handle, waiter }, counters);
    }
}

fn refresh_output(
    handle: u64,
    session: &mut Session,
    notices: &SyncSender<Notice>,
    counters: &mut RuntimeCounters,
) {
    if !session.active {
        return;
    }
    let ready = session.output_bytes <= INTERACTIVE_BATCH
        || session.output_bytes >= OUTPUT_BATCH
        || session.output_eof
        || session
            .output_deadline
            .is_some_and(|deadline| deadline <= Instant::now());
    if ready && !session.output.is_empty() && !session.output_notified {
        let bytes = session.pull(OUTPUT_BATCH);
        session.output_notified = true;
        send_notice(notices, Notice::Output { handle, bytes }, counters);
    }
    if session.output_eof
        && session.output.is_empty()
        && session.output_outstanding == 0
        && !session.output_done_notified
    {
        session.output_done_notified = true;
        send_notice(notices, Notice::OutputDone(handle), counters);
    }
}

fn refresh_due_outputs(
    iocp: HANDLE,
    closer: &CloserPool,
    notices: &SyncSender<Notice>,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            refresh_output(handle, session, notices, counters);
            ensure_read(iocp, handle, session, notices, counters);
            if session.exit_status.is_some() {
                start_pseudoconsole_close(
                    IocpSender(iocp),
                    handle,
                    session,
                    closer,
                    notices,
                    counters,
                );
            }
        }
    }
}

fn output_timeout(sessions: &GenerationRegistry<Session>) -> u32 {
    let deadline = sessions
        .handles()
        .into_iter()
        .filter_map(|handle| sessions.get(handle))
        .filter(|session| !session.output_notified && !session.output.is_empty())
        .filter_map(|session| session.output_deadline)
        .min();
    let Some(deadline) = deadline else {
        return INFINITE;
    };
    let now = Instant::now();
    if deadline <= now {
        return 0;
    }
    deadline
        .duration_since(now)
        .as_millis()
        .max(1)
        .min(u128::from(u32::MAX - 1)) as u32
}

fn send_notice(notices: &SyncSender<Notice>, notice: Notice, counters: &mut RuntimeCounters) {
    if notices.send(notice).is_ok() {
        counters.notifications += 1;
    }
}

fn shutdown_all(sessions: &mut GenerationRegistry<Session>) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            unsafe {
                TerminateJobObject(session.job.raw(), 1);
            }
            cancel_write(session);
        }
    }
}
