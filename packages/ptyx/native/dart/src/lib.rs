use ptyx_c::private as c_api;
use ptyx_c::private::{
    Error, Event, Registry, SpawnOptions, ERROR_DOMAIN_ARGUMENT, ERROR_DOMAIN_RUNTIME,
    ERROR_DOMAIN_STATE, ERROR_INFRASTRUCTURE_LOST, ERROR_INVALID_ARGUMENT, ERROR_STALE_HANDLE,
    ERROR_WRONG_STATE, OPERATION_CLOSE, OPERATION_OUTPUT, OPERATION_RUNTIME_CREATE,
    OPERATION_RUNTIME_SHUTDOWN, STATUS_INTERNAL, STATUS_INVALID_ARGUMENT, STATUS_OK,
    STATUS_STALE_HANDLE, STATUS_WRONG_STATE,
};
use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const EVENT_SPAWN_FAILED: u32 = 2;
const EVENT_OUTPUT: u32 = 3;
const EVENT_INFRASTRUCTURE_FAILED: u32 = 6;
const EVENT_CLOSE_COMPLETE: u32 = 9;
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(10);
const CLEANUP_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);
static DART_INITIALIZED: AtomicBool = AtomicBool::new(false);

struct PumpState {
    stopping: bool,
    outstanding: HashSet<u64>,
    sessions: HashSet<u64>,
}

struct Pump {
    handle: AtomicU64,
    in_flight_events: AtomicUsize,
    runtime: u64,
    runtime_retained: AtomicBool,
    port: i64,
    state: Mutex<PumpState>,
    cleanup: Mutex<()>,
    cleaned: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Pump {
    fn new(runtime: u64, port: i64) -> Self {
        Self {
            handle: AtomicU64::new(0),
            in_flight_events: AtomicUsize::new(0),
            runtime,
            runtime_retained: AtomicBool::new(true),
            port,
            state: Mutex::new(PumpState {
                stopping: false,
                outstanding: HashSet::new(),
                sessions: HashSet::new(),
            }),
            cleanup: Mutex::new(()),
            cleaned: AtomicBool::new(false),
            thread: Mutex::new(None),
        }
    }
}

struct EventWait<'a>(&'a AtomicUsize);

impl EventWait<'_> {
    fn start(counter: &AtomicUsize) -> EventWait<'_> {
        counter.fetch_add(1, Ordering::AcqRel);
        EventWait(counter)
    }
}

impl Drop for EventWait<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn pumps() -> &'static Mutex<Registry<Arc<Pump>>> {
    static PUMPS: OnceLock<Mutex<Registry<Arc<Pump>>>> = OnceLock::new();
    PUMPS.get_or_init(|| Mutex::new(Registry::new()))
}

fn pump(handle: u64) -> Option<Arc<Pump>> {
    pumps().lock().ok()?.get(handle).map(Arc::clone)
}

enum Cleanup {
    Adapter(u64),
    Event { adapter: u64, token: u64 },
    Session(u64),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CleanupKey {
    Adapter(u64),
    Event { adapter: u64, token: u64 },
    Session(u64),
}

impl Cleanup {
    fn key(&self) -> CleanupKey {
        match *self {
            Self::Adapter(handle) => CleanupKey::Adapter(handle),
            Self::Event { adapter, token } => CleanupKey::Event { adapter, token },
            Self::Session(handle) => CleanupKey::Session(handle),
        }
    }
}

fn pending_cleanup() -> &'static Mutex<HashSet<CleanupKey>> {
    static PENDING: OnceLock<Mutex<HashSet<CleanupKey>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

fn cleanup_sender() -> Option<&'static Sender<Cleanup>> {
    static CLEANUP: OnceLock<Option<Sender<Cleanup>>> = OnceLock::new();
    CLEANUP
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            thread::Builder::new()
                .name("ptyx-dart-cleanup".into())
                .spawn(move || {
                    let mut pending: VecDeque<PendingCleanup> = VecDeque::new();
                    loop {
                        let now = Instant::now();
                        let due = pending
                            .iter()
                            .map(|item| item.retry_at.saturating_duration_since(now))
                            .min();
                        let wait = due.unwrap_or(CLEANUP_RETRY_MAX_DELAY);
                        match receiver.recv_timeout(wait) {
                            Ok(cleanup) => pending.push_back(PendingCleanup::new(cleanup)),
                            Err(RecvTimeoutError::Timeout) => {
                                let Some(index) = pending
                                    .iter()
                                    .position(|item| item.retry_at <= Instant::now())
                                else {
                                    continue;
                                };
                                let Some(mut item) = pending.remove(index) else {
                                    continue;
                                };
                                let status = attempt_cleanup(&item.cleanup);
                                if status != STATUS_OK && status != STATUS_STALE_HANDLE {
                                    item.retry_at = Instant::now() + item.delay;
                                    item.delay = (item.delay * 2).min(CLEANUP_RETRY_MAX_DELAY);
                                    pending.push_back(item);
                                } else if let Ok(mut keys) = pending_cleanup().lock() {
                                    keys.remove(&item.cleanup.key());
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

struct PendingCleanup {
    cleanup: Cleanup,
    retry_at: Instant,
    delay: Duration,
}

impl PendingCleanup {
    fn new(cleanup: Cleanup) -> Self {
        Self {
            cleanup,
            retry_at: Instant::now(),
            delay: CLEANUP_RETRY_DELAY,
        }
    }
}

fn attempt_cleanup(cleanup: &Cleanup) -> u32 {
    match cleanup {
        Cleanup::Adapter(handle) => {
            let mut adapter = *handle;
            unsafe { detach_adapter(&mut adapter, ptr::null_mut()) }
        }
        Cleanup::Event { adapter, token } => {
            let Some(pump) = pump(*adapter) else {
                return unsafe { release_event(*token, ptr::null_mut()) };
            };
            acknowledge_token(&pump, *token, || unsafe {
                release_event(*token, ptr::null_mut())
            })
        }
        Cleanup::Session(handle) => release_session_handle(*handle),
    }
}

fn schedule_cleanup(cleanup: Cleanup) {
    let key = cleanup.key();
    let Ok(mut pending) = pending_cleanup().lock() else {
        return;
    };
    if !pending.insert(key) {
        return;
    }
    if let Some(sender) = cleanup_sender() {
        if sender.send(cleanup).is_err() {
            pending.remove(&key);
        }
    } else {
        pending.remove(&key);
    }
}

unsafe fn begin(error: *mut Error) -> bool {
    if !c_api::error_is_valid(error) {
        return false;
    }
    c_api::clear_error(error);
    true
}

unsafe fn fail(error: *mut Error, status: u32, domain: u32, kind: u32, operation: u32) -> u32 {
    c_api::set_error(error, Error::value(domain, kind, operation, 0));
    status
}

unsafe fn guarded(error: *mut Error, operation: u32, action: impl FnOnce() -> u32) -> u32 {
    if !begin(error) {
        return STATUS_INVALID_ARGUMENT;
    }
    catch_unwind(AssertUnwindSafe(action)).unwrap_or_else(|_| {
        fail(
            error,
            STATUS_INTERNAL,
            ERROR_DOMAIN_RUNTIME,
            ERROR_INFRASTRUCTURE_LOST,
            operation,
        )
    })
}

#[no_mangle]
/// Initializes Dart API-DL for this dynamic library.
///
/// # Safety
///
/// `api_data` must be the live pointer supplied by
/// `NativeApi.initializeApiDLData`.
pub unsafe extern "C" fn ptyd_initialize(api_data: *mut c_void) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if api_data.is_null()
            || !*LIBRARY_PINNED.get_or_init(pin_native_library)
            || cleanup_sender().is_none()
        {
            return STATUS_INVALID_ARGUMENT;
        }
        std::hint::black_box(ptyx_c::linked_symbols());
        if Dart_InitializeApiDL(api_data) == 0 {
            DART_INITIALIZED.store(true, Ordering::Release);
            STATUS_OK
        } else {
            STATUS_INVALID_ARGUMENT
        }
    }))
    .unwrap_or(STATUS_INTERNAL)
}

#[no_mangle]
/// Transfers a C ABI runtime to a Dart event adapter.
///
/// # Safety
///
/// `adapter` must be writable and `error`, when non-null, must point to
/// compatible initialized C ABI error storage.
pub unsafe extern "C" fn ptyd_runtime_attach(
    runtime: u64,
    port: i64,
    adapter: *mut u64,
    error: *mut Error,
) -> u32 {
    if !begin(error) {
        return STATUS_INVALID_ARGUMENT;
    }
    catch_unwind(AssertUnwindSafe(|| {
        if adapter.is_null() {
            return fail(
                error,
                STATUS_INVALID_ARGUMENT,
                ERROR_DOMAIN_ARGUMENT,
                ERROR_INVALID_ARGUMENT,
                OPERATION_RUNTIME_CREATE,
            );
        }
        *adapter = 0;
        if !DART_INITIALIZED.load(Ordering::Acquire) {
            return fail(
                error,
                STATUS_WRONG_STATE,
                ERROR_DOMAIN_STATE,
                ERROR_WRONG_STATE,
                OPERATION_RUNTIME_CREATE,
            );
        }
        if port <= 0 {
            return fail(
                error,
                STATUS_INVALID_ARGUMENT,
                ERROR_DOMAIN_ARGUMENT,
                ERROR_INVALID_ARGUMENT,
                OPERATION_RUNTIME_CREATE,
            );
        }
        // Validate and take ownership of a live C runtime before publishing an
        // adapter handle. Registering a stale runtime would otherwise create a
        // pump that can only fail asynchronously, leaving an orphaned registry
        // entry and making attach appear successful to the caller.
        let status = unsafe { c_api::ptyx_runtime_adapter_retain(runtime, error) };
        if status != STATUS_OK {
            return status;
        }
        let value = Arc::new(Pump::new(runtime, port));
        let handle = {
            let Ok(mut registry) = pumps().lock() else {
                let _ = unsafe { c_api::ptyx_runtime_adapter_release(runtime) };
                return fail(
                    error,
                    STATUS_INTERNAL,
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_CREATE,
                );
            };
            if registry.values().any(|existing| {
                existing.runtime == runtime && !existing.cleaned.load(Ordering::Acquire)
            }) {
                let _ = unsafe { c_api::ptyx_runtime_adapter_release(runtime) };
                return fail(
                    error,
                    STATUS_WRONG_STATE,
                    ERROR_DOMAIN_STATE,
                    ERROR_WRONG_STATE,
                    OPERATION_RUNTIME_CREATE,
                );
            }
            registry.insert(Arc::clone(&value))
        };
        value.handle.store(handle, Ordering::Release);
        let worker = match thread::Builder::new()
            .name("ptyx-dart-events".into())
            .spawn({
                let value = Arc::clone(&value);
                move || pump_events(&value)
            }) {
            Ok(worker) => worker,
            Err(_) => {
                let _ = unsafe { c_api::ptyx_runtime_adapter_release(runtime) };
                if let Ok(mut registry) = pumps().lock() {
                    registry.remove(handle);
                }
                return fail(
                    error,
                    STATUS_INTERNAL,
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_CREATE,
                );
            }
        };
        *value
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(worker);
        *adapter = handle;
        STATUS_OK
    }))
    .unwrap_or_else(|_| {
        fail(
            error,
            STATUS_INTERNAL,
            ERROR_DOMAIN_RUNTIME,
            ERROR_INFRASTRUCTURE_LOST,
            OPERATION_RUNTIME_CREATE,
        )
    })
}

#[no_mangle]
/// Returns capabilities for an attached Dart runtime adapter.
///
/// # Safety
///
/// `capabilities` must be writable and `error`, when non-null, must identify
/// initialized compatible error storage.
pub unsafe extern "C" fn ptyd_runtime_capabilities(
    adapter: u64,
    capabilities: *mut u32,
    error: *mut Error,
) -> u32 {
    guarded(error, OPERATION_RUNTIME_CREATE, || {
        if capabilities.is_null() {
            return fail(
                error,
                STATUS_INVALID_ARGUMENT,
                ERROR_DOMAIN_ARGUMENT,
                ERROR_INVALID_ARGUMENT,
                OPERATION_RUNTIME_CREATE,
            );
        }
        let Some(pump) = pump(adapter) else {
            return fail(
                error,
                STATUS_STALE_HANDLE,
                ERROR_DOMAIN_RUNTIME,
                ERROR_STALE_HANDLE,
                OPERATION_RUNTIME_CREATE,
            );
        };
        c_api::ptyx_runtime_capabilities(pump.runtime, capabilities, error)
    })
}

#[no_mangle]
/// Atomically starts and tracks a session spawn for Dart delivery.
///
/// # Safety
///
/// `options` must identify a valid borrowed C ABI options graph, `session`
/// must be writable, and `error` must satisfy the private adapter header.
pub unsafe extern "C" fn ptyd_session_spawn_start(
    adapter: u64,
    options: *const SpawnOptions,
    session: *mut u64,
    error: *mut Error,
) -> u32 {
    guarded(error, OPERATION_RUNTIME_CREATE, || {
        let Some(pump) = pump(adapter) else {
            return fail(
                error,
                STATUS_STALE_HANDLE,
                ERROR_DOMAIN_STATE,
                ERROR_STALE_HANDLE,
                OPERATION_CLOSE,
            );
        };
        if options.is_null() || session.is_null() {
            return fail(
                error,
                STATUS_INVALID_ARGUMENT,
                ERROR_DOMAIN_ARGUMENT,
                ERROR_INVALID_ARGUMENT,
                OPERATION_RUNTIME_CREATE,
            );
        }
        let Ok(mut state) = pump.state.lock() else {
            return STATUS_INTERNAL;
        };
        if state.stopping {
            return fail(
                error,
                STATUS_WRONG_STATE,
                ERROR_DOMAIN_STATE,
                ERROR_WRONG_STATE,
                OPERATION_CLOSE,
            );
        }
        let status = c_api::ptyx_session_spawn_start(pump.runtime, options, session, error);
        if status == STATUS_OK {
            state.sessions.insert(*session);
        }
        status
    })
}

#[no_mangle]
/// Abandons a session tracked by a Dart adapter.
///
/// # Safety
///
/// `session` must be writable and `error`, when non-null, must point to
/// compatible initialized C ABI error storage.
pub unsafe extern "C" fn ptyd_session_release(
    adapter: u64,
    session: *mut u64,
    error: *mut Error,
) -> u32 {
    guarded(error, OPERATION_CLOSE, || {
        if session.is_null() {
            return fail(
                error,
                STATUS_INVALID_ARGUMENT,
                ERROR_DOMAIN_ARGUMENT,
                ERROR_INVALID_ARGUMENT,
                OPERATION_CLOSE,
            );
        }
        if *session == 0 {
            return STATUS_OK;
        }
        let Some(pump) = pump(adapter) else {
            return fail(
                error,
                STATUS_STALE_HANDLE,
                ERROR_DOMAIN_STATE,
                ERROR_STALE_HANDLE,
                OPERATION_CLOSE,
            );
        };
        let Ok(mut state) = pump.state.lock() else {
            return STATUS_INTERNAL;
        };
        if !state.sessions.contains(&*session) {
            return fail(
                error,
                STATUS_WRONG_STATE,
                ERROR_DOMAIN_STATE,
                ERROR_WRONG_STATE,
                OPERATION_CLOSE,
            );
        }
        let tracked = *session;
        let status = c_api::ptyx_session_release(session, error);
        if status == STATUS_OK || status == STATUS_STALE_HANDLE {
            state.sessions.remove(&tracked);
        }
        status
    })
}

#[no_mangle]
/// Acknowledges a transferred Dart output event.
///
/// # Safety
///
/// `error`, when non-null, must point to compatible initialized C ABI error
/// storage. The token must still belong to this adapter.
pub unsafe extern "C" fn ptyd_event_ack(adapter: u64, token: u64, error: *mut Error) -> u32 {
    guarded(error, OPERATION_OUTPUT, || {
        let Some(pump) = pump(adapter) else {
            return fail(
                error,
                STATUS_STALE_HANDLE,
                ERROR_DOMAIN_STATE,
                ERROR_STALE_HANDLE,
                OPERATION_OUTPUT,
            );
        };
        let status = acknowledge_token(&pump, token, || release_event(token, error));
        if status == STATUS_STALE_HANDLE {
            fail(
                error,
                STATUS_STALE_HANDLE,
                ERROR_DOMAIN_STATE,
                ERROR_STALE_HANDLE,
                OPERATION_OUTPUT,
            )
        } else {
            if status != STATUS_OK && status != STATUS_STALE_HANDLE {
                schedule_cleanup(Cleanup::Event { adapter, token });
            }
            status
        }
    })
}

#[no_mangle]
/// Stops and releases a Dart runtime adapter.
///
/// # Safety
///
/// `adapter` must be writable and `error`, when non-null, must point to
/// compatible initialized C ABI error storage.
pub unsafe extern "C" fn ptyd_runtime_detach(adapter: *mut u64, error: *mut Error) -> u32 {
    guarded(error, OPERATION_RUNTIME_SHUTDOWN, || unsafe {
        detach_adapter_with_retry(adapter, error)
    })
}

#[no_mangle]
/// Aborts a Dart runtime adapter after a protocol or infrastructure failure.
///
/// The operation is idempotent and uses the same native ownership path as
/// explicit detach. Failed releases remain registered and are retried by the
/// native cleanup worker without requiring a Dart isolate.
///
/// # Safety
///
/// `adapter` must be writable and `error`, when non-null, must point to
/// compatible initialized C ABI error storage.
pub unsafe extern "C" fn ptyd_runtime_abort(adapter: *mut u64, error: *mut Error) -> u32 {
    guarded(error, OPERATION_RUNTIME_SHUTDOWN, || unsafe {
        detach_adapter_with_retry(adapter, error)
    })
}

unsafe fn detach_adapter_with_retry(adapter: *mut u64, error: *mut Error) -> u32 {
    let status = detach_adapter(adapter, error);
    if status != STATUS_OK && !adapter.is_null() && *adapter != 0 {
        schedule_cleanup(Cleanup::Adapter(*adapter));
    }
    status
}

unsafe fn detach_adapter(adapter: *mut u64, error: *mut Error) -> u32 {
    if adapter.is_null() {
        return fail(
            error,
            STATUS_INVALID_ARGUMENT,
            ERROR_DOMAIN_ARGUMENT,
            ERROR_INVALID_ARGUMENT,
            OPERATION_RUNTIME_SHUTDOWN,
        );
    }
    if *adapter == 0 {
        return STATUS_OK;
    }
    let handle = *adapter;
    let Some(value) = pump(handle) else {
        *adapter = 0;
        return STATUS_OK;
    };
    let stop_status = stop_pump(&value);
    if let Some(worker) = value
        .thread
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        let _ = worker.join();
    }
    let cleanup_status = cleanup_pump(&value);
    let status = if cleanup_status == STATUS_OK {
        stop_status
    } else {
        cleanup_status
    };
    if status != STATUS_OK {
        return status;
    }
    if let Ok(mut registry) = pumps().lock() {
        registry.remove(handle);
    }
    *adapter = 0;
    STATUS_OK
}

#[no_mangle]
pub extern "C" fn ptyd_runtime_finalize(token: *mut c_void) {
    let handle = token.addr() as u64;
    if handle == 0 {
        return;
    }
    schedule_cleanup(Cleanup::Adapter(handle));
}

#[no_mangle]
pub extern "C" fn ptyd_session_finalize(token: *mut c_void) {
    let handle = token.addr() as u64;
    if handle == 0 {
        return;
    }
    schedule_cleanup(Cleanup::Session(handle));
}

fn pump_events(pump: &Arc<Pump>) {
    loop {
        // Register the native wait while holding the same lock used by
        // cleanup_pump to stop publication. This closes the shutdown race in
        // which cleanup could snapshot ownership before a late waiter starts.
        let Some(_wait) = begin_event_wait(pump) else {
            break;
        };
        let mut event = Event::empty(size_of::<Event>() as u32);
        let mut error = Error::none();
        let status =
            unsafe { c_api::ptyx_runtime_next_event(pump.runtime, &mut event, &mut error) };
        if status != STATUS_OK {
            let stopping = pump
                .state
                .lock()
                .map(|state| state.stopping)
                .unwrap_or(true);
            if !stopping {
                unsafe {
                    post_terminal_failure(pump.port);
                }
            }
            break;
        }

        if pump
            .state
            .lock()
            .map(|state| state.stopping)
            .unwrap_or(true)
        {
            retain_unpublished_event(pump, &event);
            break;
        }

        if event.kind == EVENT_OUTPUT {
            let mut state = pump
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if event.token == 0
                || event.data.is_null()
                || event.data_length == 0
                || event.data_length > isize::MAX as u64
            {
                drop(state);
                handle_invalid_event(pump, &event);
                break;
            }
            state.outstanding.insert(event.token);
            let posted = unsafe { post_event(pump.port, &event) };
            if posted {
                continue;
            }
            state.outstanding.remove(&event.token);
            drop(state);
            let status = unsafe { release_event(event.token, ptr::null_mut()) };
            if status != STATUS_OK && status != STATUS_STALE_HANDLE {
                if let Ok(mut state) = pump.state.lock() {
                    state.outstanding.insert(event.token);
                }
            }
            unsafe { post_terminal_failure(pump.port) };
            break;
        }

        let kind = event.kind;
        let session = event.session;
        let posted = unsafe { post_event(pump.port, &event) };
        unsafe {
            c_api::ptyx_event_release(&mut event, ptr::null_mut());
        }
        if !posted {
            unsafe {
                post_terminal_failure(pump.port);
            }
            break;
        }
        if kind == EVENT_SPAWN_FAILED || kind == EVENT_CLOSE_COMPLETE {
            let status = release_completed_session(pump, session);
            if status != STATUS_OK && status != STATUS_STALE_HANDLE {
                unsafe {
                    post_terminal_failure(pump.port);
                }
                break;
            }
        }
    }
    let _ = stop_pump(pump);
    let handle = pump.handle.load(Ordering::Acquire);
    if cleanup_pump(pump) == STATUS_OK {
        if let Ok(mut registry) = pumps().lock() {
            registry.remove(handle);
        }
    } else if handle != 0 {
        schedule_cleanup(Cleanup::Adapter(handle));
    }
}

fn begin_event_wait(pump: &Pump) -> Option<EventWait<'_>> {
    let state = pump.state.lock().ok()?;
    if state.stopping {
        return None;
    }
    Some(EventWait::start(&pump.in_flight_events))
}

unsafe fn post_terminal_failure(port: i64) {
    let mut terminal = Event::empty(size_of::<Event>() as u32);
    terminal.kind = EVENT_INFRASTRUCTURE_FAILED;
    terminal.error = Error::value(
        ERROR_DOMAIN_RUNTIME,
        ERROR_INFRASTRUCTURE_LOST,
        OPERATION_OUTPUT,
        0,
    );
    post_event(port, &terminal);
}

unsafe fn post_event(port: i64, event: &Event) -> bool {
    ptyx_dart_post_event(
        port,
        event.kind,
        event.session,
        event.token,
        event.flags,
        event.value,
        event.error.domain,
        event.error.kind,
        event.error.native_code,
        event.data,
        event.data_length as isize,
    )
}

fn handle_invalid_event(pump: &Pump, event: &Event) {
    let status = unsafe { release_event(event.token, ptr::null_mut()) };
    if event.token != 0 && status != STATUS_OK && status != STATUS_STALE_HANDLE {
        if let Ok(mut state) = pump.state.lock() {
            state.outstanding.insert(event.token);
        }
    }
    unsafe {
        post_terminal_failure(pump.port);
    }
}

fn retain_unpublished_event(pump: &Pump, event: &Event) {
    let status = unsafe { release_event(event.token, ptr::null_mut()) };
    if event.token != 0 && status != STATUS_OK && status != STATUS_STALE_HANDLE {
        if let Ok(mut state) = pump.state.lock() {
            state.outstanding.insert(event.token);
        }
    }
}

fn release_completed_session(pump: &Pump, session: u64) -> u32 {
    let Ok(mut state) = pump.state.lock() else {
        return STATUS_INTERNAL;
    };
    if !state.sessions.contains(&session) {
        return STATUS_STALE_HANDLE;
    }
    let mut released = session;
    let status = unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) };
    if status == STATUS_OK || status == STATUS_STALE_HANDLE {
        state.sessions.remove(&session);
    }
    status
}

unsafe fn release_event(token: u64, error: *mut Error) -> u32 {
    let mut event = Event::empty(size_of::<Event>() as u32);
    event.token = token;
    c_api::ptyx_event_release(&mut event, error)
}

fn acknowledge_token(pump: &Pump, token: u64, release: impl FnOnce() -> u32) -> u32 {
    {
        let Ok(state) = pump.state.lock() else {
            return STATUS_INTERNAL;
        };
        if !state.outstanding.contains(&token) {
            return STATUS_STALE_HANDLE;
        }
    }
    let status = release();
    if status == STATUS_OK || status == STATUS_STALE_HANDLE {
        if let Ok(mut state) = pump.state.lock() {
            state.outstanding.remove(&token);
        }
    }
    status
}

fn stop_pump(pump: &Pump) -> u32 {
    if let Ok(mut state) = pump.state.lock() {
        state.stopping = true;
    }
    unsafe { c_api::ptyx_runtime_shutdown(pump.runtime, ptr::null_mut()) }
}

fn cleanup_pump(pump: &Pump) -> u32 {
    let Ok(_cleanup) = pump.cleanup.lock() else {
        return STATUS_INTERNAL;
    };
    if pump.cleaned.load(Ordering::Acquire) {
        return STATUS_OK;
    }
    // Stop publication before taking the ownership snapshot. The in-flight
    // wait counter ensures a completed native event cannot add a lease after
    // this snapshot, even when shutdown races the event thread.
    if let Ok(mut state) = pump.state.lock() {
        state.stopping = true;
    }
    let shutdown_status = stop_pump(pump);
    while pump.in_flight_events.load(Ordering::Acquire) != 0 {
        thread::yield_now();
    }
    let (events, sessions) = {
        let state = pump
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let events = state.outstanding.iter().copied().collect::<Vec<_>>();
        let sessions = state.sessions.iter().copied().collect::<Vec<_>>();
        (events, sessions)
    };
    // Return output credit and abandon sessions before the final runtime
    // release. If an explicit detach already stopped the runtime, the C ABI
    // treats those leases as terminal and relinquishes them without queuing
    // credit into a reactor that can no longer consume it.
    let mut cleanup_status = STATUS_OK;
    for token in events {
        unsafe {
            let status = release_event(token, ptr::null_mut());
            if status != STATUS_OK && status != STATUS_STALE_HANDLE {
                cleanup_status = status;
            } else if let Ok(mut state) = pump.state.lock() {
                state.outstanding.remove(&token);
            }
        }
    }
    for session in sessions {
        let mut released = session;
        let status = unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) };
        if status != STATUS_OK && status != STATUS_STALE_HANDLE {
            cleanup_status = status;
        }
        if status == STATUS_OK || status == STATUS_STALE_HANDLE {
            if let Ok(mut state) = pump.state.lock() {
                state.sessions.remove(&session);
            }
        }
    }
    if shutdown_status != STATUS_OK {
        cleanup_status = shutdown_status;
    }
    if cleanup_status != STATUS_OK {
        return cleanup_status;
    }
    let mut runtime = pump.runtime;
    if pump.runtime_retained.load(Ordering::Acquire) {
        let status = unsafe { c_api::ptyx_runtime_adapter_release(pump.runtime) };
        if status != STATUS_OK && status != STATUS_STALE_HANDLE {
            return status;
        }
        pump.runtime_retained.store(false, Ordering::Release);
    }
    let status = unsafe { c_api::ptyx_runtime_release(&mut runtime, ptr::null_mut()) };
    if status == STATUS_OK {
        pump.cleaned.store(true, Ordering::Release);
    }
    status
}

fn release_session_handle(handle: u64) -> u32 {
    let candidates = pumps()
        .lock()
        .map(|registry| registry.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let owner = candidates.into_iter().find(|pump| {
        pump.state
            .lock()
            .map(|state| state.sessions.contains(&handle))
            .unwrap_or(false)
    });
    if let Some(owner) = owner {
        let status = release_tracked_session(&owner, handle, || {
            let mut released = handle;
            unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) }
        });
        if matches!(status, STATUS_OK | STATUS_STALE_HANDLE) {
            let mut event = Event::empty(size_of::<Event>() as u32);
            event.kind = EVENT_CLOSE_COMPLETE;
            event.session = handle;
            unsafe {
                post_event(owner.port, &event);
            }
        }
        return status;
    }
    let mut released = handle;
    unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) }
}

fn release_tracked_session(pump: &Pump, handle: u64, release: impl FnOnce() -> u32) -> u32 {
    let Ok(_cleanup) = pump.cleanup.lock() else {
        return STATUS_INTERNAL;
    };
    let Ok(mut state) = pump.state.lock() else {
        return STATUS_INTERNAL;
    };
    if !state.sessions.contains(&handle) {
        return STATUS_STALE_HANDLE;
    }
    let status = release();
    if matches!(status, STATUS_OK | STATUS_STALE_HANDLE) {
        state.sessions.remove(&handle);
    }
    status
}

static LIBRARY_PINNED: OnceLock<bool> = OnceLock::new();

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pin_native_library() -> bool {
    let mut information = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    let address = ptyd_initialize as *const () as *const c_void;
    if unsafe { libc::dladdr(address, information.as_mut_ptr()) } == 0 {
        return false;
    }
    let information = unsafe { information.assume_init() };
    if information.dli_fname.is_null() {
        return false;
    }
    !unsafe { libc::dlopen(information.dli_fname, libc::RTLD_NOW | libc::RTLD_NODELETE) }.is_null()
}

#[cfg(windows)]
fn pin_native_library() -> bool {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
    };

    let mut module: HMODULE = ptr::null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            ptyd_initialize as *const () as *const u16,
            &mut module,
        ) != 0
    }
}

unsafe extern "C" {
    fn Dart_InitializeApiDL(data: *mut c_void) -> libc::intptr_t;
    fn ptyx_dart_post_event(
        port: i64,
        kind: u32,
        session: u64,
        token: u64,
        flags: u32,
        value: i64,
        error_domain: u32,
        error_kind: u32,
        native_code: i32,
        bytes: *const u8,
        length: isize,
    ) -> bool;
}

#[cfg(test)]
mod tests {
    use super::{
        acknowledge_token, attempt_cleanup, begin_event_wait, ptyd_runtime_abort,
        ptyd_runtime_detach, pumps, release_tracked_session, Cleanup, Pump,
    };
    use ptyx_c::private::{self as c_api, Error, STATUS_INTERNAL, STATUS_OK};
    use std::sync::Arc;

    #[test]
    fn event_wait_registration_obeys_stop_state() {
        let pump = Pump::new(0, 1);
        assert!(begin_event_wait(&pump).is_some());
        drop(pump.state.lock().map(|mut state| state.stopping = true));
        assert!(begin_event_wait(&pump).is_none());
    }

    #[test]
    fn concurrent_detach_releases_runtime_once() {
        let mut runtime = 0;
        let mut error = Error::none();
        let status =
            unsafe { c_api::ptyx_runtime_create(std::ptr::null(), &mut runtime, &mut error) };
        assert_eq!(status, STATUS_OK);
        let pump = Arc::new(Pump::new(runtime, 1));
        pump.runtime_retained
            .store(false, std::sync::atomic::Ordering::Release);
        let handle = pumps()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(Arc::clone(&pump));

        let cleanup = std::thread::spawn(move || attempt_cleanup(&Cleanup::Adapter(handle)));
        let mut explicit = handle;
        let status = unsafe { ptyd_runtime_detach(&mut explicit, &mut error) };
        cleanup.join().expect("cleanup worker panicked");

        assert_eq!(status, STATUS_OK);
        assert_eq!(explicit, 0);
        assert!(pump.cleaned.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn abort_releases_the_adapter_idempotently() {
        let mut runtime = 0;
        let mut error = Error::none();
        let status =
            unsafe { c_api::ptyx_runtime_create(std::ptr::null(), &mut runtime, &mut error) };
        assert_eq!(status, STATUS_OK);
        let pump = Arc::new(Pump::new(runtime, 1));
        pump.runtime_retained
            .store(false, std::sync::atomic::Ordering::Release);
        let handle = pumps()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(Arc::clone(&pump));

        let mut adapter = handle;
        assert_eq!(
            unsafe { ptyd_runtime_abort(&mut adapter, &mut error) },
            STATUS_OK
        );
        assert_eq!(adapter, 0);
        assert!(pump.cleaned.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn session_finalization_is_serialized_with_adapter_cleanup() {
        let pump = Arc::new(Pump::new(0, 1));
        pump.state.lock().expect("pump state").sessions.insert(7);
        let cleanup = pump.cleanup.lock().expect("cleanup lock");
        let (started, waiting) = std::sync::mpsc::channel();
        let finalizer = std::thread::spawn({
            let pump = Arc::clone(&pump);
            move || {
                started.send(()).expect("report finalizer start");
                release_tracked_session(&pump, 7, || STATUS_OK)
            }
        });
        waiting.recv().expect("finalizer started");

        assert!(
            pump.state.lock().expect("pump state").sessions.contains(&7),
            "adapter cleanup must still observe a session while finalization waits"
        );

        drop(cleanup);
        assert_eq!(finalizer.join().expect("finalizer thread"), STATUS_OK);
        assert!(!pump.state.lock().expect("pump state").sessions.contains(&7));
    }

    #[test]
    fn failed_event_release_retains_the_token_for_retry() {
        let pump = Pump::new(0, 1);
        pump.state.lock().expect("pump state").outstanding.insert(7);

        assert_eq!(
            acknowledge_token(&pump, 7, || STATUS_INTERNAL),
            STATUS_INTERNAL
        );
        assert!(pump
            .state
            .lock()
            .expect("pump state")
            .outstanding
            .contains(&7));

        assert_eq!(acknowledge_token(&pump, 7, || STATUS_OK), STATUS_OK);
        assert!(!pump
            .state
            .lock()
            .expect("pump state")
            .outstanding
            .contains(&7));
    }
}
