use super::{IntegratedRuntime, Notice};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::broker_client::BrokerSpawn;
#[cfg(windows)]
use crate::windows::BrokerSpawn;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::io;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
#[cfg(feature = "test-controls")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
struct Ports {
    output: i64,
    event: i64,
}

static RUNTIME: OnceLock<IntegratedRuntime> = OnceLock::new();
static LIBRARY_PINNED: OnceLock<bool> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());
static DART_CALL_LOCK: Mutex<()> = Mutex::new(());
static PORTS: OnceLock<Mutex<HashMap<u64, Ports>>> = OnceLock::new();
static LOST_PORTS: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
static ABANDONMENTS: OnceLock<Sender<u64>> = OnceLock::new();
static COMMAND_INPUT_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "test-controls")]
static TEST_SPAWN_DELAY_MS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "test-controls")]
static TEST_SPAWN_DELAY_ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "test-controls")]
static TEST_WRITE_INFRASTRUCTURE_FAILURE: AtomicBool = AtomicBool::new(false);
const COMMAND_INPUT_CAPACITY: usize = 64 * 1024 * 1024;
const MAX_SESSION_CAPACITY: usize = 64 * 1024 * 1024;
const MAX_SPAWN_PAYLOAD: usize = 64 * 1024;
const MAX_ARGUMENTS: usize = 256;
const MAX_ENVIRONMENT: usize = 4096;

thread_local! {
    static LAST_ERROR_CODE: Cell<i32> = const { Cell::new(0) };
}

fn set_last_error_code(code: i32) {
    LAST_ERROR_CODE.set(code);
}

fn native_error_code(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or_else(|| {
        if error.kind() == io::ErrorKind::Unsupported {
            #[cfg(windows)]
            {
                50 // ERROR_NOT_SUPPORTED
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                libc::ENOTSUP
            }
        } else {
            libc::EIO
        }
    })
}

unsafe fn bounded_c_string(
    pointer: *const libc::c_char,
    remaining: &mut usize,
) -> Result<CString, i32> {
    if pointer.is_null() {
        return Err(libc::EINVAL);
    }
    let maximum = remaining.saturating_add(1);
    let length = libc::strnlen(pointer, maximum);
    if length > *remaining {
        return Err(libc::E2BIG);
    }
    let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), length);
    *remaining -= length;
    CString::new(bytes).map_err(|_| libc::EINVAL)
}

struct InputAdmission(usize);

impl InputAdmission {
    fn acquire(bytes: usize) -> Option<Self> {
        COMMAND_INPUT_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|total| *total <= COMMAND_INPUT_CAPACITY)
            })
            .ok()
            .map(|_| Self(bytes))
    }
}

impl Drop for InputAdmission {
    fn drop(&mut self) {
        COMMAND_INPUT_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

#[no_mangle]
pub extern "C" fn ptyi_abi_version() -> u32 {
    7
}

#[no_mangle]
pub extern "C" fn ptyi_capabilities() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        1 | 2 | 4 | 16
    }
    #[cfg(windows)]
    {
        8
    }
}

#[no_mangle]
pub extern "C" fn ptyi_last_error_code() -> i32 {
    LAST_ERROR_CODE.get()
}

fn runtime() -> Option<&'static IntegratedRuntime> {
    RUNTIME.get()
}

fn with_runtime<R>(operation: impl FnOnce(&IntegratedRuntime) -> R) -> Option<R> {
    Some(operation(runtime()?))
}

fn ports() -> &'static Mutex<HashMap<u64, Ports>> {
    PORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lost_ports() -> &'static Mutex<HashSet<u64>> {
    LOST_PORTS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pin_native_library() -> bool {
    let mut information = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    let address = ptyi_init as *const () as *const c_void;
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

    let mut module: HMODULE = std::ptr::null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            ptyi_init as *const () as *const u16,
            &mut module,
        ) != 0
    }
}

#[no_mangle]
#[cfg(feature = "test-controls")]
pub extern "C" fn ptyi_test_fail_next_post() {
    unsafe {
        ptyx_dart_fail_next_post();
    }
}

#[no_mangle]
#[cfg(feature = "test-controls")]
pub extern "C" fn ptyi_test_kill_broker() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = with_runtime(IntegratedRuntime::kill_broker_for_test);
}

#[no_mangle]
#[cfg(feature = "test-controls")]
pub extern "C" fn ptyi_test_delay_next_spawn(milliseconds: usize) {
    TEST_SPAWN_DELAY_MS.store(milliseconds, Ordering::Release);
}

#[no_mangle]
#[cfg(feature = "test-controls")]
pub extern "C" fn ptyi_test_spawn_delay_active() -> bool {
    TEST_SPAWN_DELAY_ACTIVE.load(Ordering::Acquire)
}

#[no_mangle]
#[cfg(feature = "test-controls")]
pub extern "C" fn ptyi_test_fail_next_write_infrastructure() {
    TEST_WRITE_INFRASTRUCTURE_FAILURE.store(true, Ordering::Release);
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_init(api_data: *mut c_void) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if api_data.is_null() {
            return false;
        }
        if !*LIBRARY_PINNED.get_or_init(pin_native_library) {
            return false;
        }
        let Ok(_initializing) = INIT_LOCK.lock() else {
            return false;
        };
        if RUNTIME.get().is_some() {
            return true;
        }
        if Dart_InitializeApiDL(api_data) != 0 {
            return false;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let initialized = crate::isolate_thread::run(initialize_runtime).unwrap_or(false);
        #[cfg(windows)]
        let initialized = initialize_runtime();
        initialized
    }))
    .unwrap_or(false)
}

fn initialize_runtime() -> bool {
    let Ok(runtime) = IntegratedRuntime::try_new() else {
        return false;
    };
    let Some(notifications) = runtime.take_notifications() else {
        return false;
    };
    let notifier = std::thread::Builder::new()
        .name("ptyx-dart-notifier".to_owned())
        .spawn(move || {
            while let Some((_, notice)) = notifications.recv() {
                dispatch_notice(notice);
            }
        });
    if notifier.is_err() {
        return false;
    }
    let (abandon_sender, abandon_receiver) = mpsc::channel();
    let abandoner = std::thread::Builder::new()
        .name("ptyx-finalizer".to_owned())
        .spawn(move || {
            while let Ok(handle) = abandon_receiver.recv() {
                while !with_runtime(|runtime| runtime.try_abandon(handle)).unwrap_or(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        });
    if abandoner.is_err() || RUNTIME.set(runtime).is_err() {
        return false;
    }
    ABANDONMENTS.set(abandon_sender).is_ok()
}

#[no_mangle]
pub extern "C" fn ptyi_finalize(token: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = token.addr() as u64;
        abandon_handle(handle);
    }));
}

#[no_mangle]
pub extern "C" fn ptyi_abandon(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| abandon_handle(handle))).unwrap_or(false)
}

fn abandon_handle(handle: u64) -> bool {
    if handle == 0 {
        return false;
    }
    let Ok(_dart_calls) = DART_CALL_LOCK.lock() else {
        return false;
    };
    if let Ok(mut entries) = ports().lock() {
        entries.remove(&handle);
    }
    if let Ok(mut lost) = lost_ports().lock() {
        lost.insert(handle);
    }
    abandon_lost(handle)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_spawn(
    executable: *const libc::c_char,
    arguments: *const *const libc::c_char,
    argument_count: usize,
    environment: *const *const libc::c_char,
    environment_count: usize,
    inherit_environment: bool,
    cwd: *const libc::c_char,
    rows: u32,
    columns: u32,
    pixel_width: u32,
    pixel_height: u32,
    input_capacity: usize,
    output_capacity: usize,
) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        set_last_error_code(0);
        if executable.is_null()
            || argument_count > MAX_ARGUMENTS
            || (argument_count != 0
                && (arguments.is_null()
                    || argument_count > isize::MAX as usize / size_of::<*const libc::c_char>()))
            || (!inherit_environment
                && (environment_count > MAX_ENVIRONMENT
                    || (environment_count != 0
                        && (environment.is_null()
                            || environment_count
                                > isize::MAX as usize / size_of::<*const libc::c_char>()))))
            || input_capacity == 0
            || input_capacity > MAX_SESSION_CAPACITY
            || output_capacity == 0
            || output_capacity > MAX_SESSION_CAPACITY
            || rows == 0
            || rows > u16::MAX.into()
            || columns == 0
            || columns > u16::MAX.into()
            || pixel_width > u16::MAX.into()
            || pixel_height > u16::MAX.into()
        {
            set_last_error_code(libc::EINVAL);
            return 0;
        }
        let environment_payload_count = if inherit_environment {
            0
        } else {
            environment_count
        };
        let overhead = 36_usize.saturating_add(
            4_usize.saturating_mul(
                1_usize
                    .saturating_add(argument_count)
                    .saturating_add(environment_payload_count),
            ),
        );
        let Some(mut remaining) = MAX_SPAWN_PAYLOAD.checked_sub(overhead) else {
            set_last_error_code(libc::E2BIG);
            return 0;
        };
        let executable = match bounded_c_string(executable, &mut remaining) {
            Ok(value) if !value.is_empty() => value,
            Ok(_) => {
                set_last_error_code(libc::EINVAL);
                return 0;
            }
            Err(code) => {
                set_last_error_code(code);
                return 0;
            }
        };
        let argument_pointers = if argument_count == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(arguments, argument_count)
        };
        let mut owned_arguments = Vec::with_capacity(argument_count);
        for argument in argument_pointers {
            match bounded_c_string(*argument, &mut remaining) {
                Ok(value) => owned_arguments.push(value),
                Err(code) => {
                    set_last_error_code(code);
                    return 0;
                }
            }
        }
        let environment_pointers = if inherit_environment || environment_count == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(environment, environment_count)
        };
        let environment = if inherit_environment {
            None
        } else {
            let mut values = Vec::with_capacity(environment_count);
            for entry in environment_pointers {
                match bounded_c_string(*entry, &mut remaining) {
                    Ok(value) => values.push(value),
                    Err(code) => {
                        set_last_error_code(code);
                        return 0;
                    }
                }
            }
            Some(values)
        };
        let cwd = if cwd.is_null() {
            None
        } else {
            match bounded_c_string(cwd, &mut remaining) {
                Ok(value) => Some(value),
                Err(code) => {
                    set_last_error_code(code);
                    return 0;
                }
            }
        };
        let config = BrokerSpawn {
            executable,
            arguments: owned_arguments,
            environment,
            cwd,
            rows,
            columns,
            pixel_width,
            pixel_height,
        };
        let handle = match with_runtime(|runtime| {
            runtime.spawn_staged(config, input_capacity, output_capacity)
        }) {
            Some(Ok(handle)) => handle,
            Some(Err(error)) => {
                set_last_error_code(native_error_code(&error));
                0
            }
            None => {
                set_last_error_code(libc::EPIPE);
                0
            }
        };
        if handle == 0 {
            return 0;
        }
        #[cfg(feature = "test-controls")]
        {
            let delay = TEST_SPAWN_DELAY_MS.swap(0, Ordering::AcqRel);
            if delay != 0 {
                TEST_SPAWN_DELAY_ACTIVE.store(true, Ordering::Release);
                std::thread::sleep(std::time::Duration::from_millis(delay as u64));
                TEST_SPAWN_DELAY_ACTIVE.store(false, Ordering::Release);
            }
        }
        handle
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn ptyi_activate(handle: u64, output_port: i64, event_port: i64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if output_port <= 0 || event_port <= 0 {
            set_last_error_code(libc::EINVAL);
            return false;
        }
        {
            let Ok(_dart_calls) = DART_CALL_LOCK.lock() else {
                return false;
            };
            let Ok(mut entries) = ports().lock() else {
                return false;
            };
            entries.insert(
                handle,
                Ports {
                    output: output_port,
                    event: event_port,
                },
            );
        }
        let activated = with_runtime(|runtime| runtime.activate(handle)).unwrap_or(false);
        set_last_error_code(if activated { 0 } else { libc::EPIPE });
        if !activated {
            if let Ok(_dart_calls) = DART_CALL_LOCK.lock() {
                if let Ok(mut entries) = ports().lock() {
                    entries.remove(&handle);
                }
            }
        }
        activated
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_write(handle: u64, bytes: *const u8, length: usize) -> i64 {
    match catch_unwind(AssertUnwindSafe(|| {
        if bytes.is_null() || length == 0 || length > isize::MAX as usize {
            set_last_error_code(libc::EINVAL);
            return -1;
        }
        let Some(_admission) = InputAdmission::acquire(length) else {
            return 0;
        };
        let bytes = std::slice::from_raw_parts(bytes, length).to_vec();
        #[cfg(feature = "test-controls")]
        if TEST_WRITE_INFRASTRUCTURE_FAILURE.swap(false, Ordering::AcqRel) {
            let _ = abandon_handle(handle);
            set_last_error_code(libc::EIO);
            return crate::WRITE_INFRASTRUCTURE_FAILURE;
        }
        let result = with_runtime(|runtime| runtime.write(handle, bytes))
            .unwrap_or(crate::WRITE_INFRASTRUCTURE_FAILURE);
        if result == crate::WRITE_INFRASTRUCTURE_FAILURE {
            set_last_error_code(libc::EIO);
        } else if result < 0 {
            set_last_error_code(libc::EPIPE);
        } else {
            set_last_error_code(0);
        }
        result
    })) {
        Ok(result) => result,
        Err(_) => {
            set_last_error_code(libc::EIO);
            crate::WRITE_INFRASTRUCTURE_FAILURE
        }
    }
}

#[no_mangle]
pub extern "C" fn ptyi_credit_async(handle: u64, bytes: usize) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.credit_async(handle, bytes)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_pause(handle: u64, paused: bool) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.pause(handle, paused)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_exit_status(handle: u64, status: *mut i64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if status.is_null() {
            return false;
        }
        let Some(result) = with_runtime(|runtime| runtime.exit_status(handle)).flatten() else {
            return false;
        };
        *status = result;
        true
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_pid(handle: u64) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        let result = with_runtime(|runtime| runtime.pid(handle))
            .flatten()
            .map_or(-1, i64::from);
        set_last_error_code(if result < 0 { libc::EPIPE } else { 0 });
        result
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_size(handle: u64, values: *mut u32) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if values.is_null() {
            return false;
        }
        let Some(size) = with_runtime(|runtime| runtime.size(handle)).flatten() else {
            set_last_error_code(libc::EPIPE);
            return false;
        };
        ptr::copy_nonoverlapping(size.as_ptr(), values, size.len());
        set_last_error_code(0);
        true
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_resize(
    handle: u64,
    rows: u32,
    columns: u32,
    pixel_width: u32,
    pixel_height: u32,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let result = with_runtime(|runtime| {
            runtime.resize(handle, [rows, columns, pixel_width, pixel_height])
        })
        .unwrap_or(false);
        set_last_error_code(if result { 0 } else { libc::EIO });
        result
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_signal(handle: u64, signal: i32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let result = with_runtime(|runtime| runtime.signal(handle, signal))
            .flatten()
            .map_or(-1, i32::from);
        set_last_error_code(if result < 0 { libc::EIO } else { 0 });
        result
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn ptyi_mode(handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let result = with_runtime(|runtime| runtime.mode(handle))
            .flatten()
            .map_or(-1, |mode| {
                i32::from(mode[0]) | (i32::from(mode[1]) << 1) | (i32::from(mode[2]) << 2)
            });
        set_last_error_code(if result < 0 { libc::EIO } else { 0 });
        result
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_tty_name(handle: u64, target: *mut u8, capacity: usize) -> isize {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(name) = with_runtime(|runtime| runtime.tty_name(handle)).flatten() else {
            set_last_error_code(libc::EIO);
            return -1;
        };
        set_last_error_code(0);
        if target.is_null() || capacity < name.len() {
            return name.len() as isize;
        }
        ptr::copy_nonoverlapping(name.as_ptr(), target, name.len());
        name.len() as isize
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn ptyi_close(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let result = with_runtime(|runtime| runtime.close(handle)).unwrap_or(false);
        set_last_error_code(if result { 0 } else { libc::EIO });
        result
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_destroy(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let removed = with_runtime(|runtime| runtime.destroy(handle)).unwrap_or(false);
        if removed {
            let Ok(_dart_calls) = DART_CALL_LOCK.lock() else {
                return false;
            };
            if let Ok(mut entries) = ports().lock() {
                entries.remove(&handle);
            }
            if let Ok(mut lost) = lost_ports().lock() {
                lost.remove(&handle);
            }
        }
        removed
    }))
    .unwrap_or(false)
}

fn dispatch_notice(notice: Notice) {
    let Ok(_dart_calls) = DART_CALL_LOCK.lock() else {
        return;
    };
    let broker_lost = matches!(notice, Notice::BrokerLost(_));
    let handle = match &notice {
        Notice::Output { handle, .. }
        | Notice::InputFailed(handle)
        | Notice::OutputFailed(handle)
        | Notice::BrokerLost(handle)
        | Notice::OutputDone(handle)
        | Notice::Exit(handle) => *handle,
    };
    let Some(entry) = ports()
        .lock()
        .ok()
        .and_then(|entries| entries.get(&handle).copied())
    else {
        abandon_lost(handle);
        return;
    };
    let posted = unsafe {
        match notice {
            Notice::Output { bytes, .. } => {
                ptyx_dart_post_bytes(entry.output, handle as i64, bytes.as_ptr(), bytes.len())
            }
            Notice::InputFailed(_) => ptyx_dart_post_integer(entry.event, (handle << 3) as i64),
            Notice::OutputFailed(_) => {
                ptyx_dart_post_integer(entry.event, ((handle << 3) | 6) as i64)
            }
            Notice::BrokerLost(_) => {
                ptyx_dart_post_integer(entry.event, ((handle << 3) | 7) as i64)
            }
            Notice::Exit(_) => ptyx_dart_post_integer(entry.event, ((handle << 3) | 3) as i64),
            Notice::OutputDone(_) => {
                ptyx_dart_post_integer(entry.event, ((handle << 3) | 4) as i64)
            }
        }
    };
    if broker_lost {
        if let Ok(mut entries) = ports().lock() {
            entries.remove(&handle);
        }
    }
    if !posted {
        unsafe {
            ptyx_dart_post_integer(entry.event, ((handle << 3) | 7) as i64);
        }
        if let Ok(mut entries) = ports().lock() {
            entries.remove(&handle);
        }
        if let Ok(mut lost) = lost_ports().lock() {
            lost.insert(handle);
        }
        abandon_lost(handle);
    }
}

fn abandon_lost(handle: u64) -> bool {
    let queued = lost_ports()
        .lock()
        .ok()
        .is_some_and(|mut lost| lost.remove(&handle));
    if !queued {
        return true;
    }
    let accepted = ABANDONMENTS
        .get()
        .is_some_and(|sender| sender.send(handle).is_ok());
    if !accepted {
        if let Ok(mut lost) = lost_ports().lock() {
            lost.insert(handle);
        }
    }
    accepted
}

unsafe extern "C" {
    fn Dart_InitializeApiDL(data: *mut c_void) -> libc::intptr_t;
    fn ptyx_dart_post_integer(port: i64, message: i64) -> bool;
    fn ptyx_dart_post_bytes(port: i64, handle: i64, bytes: *const u8, length: usize) -> bool;
    #[cfg(feature = "test-controls")]
    fn ptyx_dart_fail_next_post();
}
