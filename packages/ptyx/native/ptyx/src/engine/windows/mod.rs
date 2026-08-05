mod handles;
mod spawn;

use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::io;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::ResizePseudoConsole;
use windows_sys::Win32::System::JobObjects::TerminateJobObject;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, RegisterWaitForSingleObject, UnregisterWaitEx, WaitForSingleObject,
    INFINITE, WT_EXECUTEONLYONCE,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatus,
    PostQueuedCompletionStatus,
};

use self::handles::{IoOperation, OwnedHandle, OwnedPseudoConsole};
use crate::engine::control::{enqueue_control_or_fallback, Control, ControlQueue, WakeGate};
use crate::engine::event::{self, Receiver as EventReceiver, Sender as EventSender};
use crate::engine::oneshot::{self, Sender as ReplySender};
#[cfg(any(feature = "__private_adapter", test))]
use crate::engine::session::AdmissionResult;
use crate::engine::session::{validate_capacities, InputAdmission, SessionCore};
use crate::engine::spawn::BrokerSpawn;
#[cfg(feature = "__private_adapter")]
use crate::engine::Failure;
use crate::engine::{CloseResult, Completion, GenerationRegistry, Notice};
use crate::error::{FailureKind, Operation, OperationError, WriteError, WriteErrorKind};

const BYTE_QUANTUM: usize = 64 * 1024;
const INTERACTIVE_BATCH: usize = 256;
const OUTPUT_BATCH: usize = 128 * 1024;
const OUTPUT_DELAY: Duration = Duration::from_millis(1);
const COMMAND_QUANTUM: usize = 64;
const COMMAND_CAPACITY: usize = 1024;
const NOTICE_CAPACITY: usize = 4096;
const SESSION_NOTICE_RESERVATIONS: usize = 5;
const CLOSE_ADMISSION_CAPACITY: usize = 128;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_KEY_TAG: usize = 1_usize << (usize::BITS - 2);
const NOTICE_AVAILABLE_KEY: usize = 1_usize << (usize::BITS - 1);
const PROCESS_EXIT_KEY_TAG: usize = 1_usize << (usize::BITS - 3);
#[cfg(any(feature = "__private_adapter", test))]
const WRITE_INFRASTRUCTURE_FAILURE: i64 = -2;

const fn input_closed() -> OperationError {
    OperationError::new(Operation::Write, FailureKind::Closed, None)
}

const fn infrastructure_failure(operation: Operation) -> OperationError {
    OperationError::new(operation, FailureKind::InfrastructureLost, None)
}

fn retain_cleanup_failure(session: &mut Session, failure: OperationError) {
    session.cleanup_failure.get_or_insert(failure);
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

struct Session {
    core: SessionCore,
    input_pipe: OwnedHandle,
    output_pipe: OwnedHandle,
    pseudoconsole: Option<OwnedPseudoConsole>,
    process_wait: Option<ProcessWait>,
    process: OwnedHandle,
    job: OwnedHandle,
    close_permit: Option<ClosePermit>,
    notice_reservations: Vec<NoticeReservation>,
    pid: u32,
    size: [u32; 4],
    write: Option<Pin<Box<IoOperation>>>,
    output_notification_blocked: bool,
    output_failed_notified: bool,
    read: Option<Pin<Box<IoOperation>>>,
    exit_notified: bool,
    exit_failure: Option<OperationError>,
    pseudoconsole_close_started: bool,
    pseudoconsole_close_done: bool,
    broker_lost_notified: bool,
}

impl Session {
    fn from_spawned(
        spawned: spawn::SpawnedSession,
        close_permit: ClosePermit,
        notice_reservations: Vec<NoticeReservation>,
        admission: Arc<InputAdmission>,
        output_capacity: usize,
        _graceful_close_timeout: Duration,
    ) -> Self {
        let mut core = SessionCore::new(admission, output_capacity);
        core.activation_deadline = Some(Instant::now() + ACTIVATION_TIMEOUT);
        Self {
            core,
            input_pipe: spawned.input,
            output_pipe: spawned.output,
            pseudoconsole: Some(spawned.pseudoconsole),
            process_wait: None,
            process: spawned.process,
            job: spawned.job,
            close_permit: Some(close_permit),
            notice_reservations,
            pid: spawned.pid,
            size: spawned.size,
            write: None,
            output_notification_blocked: false,
            output_failed_notified: false,
            read: None,
            exit_notified: false,
            exit_failure: None,
            pseudoconsole_close_started: false,
            pseudoconsole_close_done: false,
            broker_lost_notified: false,
        }
    }

    fn terminal(&self) -> bool {
        self.exit_status.is_some()
            && self.output_eof
            && !self.has_output()
            && self.output_outstanding == 0
            && self.read.is_none()
            && self.write.is_none()
            && self.pseudoconsole_close_done
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

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.job.raw(), 1);
        }
        cancel_read(self);
        cancel_write(self);
        if let Some(read) = self.read.take() {
            wait_for_cancelled_io(self.output_pipe.raw(), read);
        }
        if let Some(write) = self.write.take() {
            wait_for_cancelled_io(self.input_pipe.raw(), write);
        }
        if let Some(pseudoconsole) = self.pseudoconsole.take() {
            close_pseudoconsole_fallback(pseudoconsole, self.close_permit.take());
        }
    }
}

fn wait_for_cancelled_io(pipe: HANDLE, operation: Pin<Box<IoOperation>>) {
    // The reactor owns the final session drop. Waiting here observes the
    // terminal completion before releasing the OVERLAPPED allocation, so a
    // shutdown cannot leak kernel-owned I/O state or retain an unbounded
    // quarantine. Cancellation has already been requested by the caller.
    let mut transferred = 0;
    unsafe {
        GetOverlappedResult(pipe, operation.overlapped_ptr(), &mut transferred, 1);
    }
    drop(operation);
}

fn close_pseudoconsole_fallback(pseudoconsole: OwnedPseudoConsole, permit: Option<ClosePermit>) {
    // The supported Windows floor guarantees that ClosePseudoConsole is
    // nonblocking. Use it directly when an isolated closer cannot be
    // provisioned; retaining an HPCON would leak process-owned state.
    pseudoconsole.close();
    drop(permit);
}

enum Command {
    Add {
        session: Box<Session>,
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
    Resize {
        handle: u64,
        size: [u32; 4],
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
    Abandon {
        handle: u64,
    },
    Shutdown,
}

struct ReactorCommands {
    receiver: Receiver<Command>,
    // A producer posts the coalesced IOCP wake before publishing a write while
    // holding this lock. The reactor takes it before clearing that wake, so it
    // cannot observe the wake before the corresponding command is visible.
    submission: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct IocpSender(Arc<OwnedHandle>, Arc<WakeGate>);

unsafe impl Send for IocpSender {}
unsafe impl Sync for IocpSender {}

impl IocpSender {
    fn raw(&self) -> HANDLE {
        self.0.raw()
    }

    fn post_command(&self) -> io::Result<()> {
        if self.1.request() {
            if let Err(error) = self.post_key(0) {
                self.1.clear();
                return Err(error);
            }
        }
        Ok(())
    }

    fn post_notice_available(&self) -> io::Result<()> {
        self.post_key(NOTICE_AVAILABLE_KEY)
    }

    fn post_key(&self, key: usize) -> io::Result<()> {
        if unsafe { PostQueuedCompletionStatus(self.raw(), 0, key, null_mut()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clear_command(&self) {
        self.1.clear();
    }
}

struct ProcessWaitContext {
    iocp: IocpSender,
    handle: u64,
}

struct ProcessWait {
    wait: HANDLE,
    context: Option<Box<ProcessWaitContext>>,
}

// Registered wait handles and their heap-stable callback contexts may be
// transferred between Windows threads. Drop synchronously unregisters before
// releasing the context.
unsafe impl Send for ProcessWait {}

impl ProcessWait {
    fn register(process: HANDLE, handle: u64, iocp: &IocpSender) -> io::Result<Self> {
        let mut context = Box::new(ProcessWaitContext {
            iocp: iocp.clone(),
            handle,
        });
        let mut wait = null_mut();
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                process,
                Some(process_wait_callback),
                (&mut *context as *mut ProcessWaitContext).cast(),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if registered == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                wait,
                context: Some(context),
            })
        }
    }
}

impl Drop for ProcessWait {
    fn drop(&mut self) {
        if unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) } == 0 {
            // A failed unregister cannot prove that the callback released its
            // raw context pointer. Leak the small context instead of risking
            // use-after-free during exceptional teardown.
            if let Some(context) = self.context.take() {
                std::mem::forget(context);
            }
        }
    }
}

unsafe extern "system" fn process_wait_callback(context: *mut c_void, _timed_out: bool) {
    let context = unsafe { &*(context.cast::<ProcessWaitContext>()) };
    let _ = context
        .iocp
        .post_key(context.handle as usize | PROCESS_EXIT_KEY_TAG);
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
    sender: EventSender<Notice>,
    budget: Arc<NoticeBudget>,
}

impl NoticeEmitter {
    fn retire_session(&self, session: u64) {
        self.sender.retire_session(session);
    }

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
        if self.sender.send(notice.handle(), notice).is_err() {
            return false;
        }
        reservation.transfer();
        counters.notifications += 1;
        true
    }

    #[cfg(feature = "__private_adapter")]
    fn emit_without_metrics(&self, reservation: NoticeReservation, notice: Notice) -> bool {
        if self.sender.send(notice.handle(), notice).is_err() {
            return false;
        }
        reservation.transfer();
        true
    }
}

pub struct NoticeReceiver {
    receiver: EventReceiver<Notice>,
    budget: Arc<NoticeBudget>,
    iocp: IocpSender,
}

pub struct SessionNoticeReceiver {
    receiver: crate::engine::SessionReceiver<Notice>,
    budget: Arc<NoticeBudget>,
    iocp: IocpSender,
}

impl NoticeReceiver {
    #[cfg(feature = "__private_adapter")]
    pub fn recv(&self) -> Option<(u64, Notice)> {
        let notice = self.receiver.recv()?;
        self.budget.release();
        let _ = self.iocp.post_notice_available();
        Some(notice)
    }

    pub fn session(&self, handle: u64) -> Option<SessionNoticeReceiver> {
        Some(SessionNoticeReceiver {
            receiver: self.receiver.session(handle)?,
            budget: Arc::clone(&self.budget),
            iocp: self.iocp.clone(),
        })
    }
}

impl SessionNoticeReceiver {
    pub fn close(&self) -> Vec<Notice> {
        let notices = self.receiver.close();
        for _ in 0..notices.len() {
            self.budget.release();
        }
        if !notices.is_empty() {
            let _ = self.iocp.post_notice_available();
        }
        notices
    }

    pub fn recv(&self) -> Option<Notice> {
        let notice = self.receiver.recv()?;
        self.budget.release();
        let _ = self.iocp.post_notice_available();
        Some(notice)
    }

    pub fn try_recv(&self) -> Result<Option<Notice>, crate::engine::ReceiverClosed> {
        let notice = self.receiver.try_recv()?;
        if notice.is_some() {
            self.budget.release();
            let _ = self.iocp.post_notice_available();
        }
        Ok(notice)
    }

    pub fn poll_recv(&self, context: &mut Context<'_>) -> Poll<Option<Notice>> {
        match self.receiver.poll_recv(context) {
            Poll::Ready(Some(notice)) => {
                self.budget.release();
                let _ = self.iocp.post_notice_available();
                Poll::Ready(Some(notice))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for SessionNoticeReceiver {
    fn drop(&mut self) {
        drop(self.close());
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
        reservation: NoticeReservation,
    },
}

struct SpawnPool {
    sender: Mutex<Option<SyncSender<SpawnTask>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SpawnPool {
    fn new(
        commands: SyncSender<Command>,
        iocp: IocpSender,
        closer: CloserPool,
        notices: NoticeEmitter,
        controls: Arc<ControlQueue>,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<SpawnTask>(64);
        let thread = thread::Builder::new()
            .name("ptyx-spawn".to_owned())
            .spawn(move || {
                while let Ok(task) = receiver.recv() {
                    let result = stage_spawn(
                        &commands,
                        &iocp,
                        &closer,
                        &notices,
                        task.config,
                        task.input_capacity,
                        task.output_capacity,
                    );
                    let staged = result.as_ref().ok().copied();
                    let delivered = match task.target {
                        SpawnTarget::Completion(reply) => reply.send(result),
                        #[cfg(feature = "__private_adapter")]
                        SpawnTarget::Notice {
                            request,
                            reservation,
                        } => {
                            let notice = match result {
                                Ok(handle) => Notice::SpawnReady { request, handle },
                                Err(error) => Notice::SpawnFailed {
                                    request,
                                    failure: Failure::from(&error),
                                },
                            };
                            notices.emit_without_metrics(reservation, notice)
                        }
                    };
                    if !delivered {
                        if let Some(handle) = staged {
                            while let Ok(false) =
                                enqueue_abandon(&commands, &controls, handle, || {
                                    iocp.post_command().is_ok()
                                })
                            {
                                thread::yield_now();
                            }
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

    fn drain_completed(&self) -> Vec<(u64, ClosePermit)> {
        self.state
            .completed
            .lock()
            .map(|mut completed| completed.drain().collect())
            .unwrap_or_default()
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
                    // A poisoned completion registry cannot be repaired, but
                    // the permit is still owned by this worker and must be
                    // returned so another session is not permanently denied
                    // close admission.
                    drop(task.permit);
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

pub struct IntegratedRuntime {
    commands: SyncSender<Command>,
    command_submission: Arc<Mutex<()>>,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    #[cfg(feature = "__private_adapter")]
    notice_emitter: Mutex<Option<NoticeEmitter>>,
    notices: Mutex<Option<NoticeReceiver>>,
    iocp: IocpSender,
    controls: Arc<ControlQueue>,
    spawn_pool: SpawnPool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl IntegratedRuntime {
    pub fn try_new() -> io::Result<Self> {
        spawn::validate_windows_build()?;
        let iocp = Arc::new(OwnedHandle::new(unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1)
        })?);
        let iocp_sender = IocpSender(Arc::clone(&iocp), Arc::new(WakeGate::new()));
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let command_submission = Arc::new(Mutex::new(()));
        let (notice_sender, notice_receiver) = event::channel();
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
        let controls = Arc::new(ControlQueue::new());
        let spawn_pool = SpawnPool::new(
            command_sender.clone(),
            iocp_sender.clone(),
            closer.clone(),
            notice_emitter.clone(),
            Arc::clone(&controls),
        )?;
        let admissions = Arc::new(Mutex::new(HashMap::new()));
        let thread = thread::Builder::new()
            .name("ptyx-windows-iocp".to_owned())
            .spawn({
                let iocp_sender = iocp_sender.clone();
                let notice_emitter = notice_emitter.clone();
                let closer = closer.clone();
                let admissions = Arc::clone(&admissions);
                let controls = Arc::clone(&controls);
                let command_submission = Arc::clone(&command_submission);
                move || {
                    reactor(
                        iocp,
                        iocp_sender,
                        ReactorCommands {
                            receiver: command_receiver,
                            submission: command_submission,
                        },
                        notice_emitter,
                        closer,
                        admissions,
                        controls,
                    );
                }
            })?;
        Ok(Self {
            commands: command_sender,
            command_submission,
            admissions,
            #[cfg(feature = "__private_adapter")]
            notice_emitter: Mutex::new(Some(notice_emitter)),
            notices: Mutex::new(Some(notices)),
            iocp: iocp_sender,
            controls,
            spawn_pool,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub fn take_notifications(&self) -> Option<NoticeReceiver> {
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
        let emitter = self
            .notice_emitter
            .lock()
            .map_err(|_| io::Error::other("notification emitter lock poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "runtime is shutting down"))?;
        let reservation = emitter.try_reserve().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "native notification capacity is exhausted",
            )
        })?;
        self.spawn_pool.submit(SpawnTask {
            config,
            input_capacity,
            output_capacity,
            target: SpawnTarget::Notice {
                request,
                reservation,
            },
        })
    }

    pub fn activate(&self, handle: u64) -> bool {
        self.request_result(|reply| Command::Activate { handle, reply })
            .and_then(|result| result)
            .is_ok()
    }

    pub(crate) fn write(&self, handle: u64, bytes: Bytes) -> Result<(), WriteError> {
        let _submission = match self.command_submission.lock() {
            Ok(submission) => submission,
            Err(_) => {
                return Err(WriteError::new(
                    WriteErrorKind::Infrastructure,
                    bytes,
                    Some(infrastructure_failure(Operation::Write)),
                ));
            }
        };
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
                    Some(infrastructure_failure(Operation::Write)),
                ));
            }
        };
        admit_owned_write(
            &self.commands,
            || self.iocp.post_command(),
            handle,
            bytes,
            admission,
        )
    }

    #[cfg(feature = "__private_adapter")]
    pub fn write_copy(&self, handle: u64, bytes: &[u8]) -> Result<(), OperationError> {
        let _submission = match try_command_submission(&self.command_submission) {
            Ok(submission) => submission,
            Err(0) => {
                return Err(OperationError::new(
                    Operation::Write,
                    FailureKind::Backpressure,
                    None,
                ))
            }
            Err(_) => {
                return Err(infrastructure_failure(Operation::Write));
            }
        };
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
                return Err(infrastructure_failure(Operation::Write));
            }
        };
        let result = admit_write_with(
            &self.commands,
            || self.iocp.post_command(),
            handle,
            bytes.len(),
            Arc::clone(&admission),
            || Bytes::copy_from_slice(bytes),
        );
        match result {
            AdmissionResult::Accepted => Ok(()),
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
            AdmissionResult::Infrastructure => Err(infrastructure_failure(Operation::Write)),
        }
    }

    pub fn credit_async(&self, handle: u64, bytes: usize) -> bool {
        let queued = self.controls.push(Control::Credit { handle, bytes });
        if queued {
            let _ = self.iocp.post_command();
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

    pub fn terminal_mode(&self, _handle: u64) -> Result<[bool; 3], OperationError> {
        Err(OperationError::new(
            Operation::TerminalMode,
            FailureKind::Unsupported,
            None,
        ))
    }

    pub fn terminal_name(&self, _handle: u64) -> Result<Vec<u8>, OperationError> {
        Err(OperationError::new(
            Operation::TerminalName,
            FailureKind::Unsupported,
            None,
        ))
    }

    pub fn resize(&self, handle: u64, size: [u32; 4]) -> Result<(), OperationError> {
        self.request_operation(Operation::Resize, |reply| Command::Resize {
            handle,
            size,
            reply,
        })?
    }

    pub fn observe_mode(&self, _handle: u64, _observe: bool) -> Result<(), OperationError> {
        Err(OperationError::new(
            Operation::TerminalMode,
            FailureKind::Unsupported,
            None,
        ))
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
        matches!(
            enqueue_abandon(&self.commands, &self.controls, handle, || {
                self.iocp.post_command().is_ok()
            }),
            Ok(true)
        )
    }

    fn request_result<R>(&self, command: impl FnOnce(ReplySender<R>) -> Command) -> io::Result<R> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(command(sender))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx IOCP reactor stopped"))?;
        self.iocp.post_command()?;
        receiver
            .recv()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx IOCP reactor stopped"))
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

fn enqueue_abandon(
    commands: &SyncSender<Command>,
    controls: &ControlQueue,
    handle: u64,
    wake: impl FnOnce() -> bool,
) -> Result<bool, ()> {
    enqueue_control_or_fallback(
        commands,
        controls,
        Control::Abandon { handle },
        Command::Abandon { handle },
        wake,
    )
}

impl Drop for IntegratedRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl IntegratedRuntime {
    pub fn shutdown(&self) -> bool {
        let _ = self.commands.send(Command::Shutdown);
        let _ = self.iocp.post_command();
        let reactor = self
            .thread
            .lock()
            .ok()
            .and_then(|mut value| value.take())
            .is_none_or(|thread| thread.join().is_ok());
        #[cfg(feature = "__private_adapter")]
        if let Ok(mut emitter) = self.notice_emitter.lock() {
            emitter.take();
        }
        let spawn = self.spawn_pool.shutdown();
        spawn && reactor
    }
}

#[cfg(any(feature = "__private_adapter", test))]
fn try_command_submission(submission: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, i64> {
    match submission.try_lock() {
        Ok(submission) => Ok(submission),
        Err(std::sync::TryLockError::WouldBlock) => Err(0),
        Err(std::sync::TryLockError::Poisoned(_)) => Err(WRITE_INFRASTRUCTURE_FAILURE),
    }
}

#[cfg(any(feature = "__private_adapter", test))]
fn admit_write_with(
    commands: &SyncSender<Command>,
    wake: impl FnOnce() -> io::Result<()>,
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
    if wake().is_err() {
        state.open = false;
        state
            .failure
            .get_or_insert(infrastructure_failure(Operation::Write));
        return AdmissionResult::Infrastructure;
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
                state
                    .failure
                    .get_or_insert(infrastructure_failure(Operation::Write));
                AdmissionResult::Infrastructure
            }
        };
    }
    drop(state);
    AdmissionResult::Accepted
}

fn admit_owned_write(
    commands: &SyncSender<Command>,
    wake: impl FnOnce() -> io::Result<()>,
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
                Some(infrastructure_failure(Operation::Write)),
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
    if wake().is_err() {
        let failure = infrastructure_failure(Operation::Write);
        state.failure.get_or_insert(failure);
        state.open = false;
        return Err(WriteError::new(
            WriteErrorKind::Infrastructure,
            bytes,
            Some(failure),
        ));
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
            let failure = infrastructure_failure(Operation::Write);
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

fn stage_spawn(
    commands: &SyncSender<Command>,
    iocp: &IocpSender,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    config: BrokerSpawn,
    input_capacity: usize,
    output_capacity: usize,
) -> io::Result<u64> {
    let close_permit = closer.try_reserve().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "ConPTY close isolation capacity is exhausted",
        )
    })?;
    let notice_reservations = notices.reserve_session().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "native notification capacity is exhausted",
        )
    })?;
    let graceful_close_timeout = config.graceful_close_timeout;
    let spawned = spawn::spawn(config)?;
    let admission = Arc::new(InputAdmission::new(input_capacity));
    let session = Session::from_spawned(
        spawned,
        close_permit,
        notice_reservations,
        admission,
        output_capacity,
        graceful_close_timeout,
    );
    let (reply, receiver) = oneshot::channel();
    commands
        .send(Command::Add {
            session: Box::new(session),
            reply,
        })
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))?;
    iocp.post_command()?;
    receiver
        .recv()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ptyx reactor stopped"))?
}

fn reactor(
    iocp: Arc<OwnedHandle>,
    iocp_sender: IocpSender,
    commands: ReactorCommands,
    notices: NoticeEmitter,
    closer: CloserPool,
    admissions: Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
    controls: Arc<ControlQueue>,
) {
    let mut sessions = GenerationRegistry::<Session>::new();
    let mut counters = RuntimeCounters::default();
    let mut control_scratch = VecDeque::new();
    loop {
        collect_completed_closes(&closer, &mut sessions);
        refresh_due_outputs(
            iocp.raw(),
            &iocp_sender,
            &closer,
            &notices,
            &mut sessions,
            &mut counters,
        );
        reap_closed(&notices, &mut sessions, &mut counters, &admissions);
        reap_abandoned(
            iocp.raw(),
            &iocp_sender,
            &closer,
            &notices,
            &mut sessions,
            &mut counters,
            &admissions,
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
            if key == NOTICE_AVAILABLE_KEY {
                for handle in sessions.handles() {
                    if let Some(session) = sessions.get_mut(handle) {
                        session.output_notification_blocked = false;
                    }
                }
                continue;
            }
            if key & PROCESS_EXIT_KEY_TAG != 0 {
                let handle = (key & !PROCESS_EXIT_KEY_TAG) as u64;
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
            if key == 0 {
                let _submission = commands
                    .submission
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                iocp_sender.clear_command();
                counters.command_wakeups += 1;
                process_controls(
                    iocp.raw(),
                    &iocp_sender,
                    &controls,
                    &mut control_scratch,
                    &closer,
                    &notices,
                    &mut sessions,
                    &mut counters,
                );
                if process_commands(
                    iocp.raw(),
                    &iocp_sender,
                    &commands.receiver,
                    &closer,
                    &notices,
                    &mut sessions,
                    &mut counters,
                    &admissions,
                ) {
                    shutdown_all(&iocp_sender, &closer, &mut sessions, &admissions);
                    return;
                }
                continue;
            }
        }
        counters.reactor_events += 1;
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
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) -> bool {
    for _ in 0..COMMAND_QUANTUM {
        let Ok(command) = commands.try_recv() else {
            return false;
        };
        match command {
            Command::Add { session, reply } => {
                let admission = Arc::clone(&session.admission);
                let handle = sessions.insert(*session);
                let mut result = associate_session(
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
                } else if let Some(session) = sessions.get_mut(handle) {
                    match ProcessWait::register(session.process.raw(), handle, iocp_sender) {
                        Ok(wait) => session.process_wait = Some(wait),
                        Err(error) => {
                            unsafe {
                                TerminateJobObject(session.job.raw(), 1);
                            }
                            result = Err(error);
                        }
                    }
                }
                if result.is_err() && sessions.get(handle).is_some() {
                    if let Some(session) = sessions.get_mut(handle) {
                        force_pseudoconsole_close(iocp_sender, handle, session, closer);
                    }
                    sessions.remove(handle);
                }
                if result.is_ok() {
                    if let Ok(mut values) = admissions.lock() {
                        values.insert(handle, admission);
                    }
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
                        session.paused = false;
                        session.activation_deadline = None;
                        if let Some(failure) = session.input_failure {
                            notify_input_failure(handle, session, notices, counters, failure);
                        }
                        if !session.exit_notified {
                            if let Some(failure) = session.exit_failure {
                                session.exit_notified = true;
                                send_lifecycle_notice(
                                    session,
                                    notices,
                                    Notice::ExitFailed { handle, failure },
                                    counters,
                                );
                            } else if let Some(status) = session.exit_status {
                                session.exit_notified = true;
                                send_lifecycle_notice(
                                    session,
                                    notices,
                                    Notice::Exit { handle, status },
                                    counters,
                                );
                            }
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
                admission,
            } => {
                let length = bytes.len();
                let accepted = if let Some(session) = sessions.get_mut(handle) {
                    let accepted = session.enqueue_write(bytes).is_ok();
                    if !accepted {
                        admission.release(length, 1);
                        let failure = session.input_failure.unwrap_or_else(input_closed);
                        notify_input_failure(handle, session, notices, counters, failure);
                    }
                    accepted
                } else {
                    admission.release(length, 1);
                    false
                };
                if accepted {
                    if let Some(session) = sessions.get_mut(handle).filter(|value| value.active) {
                        ensure_write(iocp, handle, session, notices, counters);
                    }
                }
            }
            Command::CancelOutput { handle, reply } => {
                let found = if let Some(session) = sessions.get_mut(handle) {
                    session.discarding = true;
                    session.paused = false;
                    session.clear_output();
                    session.output_deadline = None;
                    ensure_read(iocp, handle, session, notices, counters);
                    Ok(())
                } else {
                    Err(OperationError::new(
                        Operation::Output,
                        FailureKind::WrongState,
                        None,
                    ))
                };
                let _ = reply.send(found);
            }
            Command::Size { handle, reply } => {
                let result = sessions
                    .get(handle)
                    .map(|session| session.size)
                    .ok_or_else(|| {
                        OperationError::new(Operation::Size, FailureKind::WrongState, None)
                    });
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
            Command::Resize {
                handle,
                size,
                reply,
            } => {
                let resized = sessions
                    .get_mut(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::Resize, FailureKind::WrongState, None)
                    })
                    .and_then(|session| {
                        let Ok(rows) = i16::try_from(size[0]) else {
                            return Err(OperationError::new(
                                Operation::Resize,
                                FailureKind::InvalidArgument,
                                None,
                            ));
                        };
                        let Ok(columns) = i16::try_from(size[1]) else {
                            return Err(OperationError::new(
                                Operation::Resize,
                                FailureKind::InvalidArgument,
                                None,
                            ));
                        };
                        if rows <= 0 || columns <= 0 {
                            return Err(OperationError::new(
                                Operation::Resize,
                                FailureKind::InvalidArgument,
                                None,
                            ));
                        }
                        let Some(pseudoconsole) = session.pseudoconsole.as_ref() else {
                            return Err(OperationError::new(
                                Operation::Resize,
                                FailureKind::WrongState,
                                None,
                            ));
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
                            Ok(())
                        } else {
                            Err(OperationError::new(
                                Operation::Resize,
                                FailureKind::NativeFailure,
                                Some(result),
                            ))
                        }
                    });
                let _ = reply.send(resized);
            }
            Command::Signal {
                handle,
                signal,
                reply,
            } => {
                let result = sessions
                    .get(handle)
                    .ok_or_else(|| {
                        OperationError::new(Operation::Terminate, FailureKind::WrongState, None)
                    })
                    .and_then(|session| {
                        if signal <= 0 {
                            return Err(OperationError::new(
                                Operation::Terminate,
                                FailureKind::InvalidArgument,
                                None,
                            ));
                        }
                        if session.exit_status.is_some() {
                            return Ok(false);
                        }
                        if unsafe { TerminateJobObject(session.job.raw(), 1) } != 0 {
                            Ok(true)
                        } else {
                            Err(OperationError::from_io(
                                Operation::Terminate,
                                &io::Error::last_os_error(),
                            ))
                        }
                    });
                let _ = reply.send(result);
            }
            Command::CloseStart {
                handle,
                completion,
                reply,
            } => {
                let accepted = sessions.get_mut(handle).is_some_and(|session| {
                    session.close_waiters.push(completion);
                    if !session.close_started {
                        session.close_started = true;
                        session.paused = false;
                        session.forget_output();
                        fail_input(handle, session, notices, counters, input_closed());
                        unsafe {
                            TerminateJobObject(session.job.raw(), 1);
                        }
                        cancel_write(session);
                    }
                    start_pseudoconsole_close(iocp_sender, handle, session, closer);
                    ensure_read(iocp, handle, session, notices, counters);
                    true
                });
                let _ = reply.send(accepted);
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

#[allow(clippy::too_many_arguments)]
fn process_controls(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    controls: &ControlQueue,
    scratch: &mut VecDeque<Control>,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
) {
    controls.swap_into(scratch);
    for control in scratch.drain(..) {
        match control {
            Control::Credit { handle, bytes } => {
                if let Some(session) = sessions.get_mut(handle) {
                    if session.credit(bytes) {
                        ensure_read(iocp, handle, session, notices, counters);
                        refresh_output(handle, session, notices, counters);
                    }
                }
            }
            Control::Abandon { handle } => {
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
        }
    }
}

fn associate_session(iocp: &Arc<OwnedHandle>, handle: u64, session: &Session) -> io::Result<()> {
    for pipe in [&session.input_pipe, &session.output_pipe] {
        let associated =
            unsafe { CreateIoCompletionPort(pipe.raw(), iocp.raw(), handle as usize, 0) };
        if associated.is_null() {
            return Err(io::Error::last_os_error());
        }
    }
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
            fail_input(handle, session, notices, counters, input_closed());
            cancel_write(session);
        } else {
            let error = io::Error::last_os_error();
            let failure = OperationError::from_io(Operation::Exit, &error);
            // Preserve process ownership and trailing output independently of
            // the failed status query. The internal value only marks the
            // already-signaled process terminal; it is never published.
            session.exit_status = Some(0);
            session.exit_failure = Some(failure);
            fail_input(handle, session, notices, counters, input_closed());
            cancel_write(session);
        }
    }
    if session.active && !session.exit_notified {
        if let Some(failure) = session.exit_failure {
            session.exit_notified = true;
            send_lifecycle_notice(
                session,
                notices,
                Notice::ExitFailed { handle, failure },
                counters,
            );
        } else if let Some(status) = session.exit_status {
            session.exit_notified = true;
            send_lifecycle_notice(session, notices, Notice::Exit { handle, status }, counters);
        }
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
        let mut terminal_eof = false;
        if let Some(error) = error {
            if matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_OPERATION_ABORTED as i32
            ) {
                session.output_eof = true;
                terminal_eof = true;
            } else {
                session.output_eof = true;
                session
                    .output_failure
                    .get_or_insert_with(|| OperationError::from_io(Operation::Output, &error));
            }
        } else if transferred == 0 {
            session.output_eof = true;
            terminal_eof = true;
        } else {
            let amount = transferred as usize;
            counters.read_bytes += amount as u64;
            if !session.close_started && !session.discarding {
                if session.output_bytes == 0 {
                    session.output_deadline = Some(Instant::now() + OUTPUT_DELAY);
                }
                let mut buffer = operation.into_read_buffer();
                buffer.truncate(amount);
                session.enqueue_output(Bytes::from(buffer));
            }
        }
        if terminal_eof {
            fail_input(handle, session, notices, counters, input_closed());
            cancel_write(session);
        }
        refresh_output(handle, session, notices, counters);
        ensure_read(iocp, handle, session, notices, counters);
    } else if write_matches {
        let mut operation = session
            .write
            .take()
            .expect("write completion owns operation");
        if let Some(error) = error {
            fail_input(
                handle,
                session,
                notices,
                counters,
                OperationError::from_io(Operation::Write, &error),
            );
        } else if transferred == 0 {
            fail_input(handle, session, notices, counters, input_closed());
        } else {
            let amount = transferred as usize;
            counters.write_bytes += amount as u64;
            operation.as_mut().get_mut().offset += amount;
            if !session.input_failed {
                session.input_bytes = session.input_bytes.saturating_sub(amount);
                let complete = operation.remaining_len() == 0;
                session.admission.release(amount, usize::from(complete));
                if complete {
                    session.input_entries -= 1;
                }
            }
            if !session.input_failed && operation.remaining_len() != 0 {
                operation.reset_overlapped();
                submit_write_operation(handle, session, operation, notices, counters);
            }
        }
        ensure_write(iocp, handle, session, notices, counters);
    } else {
        retain_cleanup_failure(session, infrastructure_failure(Operation::Runtime));
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
        || (session.paused && !session.discarding)
        || session.output_eof
        || (!session.discarding && session.output_total() >= session.output_capacity)
    {
        return;
    }
    let capacity = if session.discarding {
        BYTE_QUANTUM
    } else {
        (session.output_capacity - session.output_total()).min(BYTE_QUANTUM)
    };
    let mut operation = IoOperation::read(capacity);
    counters.read_syscalls += 1;
    let submitted = unsafe {
        ReadFile(
            session.output_pipe.raw(),
            operation.read_buffer_mut().as_mut_ptr(),
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
                fail_input(handle, session, notices, counters, input_closed());
                cancel_write(session);
                refresh_output(handle, session, notices, counters);
            } else {
                session.output_eof = true;
                let error = io::Error::from_raw_os_error(error as i32);
                session
                    .output_failure
                    .get_or_insert_with(|| OperationError::from_io(Operation::Output, &error));
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
    let operation = IoOperation::write(input.bytes);
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
    if submitted == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_IO_PENDING {
            let error = io::Error::from_raw_os_error(error as i32);
            fail_input(
                handle,
                session,
                notices,
                counters,
                OperationError::from_io(Operation::Write, &error),
            );
            return;
        }
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

fn cancel_read(session: &Session) {
    let Some(operation) = session.read.as_ref() else {
        return;
    };
    if unsafe { CancelIoEx(session.output_pipe.raw(), operation.overlapped_ptr()) } == 0 {
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
        close_pseudoconsole_fallback(pseudoconsole, None);
        retain_cleanup_failure(session, infrastructure_failure(Operation::Close));
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
            // The qualified Windows baseline guarantees nonblocking HPCON
            // closure. If the defensive worker cannot be created, close on the
            // reactor and complete ownership explicitly instead of leaking or
            // waiting for a worker that does not exist.
            task.pseudoconsole.close();
            session.close_permit = Some(task.permit);
            session.pseudoconsole_close_started = true;
            session.pseudoconsole_close_done = true;
        }
    }
}

fn collect_completed_closes(closer: &CloserPool, sessions: &mut GenerationRegistry<Session>) {
    for (handle, permit) in closer.drain_completed() {
        if let Some(session) = sessions.get_mut(handle) {
            session.close_permit = Some(permit);
            session.pseudoconsole_close_done = true;
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

fn fail_input(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
    failure: OperationError,
) {
    if session.fail_input(failure) {
        notify_input_failure(handle, session, notices, counters, failure);
    }
}

fn notify_input_failure(
    handle: u64,
    session: &mut Session,
    notices: &NoticeEmitter,
    counters: &mut RuntimeCounters,
    failure: OperationError,
) {
    session.input_failure.get_or_insert(failure);
    if session.active {
        if let Some(failure) = session.input_failure_notice() {
            send_lifecycle_notice(
                session,
                notices,
                Notice::InputFailed { handle, failure },
                counters,
            );
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
    if ready && !session.discarding && session.has_output() && session.output_lease_bytes == 0 {
        if let Some(reservation) = notices.try_reserve() {
            let bytes = session.pull_output(OUTPUT_BATCH);
            let amount = bytes.len();
            session.mark_output_leased(amount);
            if notices.emit(reservation, Notice::Output { handle, bytes }, counters) {
                session.output_notification_blocked = false;
            } else {
                let reclaimed = session.credit(amount);
                debug_assert!(reclaimed);
                session.abandoned = true;
                session.active = false;
            }
        } else {
            session.output_notification_blocked = true;
        }
    }
    match output_terminal_notice(
        session.output_eof,
        session.output_failure.is_some(),
        session.output_bytes,
        session.output_outstanding,
        session.output_done_notified,
        session.output_failed_notified,
    ) {
        Some(OutputTerminalNotice::Failed) => {
            session.output_failed_notified = true;
            let failure = session
                .output_failure
                .expect("failed output must retain its cause");
            send_lifecycle_notice(
                session,
                notices,
                Notice::OutputFailed { handle, failure },
                counters,
            );
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
        fail_input(handle, session, notices, counters, input_closed());
        session.forget_output();
        unsafe {
            TerminateJobObject(session.job.raw(), 1);
        }
        cancel_write(session);
    }
    start_pseudoconsole_close(iocp_sender, handle, session, closer);
    ensure_read(iocp, handle, session, notices, counters);
}

fn reap_closed(
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        let complete = sessions.get(handle).is_some_and(|session| {
            !session.abandoned && session.close_started && session.terminal()
        });
        if !complete {
            continue;
        }
        let result = sessions
            .get(handle)
            .map_or_else(CloseResult::default, |session| CloseResult {
                input_failure: session.input_failure,
                output_failure: session.output_failure,
                cleanup_failure: session.cleanup_failure,
            });
        if let Some(session) = sessions.get_mut(handle) {
            if !session.close_notified {
                session.close_notified = true;
                for waiter in session.close_waiters.drain(..) {
                    let _ = waiter.send(result);
                }
                send_lifecycle_notice(
                    session,
                    notices,
                    Notice::Closed { handle, result },
                    counters,
                );
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

#[allow(clippy::too_many_arguments)]
fn reap_abandoned(
    iocp: HANDLE,
    iocp_sender: &IocpSender,
    closer: &CloserPool,
    notices: &NoticeEmitter,
    sessions: &mut GenerationRegistry<Session>,
    counters: &mut RuntimeCounters,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            let activation_expired = !session.active
                && !session.abandoned
                && session
                    .activation_deadline
                    .is_some_and(|deadline| deadline <= Instant::now());
            if activation_expired || (session.abandoned && !session.close_started) {
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
        if removable && sessions.remove(handle).is_some() {
            notices.retire_session(handle);
            if let Ok(mut values) = admissions.lock() {
                values.remove(&handle);
            }
        }
    }
}

fn output_timeout(sessions: &GenerationRegistry<Session>) -> u32 {
    let close_polling = sessions
        .iter()
        .map(|(_, session)| session)
        .any(|session| session.pseudoconsole_close_started && !session.pseudoconsole_close_done);
    let output_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter_map(|session| {
            output_notification_deadline(
                session.output_notification_blocked,
                session.output_lease_bytes != 0,
                !session.has_output(),
                session.output_deadline,
            )
        })
        .min();
    let activation_deadline = sessions
        .iter()
        .map(|(_, session)| session)
        .filter(|session| !session.active && !session.abandoned)
        .filter_map(|session| session.activation_deadline)
        .min();
    let deadline = match (output_deadline, activation_deadline) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
    let Some(deadline) = deadline else {
        // The completion packet is an optimization, not the ownership
        // contract. Poll the completed-close registry while a close is active
        // so a failed IOCP post cannot strand session cleanup.
        return if close_polling { 100 } else { INFINITE };
    };
    let now = Instant::now();
    if deadline <= now {
        return 0;
    }
    let timeout = deadline
        .duration_since(now)
        .as_millis()
        .max(1)
        .min(u128::from(u32::MAX - 1)) as u32;
    if close_polling {
        timeout.min(100)
    } else {
        timeout
    }
}

fn output_notification_deadline(
    blocked: bool,
    notified: bool,
    output_empty: bool,
    deadline: Option<Instant>,
) -> Option<Instant> {
    (!blocked && !notified && !output_empty)
        .then_some(deadline)
        .flatten()
}

fn send_lifecycle_notice(
    session: &mut Session,
    notices: &NoticeEmitter,
    notice: Notice,
    counters: &mut RuntimeCounters,
) {
    let Some(reservation) = session.notice_reservations.pop() else {
        retain_cleanup_failure(session, infrastructure_failure(Operation::Close));
        mark_notice_failure(session);
        return;
    };
    if !notices.emit(reservation, notice, counters) {
        retain_cleanup_failure(session, infrastructure_failure(Operation::Close));
        mark_notice_failure(session);
    }
}

fn mark_notice_failure(session: &mut Session) {
    session.active = false;
    if !session.close_started {
        session.abandoned = true;
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
    let failure = session
        .cleanup_failure
        .unwrap_or_else(|| infrastructure_failure(Operation::Runtime));
    send_lifecycle_notice(
        session,
        notices,
        Notice::BrokerLost { handle, failure },
        counters,
    );
}

fn shutdown_all(
    iocp: &IocpSender,
    closer: &CloserPool,
    sessions: &mut GenerationRegistry<Session>,
    admissions: &Arc<Mutex<HashMap<u64, Arc<InputAdmission>>>>,
) {
    for handle in sessions.handles() {
        if let Some(session) = sessions.get_mut(handle) {
            unsafe {
                TerminateJobObject(session.job.raw(), 1);
            }
            cancel_read(session);
            cancel_write(session);
            force_pseudoconsole_close(iocp, handle, session, closer);
        }
    }
    if let Ok(mut values) = admissions.lock() {
        values.clear();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "__private_adapter")]
    use super::IntegratedRuntime;
    use super::{
        admit_owned_write, admit_write_with, output_notification_deadline, output_terminal_notice,
        try_command_submission, CloseAdmission, NoticeBudget, OutputTerminalNotice,
        WRITE_INFRASTRUCTURE_FAILURE,
    };
    use crate::engine::session::{AdmissionResult, InputAdmission};
    use crate::error::WriteErrorKind;
    use bytes::Bytes;
    use std::io;
    use std::sync::Mutex;
    use std::sync::{mpsc, Arc};
    #[cfg(feature = "__private_adapter")]
    use std::time::Duration;

    #[cfg(feature = "__private_adapter")]
    #[test]
    fn shutdown_closes_the_global_notification_receiver() {
        let runtime = IntegratedRuntime::try_new().expect("create Windows runtime");
        let notices = runtime
            .take_notifications()
            .expect("take global notification receiver");
        let (finished, completion) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            finished
                .send(notices.recv().is_none())
                .expect("report notification closure");
        });

        assert!(runtime.shutdown());
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "runtime shutdown must close the notification channel"
        );
        waiter.join().expect("notification waiter");
    }

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
    fn owned_write_rejection_returns_the_exact_allocation() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, _receiver) = mpsc::sync_channel(0);
        let bytes = Bytes::from(vec![1]);
        let original_pointer = bytes.as_ptr();

        let rejected = admit_owned_write(&sender, || Ok(()), 7, bytes, admission)
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

        admit_owned_write(&sender, || Ok(()), 7, bytes, admission).expect("write must be accepted");

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

        admit_owned_write(&sender, || Ok(()), 7, Bytes::new(), Arc::clone(&admission))
            .expect("empty writes are successful no-ops");

        assert!(receiver.try_recv().is_err());
        let state = admission.state.lock().expect("admission state");
        assert_eq!(state.bytes, 0);
        assert_eq!(state.entries, 0);
    }

    #[test]
    fn contended_copy_admission_returns_backpressure_without_copying() {
        let admission = Arc::new(InputAdmission::new(4));
        let held_state = admission.state.lock().expect("admission state");
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut constructed = false;

        let result = admit_write_with(
            &sender,
            || Ok(()),
            7,
            1,
            Arc::clone(&admission),
            || {
                constructed = true;
                Bytes::from_static(b"x")
            },
        );

        assert_eq!(result, AdmissionResult::Backpressure);
        assert!(!constructed);
        drop(held_state);
    }

    #[test]
    fn contended_command_submission_returns_copy_backpressure() {
        let submission = Mutex::new(());
        let held_submission = submission.lock().expect("command submission");

        let result = try_command_submission(&submission);

        assert!(matches!(result, Err(0)));
        drop(held_submission);
    }

    #[test]
    fn owned_write_wake_failure_returns_the_exact_allocation_without_enqueueing() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, receiver) = mpsc::sync_channel(1);
        let bytes = Bytes::from(vec![1]);
        let original_pointer = bytes.as_ptr();

        let rejected = admit_owned_write(
            &sender,
            || Err(io::Error::other("wake failed")),
            7,
            bytes,
            admission,
        )
        .expect_err("failed wake must reject the write");

        assert_eq!(rejected.kind(), WriteErrorKind::Infrastructure);
        let bytes = rejected.into_bytes();
        assert_eq!(bytes.as_ptr(), original_pointer);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn copy_write_wake_failure_does_not_copy_or_enqueue() {
        let admission = Arc::new(InputAdmission::new(4));
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut constructed = false;

        let result = admit_write_with(
            &sender,
            || Err(io::Error::other("wake failed")),
            7,
            1,
            admission,
            || {
                constructed = true;
                Bytes::from_static(b"x")
            },
        );

        assert_eq!(result, AdmissionResult::Infrastructure);
        assert!(!constructed);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn notice_blocked_output_has_no_due_deadline() {
        assert_eq!(
            output_notification_deadline(true, false, false, Some(std::time::Instant::now())),
            None
        );
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
