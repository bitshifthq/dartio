mod handles;
mod spawn;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
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
const OUTPUT_DELAY: Duration = Duration::from_millis(1);
const COMMAND_QUANTUM: usize = 64;
const COMMAND_CAPACITY: usize = 1024;
const NOTICE_CAPACITY: usize = 4096;
const WAITER_CAPACITY: usize = 1024;
const SESSION_NOTICE_RESERVATIONS: usize = 4;
const CLOSE_ADMISSION_CAPACITY: usize = 128;
const QUARANTINED_IO_CAPACITY: usize = CLOSE_ADMISSION_CAPACITY * 2;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_KEY_TAG: usize = 1_usize << (usize::BITS - 1);
const CLOSE_KEY_TAG: usize = 1_usize << (usize::BITS - 2);
static QUARANTINED_IO_OPERATIONS: AtomicUsize = AtomicUsize::new(0);
static QUARANTINED_PSEUDOCONSOLES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Notice {
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
    close_permit: Option<ClosePermit>,
    notice_reservations: Vec<NoticeReservation>,
    pid: u32,
    size: [u32; 4],
    input_capacity: usize,
    input_bytes: usize,
    input: VecDeque<QueuedInput>,
    write: Option<Pin<Box<IoOperation>>>,
    capacity_waiters: HashMap<u64, (usize, NoticeReservation)>,
    flush_waiters: HashMap<u64, (u64, NoticeReservation)>,
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
    output_failed_notified: bool,
    read: Option<Pin<Box<IoOperation>>>,
    paused: bool,
    output_eof: bool,
    exit_status: Option<i64>,
    exit_notified: bool,
    close_started: bool,
    pseudoconsole_close_started: bool,
    pseudoconsole_close_done: bool,
    cleanup_failed: bool,
    broker_lost_notified: bool,
    active: bool,
    activation_deadline: Option<Instant>,
    abandoned: bool,
}

impl Session {
    fn from_spawned(
        spawned: spawn::SpawnedSession,
        close_permit: ClosePermit,
        notice_reservations: Vec<NoticeReservation>,
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
            close_permit: Some(close_permit),
            notice_reservations,
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
            output_failed: false,
            output_failed_notified: false,
            read: None,
            paused: true,
            output_eof: false,
            exit_status: None,
            exit_notified: false,
            close_started: false,
            pseudoconsole_close_started: false,
            pseudoconsole_close_done: false,
            cleanup_failed: false,
            broker_lost_notified: false,
            active: false,
            activation_deadline: Some(Instant::now() + ACTIVATION_TIMEOUT),
            abandoned: false,
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

    fn wait_capacity(
        &mut self,
        required: usize,
        waiter: u64,
        reservation: NoticeReservation,
    ) -> WaitResult {
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
        self.capacity_waiters
            .insert(waiter, (required, reservation));
        WaitResult::Armed
    }

    fn wait_flush(
        &mut self,
        sequence: u64,
        waiter: u64,
        reservation: NoticeReservation,
    ) -> WaitResult {
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
        self.flush_waiters.insert(waiter, (sequence, reservation));
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
        unsafe {
            TerminateJobObject(self.job.raw(), 1);
        }
        if let Some(read) = self.read.take() {
            quarantine_io_operation(read);
        }
        if let Some(write) = self.write.take() {
            quarantine_io_operation(write);
        }
        if let Some(pseudoconsole) = self.pseudoconsole.take() {
            quarantine_pseudoconsole(pseudoconsole, self.close_permit.take());
        }
    }
}

fn quarantine_io_operation(operation: Pin<Box<IoOperation>>) {
    // A canceled OVERLAPPED remains kernel-owned until its completion arrives.
    // The session admission cap bounds this shutdown-only quarantine.
    let previous = QUARANTINED_IO_OPERATIONS.fetch_add(1, Ordering::AcqRel);
    debug_assert!(previous < QUARANTINED_IO_CAPACITY);
    std::mem::forget(operation);
}

fn quarantine_pseudoconsole(pseudoconsole: OwnedPseudoConsole, permit: Option<ClosePermit>) {
    // Reaching this path means an isolated closer thread could not be created.
    // Retaining its permit makes repeated failures consume bounded admission.
    let previous = QUARANTINED_PSEUDOCONSOLES.fetch_add(1, Ordering::AcqRel);
    debug_assert!(previous < CLOSE_ADMISSION_CAPACITY);
    std::mem::forget(pseudoconsole);
    if let Some(permit) = permit {
        std::mem::forget(permit);
    }
}

enum Command {
    Add {
        session: Box<Session>,
        reply: Sender<io::Result<u64>>,
    },
    Activate {
        handle: u64,
        reply: Sender<io::Result<()>>,
    },
    Write {
        handle: u64,
        bytes: Vec<u8>,
        reply: Sender<i64>,
    },
    CreditAsync {
        handle: u64,
        bytes: usize,
    },
    WaitCapacity {
        handle: u64,
        required: usize,
        waiter: u64,
        reservation: NoticeReservation,
        reply: Sender<WaitResult>,
    },
    WaitFlush {
        handle: u64,
        sequence: u64,
        waiter: u64,
        reservation: NoticeReservation,
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
    Shutdown,
}

#[derive(Clone)]
struct IocpSender(Arc<OwnedHandle>);

unsafe impl Send for IocpSender {}
unsafe impl Sync for IocpSender {}

impl IocpSender {
    fn raw(&self) -> HANDLE {
        self.0.raw()
    }

    fn post_command(&self) -> io::Result<()> {
        if unsafe { PostQueuedCompletionStatus(self.raw(), 0, 0, null_mut()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

struct NoticeBudget {
    limit: usize,
    reserved: AtomicUsize,
}

impl NoticeBudget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserved: AtomicUsize::new(0),
        })
    }

    fn try_reserve(self: &Arc<Self>) -> Option<NoticeReservation> {
        self.reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                (reserved < self.limit).then_some(reserved + 1)
            })
            .ok()
            .map(|_| NoticeReservation {
                budget: Arc::clone(self),
                transferred: false,
            })
    }

    fn try_reserve_many(self: &Arc<Self>, count: usize) -> Option<Vec<NoticeReservation>> {
        (0..count)
            .map(|_| self.try_reserve())
            .collect::<Option<Vec<_>>>()
    }

    fn release(&self) {
        let previous = self.reserved.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0);
    }
}

struct NoticeReservation {
    budget: Arc<NoticeBudget>,
    transferred: bool,
}

impl NoticeReservation {
    fn transfer(mut self) {
        self.transferred = true;
    }
}

impl Drop for NoticeReservation {
    fn drop(&mut self) {
        if !self.transferred {
            self.budget.release();
        }
    }
}

#[derive(Clone)]
struct NoticeEmitter {
    sender: Sender<Notice>,
    budget: Arc<NoticeBudget>,
}

impl NoticeEmitter {
    fn try_reserve(&self) -> Option<NoticeReservation> {
        self.budget.try_reserve()
    }

    fn reserve_session(&self) -> Option<Vec<NoticeReservation>> {
        self.budget.try_reserve_many(SESSION_NOTICE_RESERVATIONS)
    }

    fn emit(
        &self,
        reservation: NoticeReservation,
        notice: Notice,
        counters: &mut RuntimeCounters,
    ) -> bool {
        // Every queued notice owns a reservation, so this nonblocking channel
        // contains at most NOTICE_CAPACITY values despite being unbounded.
        if self.sender.send(notice).is_err() {
            return false;
        }
        reservation.transfer();
        counters.notifications += 1;
        true
    }
}

pub(crate) struct NoticeReceiver {
    receiver: Receiver<Notice>,
    budget: Arc<NoticeBudget>,
    iocp: IocpSender,
}

impl NoticeReceiver {
    pub(crate) fn recv(&self) -> Result<Notice, mpsc::RecvError> {
        let notice = self.receiver.recv()?;
        self.budget.release();
        let _ = self.iocp.post_command();
        Ok(notice)
    }

    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Notice, mpsc::RecvTimeoutError> {
        let notice = self.receiver.recv_timeout(timeout)?;
        self.budget.release();
        let _ = self.iocp.post_command();
        Ok(notice)
    }
}

struct CloseAdmission {
    limit: usize,
    reserved: AtomicUsize,
}

impl CloseAdmission {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserved: AtomicUsize::new(0),
        })
    }

    fn try_reserve(self: &Arc<Self>) -> Option<ClosePermit> {
        self.reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                (reserved < self.limit).then_some(reserved + 1)
            })
            .ok()
            .map(|_| ClosePermit {
                admission: Arc::clone(self),
            })
    }
}

struct ClosePermit {
    admission: Arc<CloseAdmission>,
}

impl Drop for ClosePermit {
    fn drop(&mut self) {
        let previous = self.admission.reserved.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0);
    }
}

struct CloseTask {
    handle: u64,
    pseudoconsole: OwnedPseudoConsole,
    permit: ClosePermit,
    iocp: IocpSender,
}

#[derive(Clone)]
struct CloserPool {
    state: Arc<CloserState>,
}

struct CloserState {
    admission: Arc<CloseAdmission>,
    completed: Mutex<HashMap<u64, ClosePermit>>,
}

impl CloserPool {
    fn new() -> Self {
        Self {
            state: Arc::new(CloserState {
                admission: CloseAdmission::new(CLOSE_ADMISSION_CAPACITY),
                completed: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn try_reserve(&self) -> Option<ClosePermit> {
        self.state.admission.try_reserve()
    }

    fn take_completed(&self, handle: u64) -> Option<ClosePermit> {
        self.state
            .completed
            .lock()
            .ok()
            .and_then(|mut completed| completed.remove(&handle))
    }

    fn submit(&self, task: CloseTask) -> Result<(), CloseTask> {
        // One reserved thread is isolated per HPCON. A hung close consumes its
        // permit but cannot block another admitted session from closing.
        let shared = Arc::new(Mutex::new(Some(task)));
        let worker_task = Arc::clone(&shared);
        let state = Arc::clone(&self.state);
        let spawned = thread::Builder::new()
            .name("ptyx-conpty-closer".to_owned())
            .spawn(move || {
                let Some(task) = worker_task.lock().ok().and_then(|mut slot| slot.take()) else {
                    return;
                };
                task.pseudoconsole.close();
                if let Ok(mut completed) = state.completed.lock() {
                    completed.insert(task.handle, task.permit);
                } else {
                    std::mem::forget(task.permit);
                }
                unsafe {
                    PostQueuedCompletionStatus(
                        task.iocp.raw(),
                        0,
                        task.handle as usize | CLOSE_KEY_TAG,
                        null_mut(),
                    );
                }
            });
        if spawned.is_ok() {
            Ok(())
        } else {
            Err(shared
                .lock()
                .expect("close task lock is not poisoned")
                .take()
                .expect("failed worker spawn retains its close task"))
        }
    }
}

pub(crate) struct IntegratedRuntime {
    commands: SyncSender<Command>,
    notice_emitter: NoticeEmitter,
    notices: Mutex<Option<NoticeReceiver>>,
    iocp: IocpSender,
    closer: CloserPool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl IntegratedRuntime {
    pub(crate) fn try_new() -> io::Result<Self> {
        spawn::validate_windows_build()?;
        let iocp = Arc::new(OwnedHandle::new(unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1)
        })?);
        let iocp_sender = IocpSender(Arc::clone(&iocp));
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (notice_sender, notice_receiver) = mpsc::channel();
        let notice_budget = NoticeBudget::new(NOTICE_CAPACITY);
        let notice_emitter = NoticeEmitter {
            sender: notice_sender,
            budget: Arc::clone(&notice_budget),
        };
        let notices = NoticeReceiver {
            receiver: notice_receiver,
            budget: notice_budget,
            iocp: iocp_sender.clone(),
        };
        let closer = CloserPool::new();
        let thread = thread::Builder::new()
            .name("ptyx-windows-iocp".to_owned())
            .spawn({
                let iocp_sender = iocp_sender.clone();
                let notice_emitter = notice_emitter.clone();
                let closer = closer.clone();
                move || {
                    reactor(iocp, iocp_sender, command_receiver, notice_emitter, closer);
                }
            })?;
        Ok(Self {
            commands: command_sender,
            notice_emitter,
            notices: Mutex::new(Some(notices)),
            iocp: iocp_sender,
            closer,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub(crate) fn take_notifications(&self) -> Option<NoticeReceiver> {
        self.notices.lock().ok()?.take()
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
        let close_permit = self.closer.try_reserve().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "ConPTY close isolation capacity is exhausted",
            )
        })?;
        let notice_reservations = self.notice_emitter.reserve_session().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "native notification capacity is exhausted",
            )
        })?;
        let spawned = spawn::spawn(config)?;
        let session = Session::from_spawned(
            spawned,
            close_permit,
            notice_reservations,
            input_capacity,
            output_capacity,
        );
        self.request_result(|reply| Command::Add {
            session: Box::new(session),
            reply,
        })?
    }

    pub(crate) fn activate(&self, handle: u64) -> bool {
        self.request(|reply| Command::Activate { handle, reply })
            .is_ok()
    }

    pub(crate) fn try_write(&self, handle: u64, bytes: Vec<u8>) -> i64 {
        self.request(|reply| Command::Write {
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

    pub(crate) fn wait_capacity(&self, handle: u64, required: usize, waiter: u64) -> WaitResult {
        let Some(reservation) = self.notice_emitter.try_reserve() else {
            return WaitResult::Failed;
        };
        self.request(|reply| Command::WaitCapacity {
            handle,
            required,
            waiter,
            reservation,
            reply,
        })
    }

    pub(crate) fn wait_flush(&self, handle: u64, sequence: u64, waiter: u64) -> WaitResult {
        let Some(reservation) = self.notice_emitter.try_reserve() else {
            return WaitResult::Failed;
        };
        self.request(|reply| Command::WaitFlush {
            handle,
            sequence,
            waiter,
            reservation,
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

    pub(crate) fn exit_status(&self, handle: u64) -> Option<i64> {
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

    pub(crate) fn close(&self, handle: u64) -> bool {
        self.request(|reply| Command::Close { handle, reply })
    }

    pub(crate) fn destroy(&self, handle: u64) -> bool {
        self.request(|reply| Command::Destroy { handle, reply })
    }

    pub(crate) fn try_abandon(&self, handle: u64) -> bool {
        if self.commands.try_send(Command::Abandon { handle }).is_err() {
            return false;
        }
        let _ = self.iocp.post_command();
        true
    }

    fn request<R>(&self, command: impl FnOnce(Sender<R>) -> Command) -> R {
        self.request_result(command)
            .expect("ptyx IOCP request channel closed")
    }

    fn request_result<R>(&self, command: impl FnOnce(Sender<R>) -> Command) -> io::Result<R> {
        let (sender, receiver) = mpsc::channel();
        self.commands
            .send(command(sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx IOCP reactor stopped"))?;
        self.iocp.post_command()?;
        receiver
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx IOCP reactor stopped"))
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

fn reactor(
    iocp: Arc<OwnedHandle>,
    iocp_sender: IocpSender,
    commands: Receiver<Command>,
    notices: NoticeEmitter,
    closer: CloserPool,
) {
    let mut sessions = GenerationRegistry::<Session>::new();
    let mut counters = RuntimeCounters::default();
    loop {
        refresh_due_outputs(
            iocp.raw(),
            &iocp_sender,
            &closer,
            &notices,
            &mut sessions,
            &mut counters,
        );
        reap_abandoned(
            iocp.raw(),
            &iocp_sender,
            &closer,
            &notices,
            &mut sessions,
            &mut counters,
        );
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
                let permit = closer.take_completed(handle);
                if let Some(session) = sessions.get_mut(handle) {
                    session.close_permit = permit;
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
                    &iocp_sender,
                    &commands,
                    &closer,
                    &notices,
                    &mut sessions,
                    &mut counters,
                ) {
                    shutdown_all(&iocp_sender, &closer, &mut sessions);
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
                &iocp_sender,
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
            &iocp_sender,
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
    iocp_sender: &IocpSender,
    commands: &Receiver<Command>,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) -> bool {
    for _ in 0..COMMAND_QUANTUM {
        let Ok(command) = commands.try_recv() else {
            return false;
        };
        match command {
            Command::Add { session, reply } => {
                let handle = sessions.insert(*session);
                let result = associate_session(
                    &iocp_sender.0,
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
                    if let Some(session) = sessions.get_mut(handle) {
                        force_pseudoconsole_close(iocp_sender, handle, session, closer);
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
                    .and_then(|session| {
                        if session.abandoned || session.close_started {
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "session activation was abandoned",
                            ));
                        }
                        session.active = true;
                        session.activation_deadline = None;
                        if session.exit_status.is_some() && !session.exit_notified {
                            session.exit_notified = true;
                            send_lifecycle_notice(session, notices, Notice::Exit(handle), counters);
                        }
                        Ok(())
                    });
                if result.is_ok() {
                    if let Some(session) = sessions.get_mut(handle) {
                        ensure_read(iocp, handle, session, notices, counters);
                        ensure_write(iocp, handle, session, notices, counters);
                        refresh_output(handle, session, notices, counters);
                    }
                }
                let _ = reply.send(result);
            }
            Command::Write {
                handle,
                bytes,
                reply,
            } => {
                let sequence = match sessions.get_mut(handle) {
                    None => -1,
                    Some(session)
                        if session.close_started || session.input_failed_from.is_some() =>
                    {
                        -1
                    }
                    Some(session) => session
                        .try_write(bytes)
                        .ok()
                        .and_then(|sequence| i64::try_from(sequence).ok())
                        .unwrap_or(0),
                };
                if sequence > 0 {
                    if let Some(session) = sessions.get_mut(handle).filter(|value| value.active) {
                        ensure_write(iocp, handle, session, notices, counters);
                    }
                }
                let _ = reply.send(sequence);
            }
            Command::CreditAsync { handle, bytes } => {
                if let Some(session) = sessions.get_mut(handle) {
                    if session.credit(bytes) {
                        ensure_read(iocp, handle, session, notices, counters);
                        refresh_output(handle, session, notices, counters);
                    }
                }
            }
            Command::WaitCapacity {
                handle,
                required,
                waiter,
                reservation,
                reply,
            } => {
                let result = sessions
                    .get_mut(handle)
                    .map_or(WaitResult::Failed, |session| {
                        session.wait_capacity(required, waiter, reservation)
                    });
                let _ = reply.send(result);
            }
            Command::WaitFlush {
                handle,
                sequence,
                waiter,
                reservation,
                reply,
            } => {
                let result = sessions
                    .get_mut(handle)
                    .map_or(WaitResult::Failed, |session| {
                        session.wait_flush(sequence, waiter, reservation)
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
                        start_pseudoconsole_close(iocp_sender, handle, session, closer);
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
                    start_pseudoconsole_close(iocp_sender, handle, session, closer);
                    ensure_read(iocp, handle, session, notices, counters);
                    true
                });
                let _ = reply.send(closed);
            }
            Command::Destroy { handle, reply } => {
                let removable = sessions.get(handle).is_some_and(Session::terminal);
                let _ = reply.send(removable && sessions.remove(handle).is_some());
            }
            Command::Abandon { handle } => {
                if let Some(session) = sessions.get_mut(handle) {
                    abandon_session(
                        iocp,
                        iocp_sender,
                        handle,
                        session,
                        closer,
                        notices,
                        counters,
                    );
                }
            }
            Command::Shutdown => return true,
        }
    }
    let _ = iocp_sender.post_command();
    false
}

fn associate_session(
    iocp: &Arc<OwnedHandle>,
    handle: u64,
    session: &mut Session,
) -> io::Result<()> {
    for pipe in [&session.input_pipe, &session.output_pipe] {
        let associated =
            unsafe { CreateIoCompletionPort(pipe.raw(), iocp.raw(), handle as usize, 0) };
        if associated.is_null() {
            return Err(io::Error::last_os_error());
        }
    }
    session.process_wait = Some(OwnedProcessWait::register(
        session.process.raw(),
        Arc::clone(iocp),
        handle as usize | EXIT_KEY_TAG,
    )?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_process_exit(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    handle: u64,
    closer: &CloserPool,
    notices: &NoticeEmitter,
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
            session.exit_status = Some(i64::from(status));
            fail_input_waiters(handle, session, notices, counters);
            cancel_write(session);
        } else {
            session.cleanup_failed = true;
            notify_broker_lost(handle, session, notices, counters);
        }
    }
    if session.exit_status.is_some() && session.active && !session.exit_notified {
        session.exit_notified = true;
        send_lifecycle_notice(session, notices, Notice::Exit(handle), counters);
    }
    start_pseudoconsole_close(iocp_sender, handle, session, closer);
    ensure_read(iocp, handle, session, notices, counters);
}

#[allow(clippy::too_many_arguments)]
fn handle_io_completion(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    handle: u64,
    overlapped: *mut windows_sys::Win32::System::IO::OVERLAPPED,
    transferred: u32,
    error: Option<io::Error>,
    closer: &CloserPool,
    notices: &NoticeEmitter,
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
                session.output_eof = true;
                session.output_failed = true;
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
        notify_broker_lost(handle, session, notices, counters);
    }
    if session.exit_status.is_some() {
        start_pseudoconsole_close(iocp_sender, handle, session, closer);
    }
}

fn ensure_read(
    _iocp: HANDLE,
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
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
                session.output_eof = true;
                session.output_failed = true;
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
    notices: &NoticeEmitter,
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
    notices: &NoticeEmitter,
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
    iocp: &IocpSender,
    handle: u64,
    session: &mut Session,
    closer: &CloserPool,
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
    let Some(permit) = session.close_permit.take() else {
        quarantine_pseudoconsole(pseudoconsole, None);
        session.cleanup_failed = true;
        return;
    };
    let task = CloseTask {
        handle,
        pseudoconsole,
        permit,
        iocp: iocp.clone(),
    };
    match closer.submit(task) {
        Ok(()) => session.pseudoconsole_close_started = true,
        Err(task) => {
            session.pseudoconsole = Some(task.pseudoconsole);
            session.close_permit = Some(task.permit);
        }
    }
}

fn force_pseudoconsole_close(
    iocp: &IocpSender,
    handle: u64,
    session: &mut Session,
    closer: &CloserPool,
) {
    let exit_status = session.exit_status;
    session.exit_status = Some(exit_status.unwrap_or(1));
    session.paused = false;
    session.close_started = true;
    start_pseudoconsole_close(iocp, handle, session, closer);
    session.exit_status = exit_status;
}

fn fail_input_waiters(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
) {
    let first_failure = session.input_failed_from.is_none();
    let accepted_pending = session.input_bytes != 0
        || session
            .flush_waiters
            .values()
            .any(|(sequence, _)| *sequence > session.flushed_sequence);
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
        .map(|(waiter, (_, reservation))| (waiter, reservation))
        .chain(
            session
                .flush_waiters
                .drain()
                .map(|(waiter, (_, reservation))| (waiter, reservation)),
        )
        .collect();
    for (waiter, reservation) in waiters {
        let _ = notices.emit(reservation, Notice::WaitFailed { handle, waiter }, counters);
    }
    if first_failure && accepted_pending && session.active {
        send_lifecycle_notice(session, notices, Notice::InputFailed(handle), counters);
    }
}

fn notify_waiters(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
) {
    let available = session.input_capacity - session.input_bytes;
    let capacity: Vec<_> = session
        .capacity_waiters
        .iter()
        .filter_map(|(&waiter, (required, _))| (*required <= available).then_some(waiter))
        .collect();
    for waiter in capacity {
        if let Some((_, reservation)) = session.capacity_waiters.remove(&waiter) {
            let _ = notices.emit(reservation, Notice::Capacity { handle, waiter }, counters);
        }
    }
    let flush: Vec<_> = session
        .flush_waiters
        .iter()
        .filter_map(|(&waiter, (sequence, _))| {
            (*sequence <= session.flushed_sequence).then_some(waiter)
        })
        .collect();
    for waiter in flush {
        if let Some((_, reservation)) = session.flush_waiters.remove(&waiter) {
            let _ = notices.emit(reservation, Notice::Flush { handle, waiter }, counters);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputTerminalNotice {
    Done,
    Failed,
}

fn output_terminal_notice(
    output_eof: bool,
    output_failed: bool,
    output_bytes: usize,
    output_outstanding: usize,
    output_done_notified: bool,
    output_failed_notified: bool,
) -> Option<OutputTerminalNotice> {
    if !output_eof || output_bytes != 0 || output_outstanding != 0 {
        return None;
    }
    if output_failed && !output_failed_notified {
        Some(OutputTerminalNotice::Failed)
    } else if !output_failed && !output_done_notified {
        Some(OutputTerminalNotice::Done)
    } else {
        None
    }
}

fn refresh_output(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
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
        if let Some(reservation) = notices.try_reserve() {
            let bytes = session.pull(OUTPUT_BATCH);
            if notices.emit(reservation, Notice::Output { handle, bytes }, counters) {
                session.output_notified = true;
            } else {
                session.cleanup_failed = true;
            }
        }
    }
    match output_terminal_notice(
        session.output_eof,
        session.output_failed,
        session.output_bytes,
        session.output_outstanding,
        session.output_done_notified,
        session.output_failed_notified,
    ) {
        Some(OutputTerminalNotice::Failed) => {
            session.output_failed_notified = true;
            send_lifecycle_notice(session, notices, Notice::OutputFailed(handle), counters);
        }
        Some(OutputTerminalNotice::Done) => {
            session.output_done_notified = true;
            send_lifecycle_notice(session, notices, Notice::OutputDone(handle), counters);
        }
        None => {}
    }
}

fn refresh_due_outputs(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            refresh_output(handle, session, notices, counters);
            ensure_read(iocp, handle, session, notices, counters);
            if session.exit_status.is_some() {
                start_pseudoconsole_close(iocp_sender, handle, session, closer);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn abandon_session(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    handle: u64,
    session: &mut Session,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
) {
    session.abandoned = true;
    session.active = false;
    session.activation_deadline = None;
    if !session.close_started {
        session.close_started = true;
        session.paused = false;
        session.input.clear();
        session.input_bytes = 0;
        session.capacity_waiters.clear();
        session.flush_waiters.clear();
        session.output.clear();
        session.output_bytes = 0;
        session.output_outstanding = 0;
        unsafe {
            TerminateJobObject(session.job.raw(), 1);
        }
        cancel_write(session);
    }
    start_pseudoconsole_close(iocp_sender, handle, session, closer);
    ensure_read(iocp, handle, session, notices, counters);
}

#[allow(clippy::too_many_arguments)]
fn reap_abandoned(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            if !session.active
                && !session.abandoned
                && session
                    .activation_deadline
                    .is_some_and(|deadline| deadline <= Instant::now())
            {
                abandon_session(
                    iocp,
                    iocp_sender,
                    handle,
                    session,
                    closer,
                    notices,
                    counters,
                );
            }
        }
        let removable = sessions
            .get(handle)
            .is_some_and(|session| session.abandoned && session.terminal());
        if removable {
            sessions.remove(handle);
        }
    }
}

fn output_timeout(sessions: &GenerationRegistry<Session>) -> u32 {
    let output_deadline = sessions
        .handles()
        .into_iter()
        .filter_map(|handle| sessions.get(handle))
        .filter(|session| !session.output_notified && !session.output.is_empty())
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

fn send_lifecycle_notice(
    session: &mut Session,
    notices: &NoticeEmitter,
    notice: Notice,
    counters: &mut RuntimeCounters,
) {
    let Some(reservation) = session.notice_reservations.pop() else {
        session.cleanup_failed = true;
        return;
    };
    if !notices.emit(reservation, notice, counters) {
        session.cleanup_failed = true;
    }
}

fn notify_broker_lost(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
) {
    if session.broker_lost_notified {
        return;
    }
    session.broker_lost_notified = true;
    send_lifecycle_notice(session, notices, Notice::BrokerLost(handle), counters);
}

fn shutdown_all(
    iocp: &IocpSender,
    closer: &CloserPool,
    sessions: &mut GenerationRegistry<Session>,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            unsafe {
                TerminateJobObject(session.job.raw(), 1);
            }
            cancel_write(session);
            force_pseudoconsole_close(iocp, handle, session, closer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{output_terminal_notice, CloseAdmission, NoticeBudget, OutputTerminalNotice};

    #[test]
    fn notice_budget_rejects_reservations_beyond_its_limit() {
        let budget = NoticeBudget::new(2);
        let first = budget.try_reserve();
        let second = budget.try_reserve();

        let third = budget.try_reserve();

        assert!(first.is_some());
        assert!(second.is_some());
        assert!(third.is_none());
    }

    #[test]
    fn notice_reservation_release_restores_capacity() {
        let budget = NoticeBudget::new(1);
        let reservation = budget.try_reserve().expect("reservation is available");
        drop(reservation);

        let replacement = budget.try_reserve();

        assert!(replacement.is_some());
    }

    #[test]
    fn close_admission_isolates_four_hung_closes() {
        let admission = CloseAdmission::new(5);
        let _first = admission.try_reserve().expect("first close is admitted");
        let _second = admission.try_reserve().expect("second close is admitted");
        let _third = admission.try_reserve().expect("third close is admitted");
        let _fourth = admission.try_reserve().expect("fourth close is admitted");

        let fifth = admission.try_reserve();

        assert!(fifth.is_some());
    }

    #[test]
    fn close_admission_rejects_spawn_when_every_slot_is_reserved() {
        let admission = CloseAdmission::new(1);
        let _reserved = admission.try_reserve().expect("close is admitted");

        let exhausted = admission.try_reserve();

        assert!(exhausted.is_none());
    }

    #[test]
    fn output_failure_waits_for_outstanding_bytes() {
        let notice = output_terminal_notice(true, true, 0, 1, false, false);

        assert_eq!(notice, None);
    }

    #[test]
    fn output_failure_emits_after_every_byte_drains() {
        let notice = output_terminal_notice(true, true, 0, 0, false, false);

        assert_eq!(notice, Some(OutputTerminalNotice::Failed));
    }
}
