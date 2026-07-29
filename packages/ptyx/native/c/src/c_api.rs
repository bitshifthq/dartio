use bytes::Bytes;
use ptyx::__private_adapter::{BrokerSpawn, Failure, FailureKind, IntegratedRuntime, Notice};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::mem::size_of;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStringExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

const ABI_VERSION: u32 = 1;
pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_STALE_HANDLE: u32 = 2;
pub const STATUS_WRONG_STATE: u32 = 3;
const STATUS_BACKPRESSURE: u32 = 4;
const STATUS_UNSUPPORTED: u32 = 5;
const STATUS_CLOSED: u32 = 6;
const STATUS_END_OF_STREAM: u32 = 7;
const STATUS_BUSY: u32 = 8;
const STATUS_OS_ERROR: u32 = 9;
pub const STATUS_INTERNAL: u32 = 10;
const STATUS_BUFFER_TOO_SMALL: u32 = 11;

const ERROR_DOMAIN_NONE: u32 = 0;
pub const ERROR_DOMAIN_ARGUMENT: u32 = 1;
pub const ERROR_DOMAIN_STATE: u32 = 2;
const ERROR_DOMAIN_INPUT: u32 = 3;
const ERROR_DOMAIN_OUTPUT: u32 = 4;
pub const ERROR_DOMAIN_RUNTIME: u32 = 6;
const ERROR_DOMAIN_OS: u32 = 7;

const ERROR_NONE: u32 = 0;
pub const ERROR_INVALID_ARGUMENT: u32 = 1;
pub const ERROR_STALE_HANDLE: u32 = 2;
pub const ERROR_WRONG_STATE: u32 = 3;
const ERROR_QUEUE_FULL: u32 = 4;
const ERROR_UNSUPPORTED: u32 = 5;
const ERROR_CLOSED: u32 = 6;
const ERROR_NATIVE_FAILURE: u32 = 7;
pub const ERROR_INFRASTRUCTURE_LOST: u32 = 8;

const OPERATION_NONE: u32 = 0;
pub const OPERATION_RUNTIME_CREATE: u32 = 1;
pub const OPERATION_RUNTIME_SHUTDOWN: u32 = 2;
const OPERATION_SPAWN: u32 = 3;
const OPERATION_WRITE: u32 = 4;
pub const OPERATION_OUTPUT: u32 = 5;
const OPERATION_RESIZE: u32 = 6;
const OPERATION_TERMINATE: u32 = 7;
const OPERATION_METADATA: u32 = 8;
pub const OPERATION_CLOSE: u32 = 9;

const EVENT_SPAWN_READY: u32 = 1;
const EVENT_SPAWN_FAILED: u32 = 2;
const EVENT_OUTPUT: u32 = 3;
const EVENT_INPUT_FAILED: u32 = 4;
const EVENT_OUTPUT_FAILED: u32 = 5;
const EVENT_INFRASTRUCTURE_FAILED: u32 = 6;
const EVENT_OUTPUT_DONE: u32 = 7;
const EVENT_EXIT: u32 = 8;
const EVENT_CLOSE_COMPLETE: u32 = 9;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const EVENT_MODE_CHANGED: u32 = 10;

const EVENT_CLOSE_INPUT_FAILED: u32 = 1;
const EVENT_CLOSE_OUTPUT_FAILED: u32 = 2;
const EVENT_CLOSE_CLEANUP_FAILED: u32 = 4;
const MODE_CANONICAL: u32 = 1;
const MODE_ECHO: u32 = 2;
const MODE_SIGNALS: u32 = 4;

fn mode_bits(modes: [bool; 3]) -> u32 {
    (u32::from(modes[0]) * MODE_CANONICAL)
        | (u32::from(modes[1]) * MODE_ECHO)
        | (u32::from(modes[2]) * MODE_SIGNALS)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const CAPABILITY_SIGNALS: u32 = 1;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const CAPABILITY_PROCESS_GROUPS: u32 = 2;
const CAPABILITY_TERMINAL_MODES: u32 = 4;
#[cfg(windows)]
const CAPABILITY_CONPTY: u32 = 8;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const CAPABILITY_TERMINAL_NAME: u32 = 16;
const SNAPSHOT_HAS_MODE: u32 = 1;
const SNAPSHOT_HAS_TTY_NAME: u32 = 2;

const MAX_VIEW_LENGTH: usize = isize::MAX as usize;
const MAX_ARGUMENTS: usize = 256;
const MAX_ENVIRONMENT: usize = 4096;
const MAX_SPAWN_PAYLOAD: usize = 64 * 1024;
const MAX_SESSION_CAPACITY: usize = 64 * 1024 * 1024;
const SPAWN_INHERIT_ENVIRONMENT: u32 = 1;

#[cfg(feature = "test-controls")]
static TEST_SPAWN_DELAY_MS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "test-controls")]
static TEST_SPAWN_DELAY_ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "test-controls")]
static TEST_WRITE_INFRASTRUCTURE_FAILURE: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct BytesView {
    data: *const u8,
    length: u64,
}

#[repr(C)]
pub struct Error {
    pub struct_size: u32,
    pub domain: u32,
    pub kind: u32,
    pub operation: u32,
    pub native_code: i32,
    pub flags: u32,
    pub context: [u64; 2],
    pub reserved: [u64; 3],
}

impl Error {
    pub fn value(domain: u32, kind: u32, operation: u32, native_code: i32) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            domain,
            kind,
            operation,
            native_code,
            flags: 0,
            context: [0; 2],
            reserved: [0; 3],
        }
    }

    pub fn none() -> Self {
        Self::value(ERROR_DOMAIN_NONE, ERROR_NONE, OPERATION_NONE, 0)
    }
}

#[repr(C)]
pub struct RuntimeOptions {
    struct_size: u32,
    flags: u32,
    broker_path: BytesView,
    reserved: [u64; 4],
}

#[repr(C)]
pub struct SpawnOptions {
    struct_size: u32,
    flags: u32,
    executable: BytesView,
    arguments: *const BytesView,
    argument_count: u64,
    environment: *const BytesView,
    environment_count: u64,
    working_directory: BytesView,
    size: Size,
    input_capacity: u64,
    output_capacity: u64,
    graceful_close_timeout_us: u64,
    reserved: [u64; 4],
}

#[repr(C)]
pub struct SessionSnapshot {
    struct_size: u32,
    flags: u32,
    pid: i64,
    size: Size,
    modes: u32,
    reserved0: u32,
    tty_name: *mut u8,
    tty_name_capacity: u64,
    tty_name_required: u64,
    reserved: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Size {
    rows: u32,
    columns: u32,
    pixel_width: u32,
    pixel_height: u32,
}

#[repr(C)]
pub struct Event {
    pub struct_size: u32,
    pub kind: u32,
    pub flags: u32,
    pub reserved0: u32,
    pub session: u64,
    pub token: u64,
    pub data: *const u8,
    pub data_length: u64,
    pub value: i64,
    pub error: Error,
    pub reserved: [u64; 2],
}

impl Event {
    pub fn empty(struct_size: u32) -> Self {
        Self {
            struct_size,
            kind: 0,
            flags: 0,
            reserved0: 0,
            session: 0,
            token: 0,
            data: ptr::null(),
            data_length: 0,
            value: 0,
            error: Error::none(),
            reserved: [0; 2],
        }
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub struct Registry<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> Registry<T> {
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
        (u64::from(slot.generation) << 32) | (index as u64 + 1)
    }

    pub fn get(&self, handle: u64) -> Option<&T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| slot.value.as_ref())
    }

    #[cfg(feature = "test-controls")]
    pub fn sole(&self) -> Option<&T> {
        let mut values = self.slots.iter().filter_map(|slot| slot.value.as_ref());
        let value = values.next()?;
        values.next().is_none().then_some(value)
    }

    #[cfg(feature = "test-controls")]
    pub fn live_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    pub fn remove(&mut self, handle: u64) -> Option<T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        if slot.generation != u32::MAX {
            slot.generation += 1;
            self.free.push(index);
        }
        Some(value)
    }
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_handle(handle: u64) -> Option<(usize, u32)> {
    let index = (handle as u32).checked_sub(1)? as usize;
    let generation = (handle >> 32) as u32;
    (generation != 0).then_some((index, generation))
}

type ReceiveEvent = dyn Fn() -> Option<(u64, Notice)> + Send + Sync;

struct RuntimeEntry {
    engine: IntegratedRuntime,
    receive_event: Box<ReceiveEvent>,
    event_consumer: Mutex<()>,
    sessions: Mutex<HashMap<u64, u64>>,
    session_count: AtomicUsize,
    event_count: AtomicUsize,
    shut_down: AtomicBool,
}

struct SessionEntry {
    runtime: Arc<RuntimeEntry>,
    state: Mutex<SessionState>,
    close_started: Mutex<bool>,
    output_cancelled: Mutex<bool>,
}

#[derive(Clone, Copy)]
enum SessionState {
    Spawning,
    Activating,
    Active(u64),
    Closed,
    Failed,
    Released,
}

struct EventLease {
    runtime: Arc<RuntimeEntry>,
    engine_handle: u64,
    bytes: Bytes,
}

struct AdapterState {
    runtimes: Registry<Arc<RuntimeEntry>>,
    events: Registry<EventLease>,
}

impl AdapterState {
    fn new() -> Self {
        Self {
            runtimes: Registry::new(),
            events: Registry::new(),
        }
    }
}

fn adapter() -> &'static Mutex<AdapterState> {
    static ADAPTER: OnceLock<Mutex<AdapterState>> = OnceLock::new();
    ADAPTER.get_or_init(|| Mutex::new(AdapterState::new()))
}

fn runtime_entry(handle: u64) -> Option<Arc<RuntimeEntry>> {
    adapter().lock().ok()?.runtimes.get(handle).map(Arc::clone)
}

fn sessions() -> &'static RwLock<Registry<Arc<SessionEntry>>> {
    static SESSIONS: OnceLock<RwLock<Registry<Arc<SessionEntry>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| RwLock::new(Registry::new()))
}

fn session_entry(handle: u64) -> Option<Arc<SessionEntry>> {
    let state = sessions().read().ok()?;
    state.get(handle).map(Arc::clone)
}

fn active_session(handle: u64) -> Result<(Arc<SessionEntry>, u64), u32> {
    let entry = session_entry(handle).ok_or(STATUS_STALE_HANDLE)?;
    let state = *entry.state.lock().map_err(|_| STATUS_INTERNAL)?;
    match state {
        SessionState::Active(engine_handle) => Ok((entry, engine_handle)),
        SessionState::Spawning
        | SessionState::Activating
        | SessionState::Closed
        | SessionState::Failed
        | SessionState::Released => Err(STATUS_WRONG_STATE),
    }
}

fn active_session_for_write(handle: u64) -> Result<(Arc<SessionEntry>, u64), u32> {
    let entry = {
        let state = match sessions().try_read() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(STATUS_BACKPRESSURE),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(STATUS_INTERNAL),
        };
        state
            .get(handle)
            .map(Arc::clone)
            .ok_or(STATUS_STALE_HANDLE)?
    };
    let state = match entry.state.try_lock() {
        Ok(state) => *state,
        Err(std::sync::TryLockError::WouldBlock) => return Err(STATUS_BACKPRESSURE),
        Err(std::sync::TryLockError::Poisoned(_)) => return Err(STATUS_INTERNAL),
    };
    match state {
        SessionState::Active(engine_handle) => Ok((entry, engine_handle)),
        SessionState::Spawning
        | SessionState::Activating
        | SessionState::Closed
        | SessionState::Failed
        | SessionState::Released => Err(STATUS_WRONG_STATE),
    }
}

unsafe fn require_active(
    handle: u64,
    operation: u32,
    error: *mut Error,
) -> Result<(Arc<SessionEntry>, u64), u32> {
    match active_session(handle) {
        Ok(value) => Ok(value),
        Err(STATUS_STALE_HANDLE) => {
            set_error(error, stale_error(operation));
            Err(STATUS_STALE_HANDLE)
        }
        Err(STATUS_WRONG_STATE) => {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, operation, 0),
            );
            Err(STATUS_WRONG_STATE)
        }
        Err(_) => {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    operation,
                    0,
                ),
            );
            Err(STATUS_INTERNAL)
        }
    }
}

/// Checks whether an optional C error destination has a compatible layout.
///
/// # Safety
///
/// A non-null `error` must point to readable initialized `Error` storage.
pub unsafe fn error_is_valid(error: *mut Error) -> bool {
    error.is_null() || (*error).struct_size as usize >= size_of::<Error>()
}

/// Clears a validated optional C error destination.
///
/// # Safety
///
/// A non-null `error` must point to writable compatible `Error` storage.
pub unsafe fn clear_error(error: *mut Error) {
    if !error.is_null() && (*error).struct_size as usize >= size_of::<Error>() {
        *error = Error::none();
    }
}

/// Stores a value in a validated optional C error destination.
///
/// # Safety
///
/// A non-null `error` must point to writable compatible `Error` storage.
pub unsafe fn set_error(error: *mut Error, value: Error) {
    if !error.is_null() && (*error).struct_size as usize >= size_of::<Error>() {
        *error = value;
    }
}

fn stale_error(operation: u32) -> Error {
    Error::value(ERROR_DOMAIN_STATE, ERROR_STALE_HANDLE, operation, 0)
}

fn invalid_error(operation: u32) -> Error {
    Error::value(ERROR_DOMAIN_ARGUMENT, ERROR_INVALID_ARGUMENT, operation, 0)
}

fn io_error(operation: u32, error: &io::Error) -> (u32, Error) {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
    ) {
        return (STATUS_INVALID_ARGUMENT, invalid_error(operation));
    }
    if error.kind() == io::ErrorKind::Unsupported {
        return (
            STATUS_UNSUPPORTED,
            Error::value(
                ERROR_DOMAIN_OS,
                ERROR_UNSUPPORTED,
                operation,
                native_code(error),
            ),
        );
    }
    (
        STATUS_OS_ERROR,
        Error::value(
            ERROR_DOMAIN_OS,
            ERROR_NATIVE_FAILURE,
            operation,
            native_code(error),
        ),
    )
}

fn native_code(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(0)
}

unsafe fn boundary(error: *mut Error, operation: impl FnOnce() -> u32) -> u32 {
    if !error_is_valid(error) {
        return STATUS_INVALID_ARGUMENT;
    }
    clear_error(error);
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(status) => status,
        Err(_) => {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_NONE,
                    0,
                ),
            );
            STATUS_INTERNAL
        }
    }
}

unsafe fn borrowed_bytes(view: &BytesView) -> Result<&[u8], ()> {
    let length = usize::try_from(view.length).map_err(|_| ())?;
    if length > MAX_VIEW_LENGTH || (length != 0 && view.data.is_null()) {
        return Err(());
    }
    if length == 0 {
        return Ok(&[]);
    }
    Ok(std::slice::from_raw_parts(view.data, length))
}

fn capabilities() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        CAPABILITY_SIGNALS
            | CAPABILITY_PROCESS_GROUPS
            | CAPABILITY_TERMINAL_MODES
            | CAPABILITY_TERMINAL_NAME
    }
    #[cfg(windows)]
    {
        CAPABILITY_CONPTY
    }
}

#[no_mangle]
pub extern "C" fn ptyx_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_error_format(
    error: *const Error,
    target: *mut u8,
    capacity: u64,
    required: *mut u64,
) -> u32 {
    boundary(ptr::null_mut(), || {
        if error.is_null()
            || ((*error).struct_size as usize) < size_of::<Error>()
            || (capacity != 0 && target.is_null())
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let message = error_message(&*error).as_bytes();
        if !required.is_null() {
            *required = message.len() as u64;
        }
        let Ok(capacity) = usize::try_from(capacity) else {
            return STATUS_INVALID_ARGUMENT;
        };
        if capacity < message.len() {
            return STATUS_BUFFER_TOO_SMALL;
        }
        if !message.is_empty() {
            ptr::copy_nonoverlapping(message.as_ptr(), target, message.len());
        }
        if capacity > message.len() {
            *target.add(message.len()) = 0;
        }
        STATUS_OK
    })
}

fn error_message(error: &Error) -> &'static str {
    match (error.domain, error.kind) {
        (ERROR_DOMAIN_NONE, ERROR_NONE) => "no error",
        (ERROR_DOMAIN_ARGUMENT, ERROR_INVALID_ARGUMENT) => "invalid argument",
        (ERROR_DOMAIN_STATE, ERROR_STALE_HANDLE) => "stale handle",
        (ERROR_DOMAIN_STATE, ERROR_WRONG_STATE) => "operation is invalid in the current state",
        (ERROR_DOMAIN_INPUT, ERROR_QUEUE_FULL) => "input capacity is exhausted",
        (_, ERROR_UNSUPPORTED) => "operation is unsupported",
        (_, ERROR_CLOSED) => "session is closed",
        (_, ERROR_INFRASTRUCTURE_LOST) => "native runtime infrastructure was lost",
        _ => "native operation failed",
    }
}

#[no_mangle]
/// Creates a C ABI runtime.
///
/// # Safety
///
/// Every non-null pointer must satisfy the layout, initialization, and
/// ownership contract documented by `ptyx.h`.
pub unsafe extern "C" fn ptyx_runtime_create(
    options: *const RuntimeOptions,
    runtime: *mut u64,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if runtime.is_null() {
            set_error(error, invalid_error(OPERATION_RUNTIME_CREATE));
            return STATUS_INVALID_ARGUMENT;
        }
        *runtime = 0;
        let broker_path = match runtime_broker_path(options) {
            Ok(value) => value,
            Err(()) => {
                set_error(error, invalid_error(OPERATION_RUNTIME_CREATE));
                return STATUS_INVALID_ARGUMENT;
            }
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let created = IntegratedRuntime::try_new(&broker_path);
        #[cfg(windows)]
        let _ = broker_path;
        #[cfg(windows)]
        let created = IntegratedRuntime::try_new();
        let engine = match created {
            Ok(value) => value,
            Err(failure) => {
                let (status, value) = io_error(OPERATION_RUNTIME_CREATE, &failure);
                set_error(error, value);
                return status;
            }
        };
        let Some(receiver) = engine.take_notifications() else {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_CREATE,
                    0,
                ),
            );
            return STATUS_INTERNAL;
        };
        let entry = Arc::new(RuntimeEntry {
            engine,
            receive_event: Box::new(move || receiver.recv()),
            event_consumer: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            session_count: AtomicUsize::new(0),
            event_count: AtomicUsize::new(0),
            shut_down: AtomicBool::new(false),
        });
        let Ok(mut state) = adapter().lock() else {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_CREATE,
                    0,
                ),
            );
            return STATUS_INTERNAL;
        };
        *runtime = state.runtimes.insert(entry);
        STATUS_OK
    })
}

unsafe fn runtime_broker_path(options: *const RuntimeOptions) -> Result<PathBuf, ()> {
    if !options.is_null() {
        if ((*options).struct_size as usize) < size_of::<RuntimeOptions>()
            || (*options).flags != 0
            || (*options).reserved != [0; 4]
        {
            return Err(());
        }
        let bytes = borrowed_bytes(&(*options).broker_path)?;
        if !bytes.is_empty() {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                return Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())));
            }
            #[cfg(windows)]
            {
                return Ok(PathBuf::new());
            }
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        ptyx::__private_adapter::broker_path().map_err(|_| ())
    }
    #[cfg(windows)]
    {
        Ok(PathBuf::new())
    }
}

#[no_mangle]
/// Reads the capabilities of a C ABI runtime.
///
/// # Safety
///
/// `output` must be writable and `error`, when non-null, must be compatible
/// initialized storage as documented by `ptyx.h`.
pub unsafe extern "C" fn ptyx_runtime_capabilities(
    runtime: u64,
    output: *mut u32,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if output.is_null() {
            set_error(error, invalid_error(OPERATION_METADATA));
            return STATUS_INVALID_ARGUMENT;
        }
        if runtime_entry(runtime).is_none() {
            set_error(error, stale_error(OPERATION_METADATA));
            return STATUS_STALE_HANDLE;
        }
        *output = capabilities();
        STATUS_OK
    })
}

#[no_mangle]
/// Transfers the next C ABI event to the caller.
///
/// # Safety
///
/// `event` must be writable initialized event storage and `error`, when
/// non-null, must satisfy the contract documented by `ptyx.h`.
pub unsafe extern "C" fn ptyx_runtime_next_event(
    runtime: u64,
    event: *mut Event,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if event.is_null() || ((*event).struct_size as usize) < size_of::<Event>() {
            set_error(error, invalid_error(OPERATION_OUTPUT));
            return STATUS_INVALID_ARGUMENT;
        }
        if (*event).token != 0 {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_OUTPUT, 0),
            );
            return STATUS_WRONG_STATE;
        }
        let Some(runtime) = runtime_entry(runtime) else {
            set_error(error, stale_error(OPERATION_OUTPUT));
            return STATUS_STALE_HANDLE;
        };
        let Ok(_consumer) = runtime.event_consumer.try_lock() else {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_OUTPUT, 0),
            );
            return STATUS_BUSY;
        };
        loop {
            let Some((route, notice)) = (runtime.receive_event)() else {
                return STATUS_END_OF_STREAM;
            };
            match notice {
                Notice::SpawnReady {
                    request,
                    handle: engine_handle,
                } => {
                    #[cfg(feature = "test-controls")]
                    {
                        let delay = TEST_SPAWN_DELAY_MS.swap(0, Ordering::AcqRel);
                        if delay != 0 {
                            TEST_SPAWN_DELAY_ACTIVE.store(true, Ordering::Release);
                            std::thread::sleep(Duration::from_millis(delay as u64));
                            TEST_SPAWN_DELAY_ACTIVE.store(false, Ordering::Release);
                        }
                    }
                    let Some(entry) = session_entry(request) else {
                        runtime.engine.try_abandon(engine_handle);
                        continue;
                    };
                    if !Arc::ptr_eq(&entry.runtime, &runtime) {
                        runtime.engine.try_abandon(engine_handle);
                        continue;
                    }
                    let should_activate = entry
                        .state
                        .lock()
                        .map(|mut state| {
                            if matches!(*state, SessionState::Spawning) {
                                *state = SessionState::Activating;
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if !should_activate {
                        runtime.engine.try_abandon(engine_handle);
                        continue;
                    }
                    if !runtime.engine.activate(engine_handle) {
                        runtime.engine.try_abandon(engine_handle);
                        if let Ok(mut state) = entry.state.lock() {
                            if matches!(*state, SessionState::Activating) {
                                *state = SessionState::Failed;
                            }
                        }
                        let struct_size = (*event).struct_size;
                        *event = Event::empty(struct_size);
                        (*event).session = request;
                        (*event).kind = EVENT_SPAWN_FAILED;
                        (*event).error = Error::value(
                            ERROR_DOMAIN_RUNTIME,
                            ERROR_INFRASTRUCTURE_LOST,
                            OPERATION_SPAWN,
                            0,
                        );
                        return STATUS_OK;
                    }
                    let Ok(mut sessions) = runtime.sessions.lock() else {
                        runtime.engine.try_abandon(engine_handle);
                        continue;
                    };
                    let activated = entry
                        .state
                        .lock()
                        .map(|mut state| {
                            if matches!(*state, SessionState::Activating) {
                                *state = SessionState::Active(engine_handle);
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if activated {
                        sessions.insert(engine_handle, request);
                    } else {
                        runtime.engine.try_abandon(engine_handle);
                        continue;
                    }
                    let struct_size = (*event).struct_size;
                    *event = Event::empty(struct_size);
                    (*event).session = request;
                    (*event).kind = EVENT_SPAWN_READY;
                    return STATUS_OK;
                }
                Notice::SpawnFailed { request, failure } => {
                    let Some(entry) = session_entry(request) else {
                        continue;
                    };
                    if !Arc::ptr_eq(&entry.runtime, &runtime) {
                        continue;
                    }
                    let deliver = entry
                        .state
                        .lock()
                        .map(|mut state| {
                            if matches!(*state, SessionState::Spawning) {
                                *state = SessionState::Failed;
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if !deliver {
                        continue;
                    }
                    let struct_size = (*event).struct_size;
                    *event = Event::empty(struct_size);
                    (*event).session = request;
                    (*event).kind = EVENT_SPAWN_FAILED;
                    (*event).error = failure_error(OPERATION_SPAWN, failure);
                    return STATUS_OK;
                }
                notice => {
                    let engine_handle = route;
                    let public_handle = runtime
                        .sessions
                        .lock()
                        .ok()
                        .and_then(|sessions| sessions.get(&engine_handle).copied());
                    let Some(public_handle) = public_handle else {
                        continue;
                    };
                    let struct_size = (*event).struct_size;
                    *event = Event::empty(struct_size);
                    (*event).session = public_handle;
                    return populate_event(&runtime, engine_handle, notice, &mut *event, error);
                }
            }
        }
    })
}

fn failure_error(operation: u32, failure: Failure) -> Error {
    let (domain, kind) = match failure.kind {
        FailureKind::InvalidInput => (ERROR_DOMAIN_ARGUMENT, ERROR_INVALID_ARGUMENT),
        FailureKind::Backpressure => (ERROR_DOMAIN_STATE, ERROR_QUEUE_FULL),
        FailureKind::Unsupported => (ERROR_DOMAIN_OS, ERROR_UNSUPPORTED),
        FailureKind::NotFound | FailureKind::PermissionDenied | FailureKind::Other => {
            (ERROR_DOMAIN_OS, ERROR_NATIVE_FAILURE)
        }
        FailureKind::Infrastructure => (ERROR_DOMAIN_RUNTIME, ERROR_INFRASTRUCTURE_LOST),
    };
    Error::value(domain, kind, operation, failure.native_code.unwrap_or(0))
}

unsafe fn populate_event(
    runtime: &Arc<RuntimeEntry>,
    engine_handle: u64,
    notice: Notice,
    event: &mut Event,
    error: *mut Error,
) -> u32 {
    match notice {
        Notice::Output { bytes, .. } => {
            event.kind = EVENT_OUTPUT;
            event.data = bytes.as_ptr();
            event.data_length = bytes.len() as u64;
            let lease = EventLease {
                runtime: Arc::clone(runtime),
                engine_handle,
                bytes,
            };
            let Ok(mut state) = adapter().lock() else {
                set_error(
                    error,
                    Error::value(
                        ERROR_DOMAIN_RUNTIME,
                        ERROR_INFRASTRUCTURE_LOST,
                        OPERATION_OUTPUT,
                        0,
                    ),
                );
                return STATUS_INTERNAL;
            };
            event.token = state.events.insert(lease);
            runtime.event_count.fetch_add(1, Ordering::AcqRel);
        }
        Notice::InputFailed(_) => {
            event.kind = EVENT_INPUT_FAILED;
            event.error =
                Error::value(ERROR_DOMAIN_INPUT, ERROR_NATIVE_FAILURE, OPERATION_WRITE, 0);
        }
        Notice::OutputFailed(_) => {
            event.kind = EVENT_OUTPUT_FAILED;
            event.error = Error::value(
                ERROR_DOMAIN_OUTPUT,
                ERROR_NATIVE_FAILURE,
                OPERATION_OUTPUT,
                0,
            );
        }
        Notice::BrokerLost(_) => {
            event.kind = EVENT_INFRASTRUCTURE_FAILED;
            event.error = Error::value(
                ERROR_DOMAIN_RUNTIME,
                ERROR_INFRASTRUCTURE_LOST,
                OPERATION_OUTPUT,
                0,
            );
        }
        Notice::OutputDone(_) => event.kind = EVENT_OUTPUT_DONE,
        Notice::Exit { status, .. } => {
            event.kind = EVENT_EXIT;
            event.value = status;
        }
        Notice::Closed { result, .. } => {
            event.kind = EVENT_CLOSE_COMPLETE;
            event.flags = (u32::from(result.input_failed) * EVENT_CLOSE_INPUT_FAILED)
                | (u32::from(result.output_failed) * EVENT_CLOSE_OUTPUT_FAILED)
                | (u32::from(result.cleanup_failed) * EVENT_CLOSE_CLEANUP_FAILED);
            event.error = if result.cleanup_failed {
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_CLOSE,
                    0,
                )
            } else if result.input_failed {
                Error::value(ERROR_DOMAIN_INPUT, ERROR_NATIVE_FAILURE, OPERATION_CLOSE, 0)
            } else if result.output_failed {
                Error::value(
                    ERROR_DOMAIN_OUTPUT,
                    ERROR_NATIVE_FAILURE,
                    OPERATION_CLOSE,
                    0,
                )
            } else {
                Error::none()
            };
            if let Some(entry) = session_entry(event.session) {
                if let Ok(mut state) = entry.state.lock() {
                    *state = SessionState::Closed;
                }
            }
            if let Ok(mut sessions) = runtime.sessions.lock() {
                sessions.remove(&engine_handle);
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Notice::ModeChanged { modes, .. } => {
            event.kind = EVENT_MODE_CHANGED;
            event.value = i64::from(mode_bits(modes));
        }
        Notice::SpawnReady { .. } | Notice::SpawnFailed { .. } => {
            return STATUS_INTERNAL;
        }
    }
    STATUS_OK
}

#[no_mangle]
/// Starts C ABI runtime shutdown.
///
/// # Safety
///
/// `error`, when non-null, must point to compatible initialized storage.
pub unsafe extern "C" fn ptyx_runtime_shutdown(runtime: u64, error: *mut Error) -> u32 {
    boundary(error, || {
        let Some(runtime) = runtime_entry(runtime) else {
            set_error(error, stale_error(OPERATION_RUNTIME_SHUTDOWN));
            return STATUS_STALE_HANDLE;
        };
        if runtime.shut_down.load(Ordering::Acquire) {
            return STATUS_OK;
        }
        if !runtime.engine.shutdown() {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_SHUTDOWN,
                    0,
                ),
            );
            return STATUS_INTERNAL;
        }
        runtime.shut_down.store(true, Ordering::Release);
        STATUS_OK
    })
}

#[no_mangle]
/// Releases a shut-down C ABI runtime handle.
///
/// # Safety
///
/// `runtime` must be writable and `error`, when non-null, must point to
/// compatible initialized storage.
pub unsafe extern "C" fn ptyx_runtime_release(runtime: *mut u64, error: *mut Error) -> u32 {
    boundary(error, || {
        if runtime.is_null() {
            set_error(error, invalid_error(OPERATION_RUNTIME_SHUTDOWN));
            return STATUS_INVALID_ARGUMENT;
        }
        if *runtime == 0 {
            return STATUS_OK;
        }
        let Some(entry) = runtime_entry(*runtime) else {
            set_error(error, stale_error(OPERATION_RUNTIME_SHUTDOWN));
            return STATUS_STALE_HANDLE;
        };
        if !entry.shut_down.load(Ordering::Acquire)
            || entry.session_count.load(Ordering::Acquire) != 0
            || entry.event_count.load(Ordering::Acquire) != 0
        {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_STATE,
                    ERROR_WRONG_STATE,
                    OPERATION_RUNTIME_SHUTDOWN,
                    0,
                ),
            );
            return STATUS_BUSY;
        }
        let Ok(mut state) = adapter().lock() else {
            return STATUS_INTERNAL;
        };
        if state.runtimes.remove(*runtime).is_none() {
            set_error(error, stale_error(OPERATION_RUNTIME_SHUTDOWN));
            return STATUS_STALE_HANDLE;
        }
        *runtime = 0;
        STATUS_OK
    })
}

#[no_mangle]
/// Starts a C ABI session spawn.
///
/// # Safety
///
/// The options graph must remain readable for the call, `session` must be
/// writable, and `error` must satisfy the contract documented by `ptyx.h`.
pub unsafe extern "C" fn ptyx_session_spawn_start(
    runtime: u64,
    options: *const SpawnOptions,
    session: *mut u64,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if session.is_null() || options.is_null() {
            set_error(error, invalid_error(OPERATION_SPAWN));
            return STATUS_INVALID_ARGUMENT;
        }
        *session = 0;
        let Some(runtime) = runtime_entry(runtime) else {
            set_error(error, stale_error(OPERATION_SPAWN));
            return STATUS_STALE_HANDLE;
        };
        if runtime.shut_down.load(Ordering::Acquire) {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_CLOSED, OPERATION_SPAWN, 0),
            );
            return STATUS_CLOSED;
        }
        let (config, input_capacity, output_capacity) = match spawn_config(&*options) {
            Ok(value) => value,
            Err(()) => {
                set_error(error, invalid_error(OPERATION_SPAWN));
                return STATUS_INVALID_ARGUMENT;
            }
        };
        let entry = Arc::new(SessionEntry {
            runtime: Arc::clone(&runtime),
            state: Mutex::new(SessionState::Spawning),
            close_started: Mutex::new(false),
            output_cancelled: Mutex::new(false),
        });
        let public_handle = {
            let Ok(mut state) = sessions().write() else {
                return STATUS_INTERNAL;
            };
            state.insert(entry)
        };
        runtime.session_count.fetch_add(1, Ordering::AcqRel);
        match runtime.engine.spawn_start_notified(
            public_handle,
            config,
            input_capacity,
            output_capacity,
        ) {
            Ok(()) => {
                *session = public_handle;
                STATUS_OK
            }
            Err(failure) => {
                if let Ok(mut state) = sessions().write() {
                    state.remove(public_handle);
                }
                runtime.session_count.fetch_sub(1, Ordering::AcqRel);
                let (status, value) = io_error(OPERATION_SPAWN, &failure);
                set_error(error, value);
                status
            }
        }
    })
}

unsafe fn spawn_config(options: &SpawnOptions) -> Result<(BrokerSpawn, usize, usize), ()> {
    if (options.struct_size as usize) < size_of::<SpawnOptions>()
        || options.flags & !SPAWN_INHERIT_ENVIRONMENT != 0
        || options.reserved != [0; 4]
        || options.argument_count > MAX_ARGUMENTS as u64
        || options.environment_count > MAX_ENVIRONMENT as u64
        || options.graceful_close_timeout_us > 60_000_000
        || options.size.rows == 0
        || options.size.columns == 0
        || options.size.rows > i16::MAX as u32
        || options.size.columns > i16::MAX as u32
        || options.size.pixel_width > u32::from(u16::MAX)
        || options.size.pixel_height > u32::from(u16::MAX)
    {
        return Err(());
    }
    let input_capacity = usize::try_from(options.input_capacity).map_err(|_| ())?;
    let output_capacity = usize::try_from(options.output_capacity).map_err(|_| ())?;
    if !(1..=MAX_SESSION_CAPACITY).contains(&input_capacity)
        || !(1..=MAX_SESSION_CAPACITY).contains(&output_capacity)
    {
        return Err(());
    }
    let mut remaining = MAX_SPAWN_PAYLOAD;
    let executable = spawn_bytes(&options.executable, &mut remaining)?;
    if executable.is_empty() {
        return Err(());
    }
    let arguments = spawn_byte_strings(options.arguments, options.argument_count, &mut remaining)?;
    let inherit_environment = options.flags & SPAWN_INHERIT_ENVIRONMENT != 0;
    if inherit_environment && options.environment_count != 0 {
        return Err(());
    }
    let environment = if inherit_environment {
        None
    } else {
        let values = spawn_byte_strings(
            options.environment,
            options.environment_count,
            &mut remaining,
        )?;
        if values
            .iter()
            .any(|bytes| bytes.first() == Some(&b'=') || !bytes.contains(&b'='))
        {
            return Err(());
        }
        Some(values)
    };
    let working_directory = borrowed_bytes(&options.working_directory)?;
    let cwd = if working_directory.is_empty() {
        None
    } else {
        if working_directory.len() > remaining || working_directory.contains(&0) {
            return Err(());
        }
        Some(working_directory.to_vec())
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let config = BrokerSpawn {
        executable: OsString::from_vec(executable),
        arguments: arguments.into_iter().map(OsString::from_vec).collect(),
        environment: environment.map(|entries| {
            entries
                .into_iter()
                .map(|entry| {
                    let separator = entry
                        .iter()
                        .position(|byte| *byte == b'=')
                        .expect("environment separator was validated");
                    (
                        OsString::from_vec(entry[..separator].to_vec()),
                        OsString::from_vec(entry[separator + 1..].to_vec()),
                    )
                })
                .collect()
        }),
        cwd: cwd.map(|value| PathBuf::from(OsString::from_vec(value))),
        rows: options.size.rows,
        columns: options.size.columns,
        pixel_width: options.size.pixel_width,
        pixel_height: options.size.pixel_height,
        graceful_close_timeout: Duration::from_micros(options.graceful_close_timeout_us),
    };
    #[cfg(windows)]
    let config = BrokerSpawn {
        executable: windows_os_string(executable)?,
        arguments: arguments
            .into_iter()
            .map(windows_os_string)
            .collect::<Result<Vec<_>, _>>()?,
        environment: environment
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| {
                        let separator = entry
                            .iter()
                            .position(|byte| *byte == b'=')
                            .expect("environment separator was validated");
                        Ok((
                            windows_os_string(entry[..separator].to_vec())?,
                            windows_os_string(entry[separator + 1..].to_vec())?,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
        cwd: cwd.map(windows_os_string).transpose()?.map(PathBuf::from),
        rows: options.size.rows,
        columns: options.size.columns,
        pixel_width: options.size.pixel_width,
        pixel_height: options.size.pixel_height,
        graceful_close_timeout: Duration::from_micros(options.graceful_close_timeout_us),
    };
    Ok((config, input_capacity, output_capacity))
}

#[cfg(windows)]
fn windows_os_string(bytes: Vec<u8>) -> Result<OsString, ()> {
    String::from_utf8(bytes).map(OsString::from).map_err(|_| ())
}

unsafe fn spawn_byte_strings(
    values: *const BytesView,
    count: u64,
    remaining: &mut usize,
) -> Result<Vec<Vec<u8>>, ()> {
    let count = usize::try_from(count).map_err(|_| ())?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if values.is_null() || count > isize::MAX as usize / size_of::<BytesView>() {
        return Err(());
    }
    std::slice::from_raw_parts(values, count)
        .iter()
        .map(|value| spawn_bytes(value, remaining))
        .collect()
}

unsafe fn spawn_bytes(view: &BytesView, remaining: &mut usize) -> Result<Vec<u8>, ()> {
    let bytes = borrowed_bytes(view)?;
    if bytes.len() > *remaining || bytes.contains(&0) {
        return Err(());
    }
    *remaining -= bytes.len();
    Ok(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_write(
    session: u64,
    bytes: *const u8,
    length: u64,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        let Ok(length) = usize::try_from(length) else {
            set_error(error, invalid_error(OPERATION_WRITE));
            return STATUS_INVALID_ARGUMENT;
        };
        if length > MAX_VIEW_LENGTH || (length != 0 && bytes.is_null()) {
            set_error(error, invalid_error(OPERATION_WRITE));
            return STATUS_INVALID_ARGUMENT;
        }
        #[cfg(feature = "test-controls")]
        if TEST_WRITE_INFRASTRUCTURE_FAILURE.swap(false, Ordering::AcqRel) {
            set_error(
                error,
                Error::value(
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_WRITE,
                    0,
                ),
            );
            return STATUS_INTERNAL;
        }
        let (entry, engine_handle) = match active_session_for_write(session) {
            Ok(value) => value,
            Err(STATUS_BACKPRESSURE) => {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_INPUT, ERROR_QUEUE_FULL, OPERATION_WRITE, 0),
                );
                return STATUS_BACKPRESSURE;
            }
            Err(STATUS_STALE_HANDLE) => {
                set_error(error, stale_error(OPERATION_WRITE));
                return STATUS_STALE_HANDLE;
            }
            Err(STATUS_WRONG_STATE) => {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_WRITE, 0),
                );
                return STATUS_WRONG_STATE;
            }
            Err(_) => {
                set_error(
                    error,
                    Error::value(
                        ERROR_DOMAIN_RUNTIME,
                        ERROR_INFRASTRUCTURE_LOST,
                        OPERATION_WRITE,
                        0,
                    ),
                );
                return STATUS_INTERNAL;
            }
        };
        if length == 0 {
            return STATUS_OK;
        }
        let borrowed = std::slice::from_raw_parts(bytes, length);
        match entry.runtime.engine.write_copy(engine_handle, borrowed) {
            1 => STATUS_OK,
            0 => {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_INPUT, ERROR_QUEUE_FULL, OPERATION_WRITE, 0),
                );
                STATUS_BACKPRESSURE
            }
            -1 => {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_INPUT, ERROR_CLOSED, OPERATION_WRITE, 0),
                );
                STATUS_CLOSED
            }
            _ => {
                set_error(
                    error,
                    Error::value(
                        ERROR_DOMAIN_RUNTIME,
                        ERROR_INFRASTRUCTURE_LOST,
                        OPERATION_WRITE,
                        0,
                    ),
                );
                STATUS_INTERNAL
            }
        }
    })
}

#[cfg(feature = "test-controls")]
pub fn test_delay_next_spawn(milliseconds: usize) {
    TEST_SPAWN_DELAY_MS.store(milliseconds, Ordering::Release);
}

#[cfg(feature = "test-controls")]
pub fn test_spawn_delay_active() -> bool {
    TEST_SPAWN_DELAY_ACTIVE.load(Ordering::Acquire)
}

#[cfg(feature = "test-controls")]
pub fn test_fail_next_write() {
    TEST_WRITE_INFRASTRUCTURE_FAILURE.store(true, Ordering::Release);
}

#[cfg(feature = "test-controls")]
pub fn test_kill_broker(runtime: u64) -> bool {
    let Some(runtime) = runtime_entry(runtime) else {
        return false;
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        runtime.engine.kill_broker_for_test();
        true
    }
    #[cfg(windows)]
    {
        let _ = runtime;
        false
    }
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_cancel_output(session: u64, error: *mut Error) -> u32 {
    boundary(error, || {
        let Some(entry) = session_entry(session) else {
            set_error(error, stale_error(OPERATION_OUTPUT));
            return STATUS_STALE_HANDLE;
        };
        let Ok(mut output_cancelled) = entry.output_cancelled.lock() else {
            return STATUS_INTERNAL;
        };
        if *output_cancelled {
            STATUS_OK
        } else if let SessionState::Active(engine_handle) = *entry
            .state
            .lock()
            .unwrap_or_else(|value| value.into_inner())
        {
            if entry.runtime.engine.cancel_output(engine_handle) {
                *output_cancelled = true;
                STATUS_OK
            } else {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_OUTPUT, 0),
                );
                STATUS_WRONG_STATE
            }
        } else {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_OUTPUT, 0),
            );
            STATUS_WRONG_STATE
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_resize(
    session: u64,
    size: *const Size,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if size.is_null()
            || (*size).rows == 0
            || (*size).columns == 0
            || (*size).rows > i16::MAX as u32
            || (*size).columns > i16::MAX as u32
            || (*size).pixel_width > u32::from(u16::MAX)
            || (*size).pixel_height > u32::from(u16::MAX)
        {
            set_error(error, invalid_error(OPERATION_RESIZE));
            return STATUS_INVALID_ARGUMENT;
        }
        let (entry, engine_handle) = match require_active(session, OPERATION_RESIZE, error) {
            Ok(value) => value,
            Err(status) => return status,
        };
        let size = *size;
        if entry.runtime.engine.resize(
            engine_handle,
            [size.rows, size.columns, size.pixel_width, size.pixel_height],
        ) {
            STATUS_OK
        } else {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_OS, ERROR_NATIVE_FAILURE, OPERATION_RESIZE, 0),
            );
            STATUS_OS_ERROR
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_terminate(
    session: u64,
    signal: i32,
    delivered: *mut u32,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if delivered.is_null() {
            set_error(error, invalid_error(OPERATION_TERMINATE));
            return STATUS_INVALID_ARGUMENT;
        }
        let (entry, engine_handle) = match require_active(session, OPERATION_TERMINATE, error) {
            Ok(value) => value,
            Err(status) => return status,
        };
        match entry.runtime.engine.signal(engine_handle, signal) {
            Some(value) => {
                *delivered = u32::from(value);
                STATUS_OK
            }
            None => {
                set_error(
                    error,
                    Error::value(
                        ERROR_DOMAIN_OS,
                        ERROR_NATIVE_FAILURE,
                        OPERATION_TERMINATE,
                        0,
                    ),
                );
                STATUS_OS_ERROR
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_snapshot(
    session: u64,
    snapshot: *mut SessionSnapshot,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if snapshot.is_null()
            || ((*snapshot).struct_size as usize) < size_of::<SessionSnapshot>()
            || (*snapshot).reserved0 != 0
            || (*snapshot).reserved != [0; 4]
            || ((*snapshot).tty_name_capacity != 0 && (*snapshot).tty_name.is_null())
        {
            set_error(error, invalid_error(OPERATION_METADATA));
            return STATUS_INVALID_ARGUMENT;
        }
        let (entry, engine_handle) = match require_active(session, OPERATION_METADATA, error) {
            Ok(value) => value,
            Err(status) => return status,
        };
        let Some(value) = entry.runtime.engine.snapshot(engine_handle) else {
            return metadata_failure(error);
        };
        (*snapshot).flags = 0;
        (*snapshot).pid = value.pid;
        (*snapshot).size = Size {
            rows: value.size[0],
            columns: value.size[1],
            pixel_width: value.size[2],
            pixel_height: value.size[3],
        };
        (*snapshot).modes = 0;
        if let Some(modes) = value.mode {
            (*snapshot).flags |= SNAPSHOT_HAS_MODE;
            (*snapshot).modes = mode_bits(modes);
        }
        (*snapshot).tty_name_required = 0;
        let Some(name) = value.tty_name else {
            return STATUS_OK;
        };
        (*snapshot).flags |= SNAPSHOT_HAS_TTY_NAME;
        (*snapshot).tty_name_required = name.len() as u64;
        let Ok(capacity) = usize::try_from((*snapshot).tty_name_capacity) else {
            set_error(error, invalid_error(OPERATION_METADATA));
            return STATUS_INVALID_ARGUMENT;
        };
        if capacity < name.len() {
            return STATUS_BUFFER_TOO_SMALL;
        }
        if !name.is_empty() {
            ptr::copy_nonoverlapping(name.as_ptr(), (*snapshot).tty_name, name.len());
        }
        STATUS_OK
    })
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_observe_mode(
    session: u64,
    enabled: u32,
    error: *mut Error,
) -> u32 {
    boundary(error, || {
        if enabled > 1 {
            set_error(error, invalid_error(OPERATION_METADATA));
            return STATUS_INVALID_ARGUMENT;
        }
        let (entry, engine_handle) = match require_active(session, OPERATION_METADATA, error) {
            Ok(value) => value,
            Err(status) => return status,
        };
        if capabilities() & CAPABILITY_TERMINAL_MODES == 0 {
            set_error(
                error,
                Error::value(ERROR_DOMAIN_STATE, ERROR_UNSUPPORTED, OPERATION_METADATA, 0),
            );
            return STATUS_UNSUPPORTED;
        }
        if entry
            .runtime
            .engine
            .observe_mode(engine_handle, enabled != 0)
        {
            STATUS_OK
        } else {
            metadata_failure(error)
        }
    })
}

unsafe fn metadata_failure(error: *mut Error) -> u32 {
    set_error(
        error,
        Error::value(ERROR_DOMAIN_OS, ERROR_NATIVE_FAILURE, OPERATION_METADATA, 0),
    );
    STATUS_OS_ERROR
}

#[no_mangle]
pub unsafe extern "C" fn ptyx_session_close(session: u64, error: *mut Error) -> u32 {
    boundary(error, || {
        let Some(entry) = session_entry(session) else {
            set_error(error, stale_error(OPERATION_CLOSE));
            return STATUS_STALE_HANDLE;
        };
        let mut close_started = entry
            .close_started
            .lock()
            .unwrap_or_else(|value| value.into_inner());
        let engine_handle = match *entry
            .state
            .lock()
            .unwrap_or_else(|value| value.into_inner())
        {
            SessionState::Active(value) => value,
            SessionState::Closed => return STATUS_OK,
            SessionState::Spawning
            | SessionState::Activating
            | SessionState::Failed
            | SessionState::Released => {
                set_error(
                    error,
                    Error::value(ERROR_DOMAIN_STATE, ERROR_WRONG_STATE, OPERATION_CLOSE, 0),
                );
                return STATUS_WRONG_STATE;
            }
        };
        if *close_started {
            return STATUS_OK;
        }
        match entry.runtime.engine.close_start(engine_handle) {
            Ok(completion) => {
                drop(completion);
                *close_started = true;
                STATUS_OK
            }
            Err(failure) => {
                let (status, value) = io_error(OPERATION_CLOSE, &failure);
                set_error(error, value);
                status
            }
        }
    })
}

#[no_mangle]
/// Releases a C ABI session handle.
///
/// # Safety
///
/// `session` must be writable and `error`, when non-null, must point to
/// compatible initialized storage.
pub unsafe extern "C" fn ptyx_session_release(session: *mut u64, error: *mut Error) -> u32 {
    boundary(error, || {
        if session.is_null() {
            set_error(error, invalid_error(OPERATION_CLOSE));
            return STATUS_INVALID_ARGUMENT;
        }
        if *session == 0 {
            return STATUS_OK;
        }
        let Some(entry) = session_entry(*session) else {
            set_error(error, stale_error(OPERATION_CLOSE));
            return STATUS_STALE_HANDLE;
        };
        let engine_handle = {
            let Ok(mut sessions) = entry.runtime.sessions.lock() else {
                set_error(
                    error,
                    Error::value(
                        ERROR_DOMAIN_RUNTIME,
                        ERROR_INFRASTRUCTURE_LOST,
                        OPERATION_CLOSE,
                        0,
                    ),
                );
                return STATUS_INTERNAL;
            };
            let mut session_state = entry
                .state
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            let engine_handle = match *session_state {
                SessionState::Active(value) => Some(value),
                SessionState::Spawning
                | SessionState::Activating
                | SessionState::Closed
                | SessionState::Failed
                | SessionState::Released => None,
            };
            *session_state = SessionState::Released;
            if let Some(engine_handle) = engine_handle {
                sessions.remove(&engine_handle);
            }
            engine_handle
        };
        if let Some(engine_handle) = engine_handle {
            entry.runtime.engine.try_abandon(engine_handle);
        }
        let Ok(mut state) = sessions().write() else {
            return STATUS_INTERNAL;
        };
        if state.remove(*session).is_none() {
            set_error(error, stale_error(OPERATION_CLOSE));
            return STATUS_STALE_HANDLE;
        }
        entry.runtime.session_count.fetch_sub(1, Ordering::AcqRel);
        *session = 0;
        STATUS_OK
    })
}

#[no_mangle]
/// Releases a transferred C ABI event.
///
/// # Safety
///
/// `event` must identify caller-owned compatible event storage and `error`,
/// when non-null, must point to compatible initialized storage.
pub unsafe extern "C" fn ptyx_event_release(event: *mut Event, error: *mut Error) -> u32 {
    boundary(error, || {
        if event.is_null() {
            set_error(error, invalid_error(OPERATION_OUTPUT));
            return STATUS_INVALID_ARGUMENT;
        }
        if ((*event).struct_size as usize) < size_of::<Event>() {
            set_error(error, invalid_error(OPERATION_OUTPUT));
            return STATUS_INVALID_ARGUMENT;
        }
        if (*event).token == 0 {
            let struct_size = (*event).struct_size;
            *event = Event::empty(struct_size);
            return STATUS_OK;
        }
        let Ok(mut state) = adapter().lock() else {
            return STATUS_INTERNAL;
        };
        let Some(lease) = state.events.get((*event).token) else {
            set_error(error, stale_error(OPERATION_OUTPUT));
            return STATUS_STALE_HANDLE;
        };
        let _ = lease
            .runtime
            .engine
            .credit_async(lease.engine_handle, lease.bytes.len());
        let Some(lease) = state.events.remove((*event).token) else {
            return STATUS_INTERNAL;
        };
        lease.runtime.event_count.fetch_sub(1, Ordering::AcqRel);
        let struct_size = (*event).struct_size;
        *event = Event::empty(struct_size);
        STATUS_OK
    })
}

#[cfg(test)]
mod tests {
    use super::{
        active_session_for_write, decode_handle, io_error, sessions, Error, Event, Registry,
        RuntimeOptions, SessionSnapshot, SpawnOptions, ERROR_DOMAIN_ARGUMENT,
        ERROR_INVALID_ARGUMENT, OPERATION_SPAWN, STATUS_BACKPRESSURE, STATUS_INVALID_ARGUMENT,
    };
    use std::io;
    use std::mem::size_of;

    #[test]
    fn error_layout_matches_the_public_header() {
        assert_eq!(size_of::<Error>(), 64);
        assert_eq!(size_of::<RuntimeOptions>(), 56);
        assert_eq!(size_of::<SpawnOptions>(), 144);
        assert_eq!(size_of::<SessionSnapshot>(), 96);
        assert_eq!(size_of::<Event>(), 136);
    }

    #[test]
    fn retired_handles_do_not_resolve_after_slot_reuse() {
        let mut registry = Registry::new();
        let retired = registry.insert(1);

        assert_eq!(registry.remove(retired), Some(1));
        let replacement = registry.insert(2);

        assert!(registry.get(retired).is_none());
        assert_eq!(registry.get(replacement), Some(&2));
    }

    #[test]
    fn zero_and_generationless_handles_are_invalid() {
        assert_eq!(decode_handle(0), None);
        assert_eq!(decode_handle(1), None);
    }

    #[test]
    fn contended_write_registry_returns_backpressure() {
        let _held_registry = sessions().write().expect("session registry");

        let result = active_session_for_write(u64::MAX);

        assert!(matches!(result, Err(STATUS_BACKPRESSURE)));
    }

    #[test]
    fn native_validation_errors_remain_invalid_argument_errors() {
        let (status, error) = io_error(
            OPERATION_SPAWN,
            &io::Error::new(io::ErrorKind::InvalidInput, "invalid spawn"),
        );

        assert_eq!(status, STATUS_INVALID_ARGUMENT);
        assert_eq!(error.domain, ERROR_DOMAIN_ARGUMENT);
        assert_eq!(error.kind, ERROR_INVALID_ARGUMENT);
        assert_eq!(error.operation, OPERATION_SPAWN);
    }
}
