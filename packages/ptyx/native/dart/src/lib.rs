use ptyx_c::private as c_api;
use ptyx_c::private::{
    Error, Event, Registry, SpawnOptions, ERROR_DOMAIN_ARGUMENT, ERROR_DOMAIN_RUNTIME,
    ERROR_DOMAIN_STATE, ERROR_INFRASTRUCTURE_LOST, ERROR_INVALID_ARGUMENT, ERROR_STALE_HANDLE,
    ERROR_WRONG_STATE, OPERATION_CLOSE, OPERATION_OUTPUT, OPERATION_RUNTIME_CREATE,
    OPERATION_RUNTIME_SHUTDOWN, STATUS_INTERNAL, STATUS_INVALID_ARGUMENT, STATUS_OK,
    STATUS_STALE_HANDLE, STATUS_WRONG_STATE,
};
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
#[cfg(feature = "test-controls")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

const EVENT_SPAWN_FAILED: u32 = 2;
const EVENT_OUTPUT: u32 = 3;
const EVENT_INFRASTRUCTURE_FAILED: u32 = 6;
const EVENT_CLOSE_COMPLETE: u32 = 9;
static DART_INITIALIZED: AtomicBool = AtomicBool::new(false);

struct PumpState {
    stopping: bool,
    outstanding: HashSet<u64>,
    sessions: HashSet<u64>,
}

struct Pump {
    runtime: u64,
    port: i64,
    state: Mutex<PumpState>,
    cleanup: Mutex<()>,
    cleaned: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Pump {
    fn new(runtime: u64, port: i64) -> Self {
        Self {
            runtime,
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

fn pumps() -> &'static Mutex<Registry<Arc<Pump>>> {
    static PUMPS: OnceLock<Mutex<Registry<Arc<Pump>>>> = OnceLock::new();
    PUMPS.get_or_init(|| Mutex::new(Registry::new()))
}

fn pump(handle: u64) -> Option<Arc<Pump>> {
    pumps().lock().ok()?.get(handle).map(Arc::clone)
}

fn cleanup_sender() -> Option<&'static Sender<u64>> {
    static CLEANUP: OnceLock<Option<Sender<u64>>> = OnceLock::new();
    CLEANUP
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            thread::Builder::new()
                .name("ptyx-dart-cleanup".into())
                .spawn(move || {
                    while let Ok(handle) = receiver.recv() {
                        detach_handle(handle);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
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
        #[cfg(feature = "test-controls")]
        delay_next_attach();
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
        let mut capabilities = 0;
        let status = c_api::ptyx_runtime_capabilities(runtime, &mut capabilities, error);
        if status != STATUS_OK {
            return status;
        }
        let value = Arc::new(Pump::new(runtime, port));
        let handle = {
            let Ok(mut registry) = pumps().lock() else {
                return fail(
                    error,
                    STATUS_INTERNAL,
                    ERROR_DOMAIN_RUNTIME,
                    ERROR_INFRASTRUCTURE_LOST,
                    OPERATION_RUNTIME_CREATE,
                );
            };
            registry.insert(Arc::clone(&value))
        };
        let worker = match thread::Builder::new()
            .name("ptyx-dart-events".into())
            .spawn({
                let value = Arc::clone(&value);
                move || pump_events(&value)
            }) {
            Ok(worker) => worker,
            Err(_) => {
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
        if status == STATUS_OK {
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
        {
            let Ok(mut state) = pump.state.lock() else {
                return STATUS_INTERNAL;
            };
            if !state.outstanding.remove(&token) {
                return fail(
                    error,
                    STATUS_STALE_HANDLE,
                    ERROR_DOMAIN_STATE,
                    ERROR_STALE_HANDLE,
                    OPERATION_OUTPUT,
                );
            }
        }
        release_event(token, error)
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
    guarded(error, OPERATION_RUNTIME_SHUTDOWN, || {
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
        stop_pump(&value);
        if let Some(worker) = value
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = worker.join();
        }
        let status = cleanup_pump(&value);
        if status != STATUS_OK {
            return status;
        }
        if let Ok(mut registry) = pumps().lock() {
            registry.remove(handle);
        }
        *adapter = 0;
        STATUS_OK
    })
}

#[no_mangle]
pub extern "C" fn ptyd_runtime_finalize(token: *mut c_void) {
    let handle = token.addr() as u64;
    if handle == 0 {
        return;
    }
    if let Some(sender) = cleanup_sender() {
        let _ = sender.send(handle);
    }
}

fn pump_events(pump: &Arc<Pump>) {
    loop {
        if pump
            .state
            .lock()
            .map(|state| state.stopping)
            .unwrap_or(true)
        {
            break;
        }

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

        if event.kind == EVENT_OUTPUT {
            let mut state = pump
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.stopping {
                drop(state);
                unsafe {
                    release_event(event.token, ptr::null_mut());
                }
                break;
            }
            state.outstanding.insert(event.token);
            let posted = unsafe { post_event(pump.port, &event) };
            if posted {
                continue;
            }
            state.outstanding.remove(&event.token);
            drop(state);
            unsafe {
                release_event(event.token, ptr::null_mut());
                post_terminal_failure(pump.port);
            }
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
            release_completed_session(pump, session);
        }
    }
    stop_pump(pump);
    let _ = cleanup_pump(pump);
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
        event.error.operation,
        event.error.native_code,
        event.error.flags,
        event.data,
        event.data_length as isize,
    )
}

fn release_completed_session(pump: &Pump, session: u64) {
    let Ok(mut state) = pump.state.lock() else {
        return;
    };
    if !state.sessions.contains(&session) {
        return;
    }
    let mut released = session;
    let status = unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) };
    if status == STATUS_OK || status == STATUS_STALE_HANDLE {
        state.sessions.remove(&session);
    }
}

unsafe fn release_event(token: u64, error: *mut Error) -> u32 {
    let mut event = Event::empty(size_of::<Event>() as u32);
    event.token = token;
    c_api::ptyx_event_release(&mut event, error)
}

fn stop_pump(pump: &Pump) {
    if let Ok(mut state) = pump.state.lock() {
        state.stopping = true;
    }
    unsafe {
        c_api::ptyx_runtime_shutdown(pump.runtime, ptr::null_mut());
    }
}

fn cleanup_pump(pump: &Pump) -> u32 {
    let Ok(_cleanup) = pump.cleanup.lock() else {
        return STATUS_INTERNAL;
    };
    if pump.cleaned.load(Ordering::Acquire) {
        return STATUS_OK;
    }
    stop_pump(pump);
    let (events, sessions) = {
        let mut state = pump
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let events = state.outstanding.drain().collect::<Vec<_>>();
        let sessions = state.sessions.iter().copied().collect::<Vec<_>>();
        (events, sessions)
    };
    for token in events {
        unsafe {
            release_event(token, ptr::null_mut());
        }
    }
    for session in sessions {
        let mut released = session;
        let status = unsafe { c_api::ptyx_session_release(&mut released, ptr::null_mut()) };
        if status == STATUS_OK || status == STATUS_STALE_HANDLE {
            if let Ok(mut state) = pump.state.lock() {
                state.sessions.remove(&session);
            }
        }
    }
    let mut runtime = pump.runtime;
    let status = unsafe { c_api::ptyx_runtime_release(&mut runtime, ptr::null_mut()) };
    if status == STATUS_OK {
        pump.cleaned.store(true, Ordering::Release);
    }
    status
}

fn detach_handle(handle: u64) {
    let mut handle = handle;
    unsafe {
        ptyd_runtime_detach(&mut handle, ptr::null_mut());
    }
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

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_fail_next_post() {
    unsafe {
        ptyx_dart_test_fail_next_post();
    }
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_kill_broker() -> u32 {
    let pump = pumps()
        .lock()
        .ok()
        .and_then(|registry| registry.sole().map(Arc::clone));
    pump.map_or(0, |pump| u32::from(c_api::test_kill_broker(pump.runtime)))
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_delay_next_spawn(milliseconds: u64) {
    c_api::test_delay_next_spawn(usize::try_from(milliseconds).unwrap_or(usize::MAX));
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_spawn_delay_active() -> u32 {
    u32::from(c_api::test_spawn_delay_active())
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_fail_next_write() {
    c_api::test_fail_next_write();
}

#[cfg(feature = "test-controls")]
static ATTACH_DELAY_MILLISECONDS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "test-controls")]
static ATTACH_DELAY_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "test-controls")]
fn delay_next_attach() {
    let milliseconds = ATTACH_DELAY_MILLISECONDS.swap(0, Ordering::AcqRel);
    if milliseconds == 0 {
        return;
    }
    ATTACH_DELAY_ACTIVE.store(true, Ordering::Release);
    std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    ATTACH_DELAY_ACTIVE.store(false, Ordering::Release);
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_delay_next_attach(milliseconds: u64) {
    ATTACH_DELAY_MILLISECONDS.store(milliseconds, Ordering::Release);
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_attach_delay_active() -> u32 {
    u32::from(ATTACH_DELAY_ACTIVE.load(Ordering::Acquire))
}

#[cfg(feature = "test-controls")]
#[no_mangle]
pub extern "C" fn ptyd_test_adapter_count() -> u32 {
    pumps()
        .lock()
        .map_or(u32::MAX, |registry| registry.live_count() as u32)
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
        error_operation: u32,
        native_code: i32,
        error_flags: u32,
        bytes: *const u8,
        length: isize,
    ) -> bool;
    #[cfg(feature = "test-controls")]
    fn ptyx_dart_test_fail_next_post();
}

#[cfg(test)]
mod tests {
    use super::{detach_handle, ptyd_runtime_detach, pumps, Pump};
    use ptyx_c::private::{self as c_api, Error, STATUS_OK};
    use std::sync::Arc;

    #[test]
    fn concurrent_detach_releases_runtime_once() {
        let mut runtime = 0;
        let mut error = Error::none();
        let status =
            unsafe { c_api::ptyx_runtime_create(std::ptr::null(), &mut runtime, &mut error) };
        assert_eq!(status, STATUS_OK);
        let pump = Arc::new(Pump::new(runtime, 1));
        let handle = pumps()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(Arc::clone(&pump));

        let cleanup = std::thread::spawn(move || detach_handle(handle));
        let mut explicit = handle;
        let status = unsafe { ptyd_runtime_detach(&mut explicit, &mut error) };
        cleanup.join().expect("cleanup worker panicked");

        assert_eq!(status, STATUS_OK);
        assert_eq!(explicit, 0);
        assert!(pump.cleaned.load(std::sync::atomic::Ordering::Acquire));
    }
}
