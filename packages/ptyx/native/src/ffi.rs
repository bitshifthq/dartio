use super::{IntegratedRuntime, Notice, WaitResult};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::broker_client::BrokerSpawn;
#[cfg(windows)]
use crate::windows::BrokerSpawn;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy)]
struct Ports {
    output: i64,
    event: i64,
}

static RUNTIME: OnceLock<Mutex<IntegratedRuntime>> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());
static PORTS: OnceLock<Mutex<HashMap<u64, Ports>>> = OnceLock::new();
static LOST_PORTS: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
static COMMAND_INPUT_BYTES: AtomicUsize = AtomicUsize::new(0);
const COMMAND_INPUT_CAPACITY: usize = 64 * 1024 * 1024;

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
    2
}

#[no_mangle]
pub extern "C" fn ptyi_capabilities() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        1 | 2 | 4
    }
    #[cfg(windows)]
    {
        8
    }
}

fn runtime() -> Option<&'static Mutex<IntegratedRuntime>> {
    RUNTIME.get()
}

fn with_runtime<R>(operation: impl FnOnce(&IntegratedRuntime) -> R) -> Option<R> {
    let runtime = runtime()?.lock().ok()?;
    Some(operation(&runtime))
}

fn ports() -> &'static Mutex<HashMap<u64, Ports>> {
    PORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lost_ports() -> &'static Mutex<HashSet<u64>> {
    LOST_PORTS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[no_mangle]
pub extern "C" fn ptyi_test_fail_next_post() {
    unsafe {
        ptyx_dart_fail_next_post();
    }
}

#[no_mangle]
pub extern "C" fn ptyi_test_kill_broker() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = with_runtime(IntegratedRuntime::kill_broker_for_test);
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_init(api_data: *mut c_void) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if api_data.is_null() || Dart_InitializeApiDL(api_data) != 0 {
            return false;
        }
        let Ok(_initializing) = INIT_LOCK.lock() else {
            return false;
        };
        if RUNTIME.get().is_some() {
            return true;
        }
        let Ok(mut runtime) = IntegratedRuntime::try_new() else {
            return false;
        };
        let Some(notifications) = runtime.take_notifications() else {
            return false;
        };
        let notifier = std::thread::Builder::new()
            .name("ptyx-dart-notifier".to_owned())
            .spawn(move || {
                while let Ok(notice) = notifications.recv() {
                    dispatch_notice(notice);
                }
            });
        if notifier.is_err() {
            return false;
        }
        RUNTIME.set(Mutex::new(runtime)).is_ok()
    }))
    .unwrap_or(false)
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
    output_port: i64,
    event_port: i64,
) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        if executable.is_null() || (argument_count != 0 && arguments.is_null()) {
            return 0;
        }
        let executable = CStr::from_ptr(executable);
        let arguments = std::slice::from_raw_parts(arguments, argument_count)
            .iter()
            .map(|argument| (!argument.is_null()).then(|| CStr::from_ptr(*argument).to_owned()))
            .collect::<Option<Vec<CString>>>();
        let Some(arguments) = arguments else {
            return 0;
        };
        if !inherit_environment && environment_count != 0 && environment.is_null() {
            return 0;
        }
        let environment = if inherit_environment {
            None
        } else {
            let values = std::slice::from_raw_parts(environment, environment_count)
                .iter()
                .map(|entry| (!entry.is_null()).then(|| CStr::from_ptr(*entry).to_owned()))
                .collect::<Option<Vec<CString>>>();
            let Some(values) = values else {
                return 0;
            };
            Some(values)
        };
        let cwd = (!cwd.is_null()).then(|| CStr::from_ptr(cwd).to_owned());
        let config = BrokerSpawn {
            executable: executable.to_owned(),
            arguments,
            environment,
            cwd,
            rows,
            columns,
            pixel_width,
            pixel_height,
        };
        let handle = with_runtime(|runtime| {
            runtime
                .spawn_staged(config, input_capacity, output_capacity)
                .unwrap_or(0)
        })
        .unwrap_or(0);
        if handle == 0 {
            return 0;
        }
        if let Ok(mut entries) = ports().lock() {
            entries.insert(
                handle,
                Ports {
                    output: output_port,
                    event: event_port,
                },
            );
        }
        handle
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn ptyi_activate(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let activated = with_runtime(|runtime| runtime.activate(handle)).unwrap_or(false);
        if !activated {
            if let Ok(mut entries) = ports().lock() {
                entries.remove(&handle);
            }
        }
        activated
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_write(handle: u64, bytes: *const u8, length: usize) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        if bytes.is_null() || length == 0 {
            return 0;
        }
        let Some(_admission) = InputAdmission::acquire(length) else {
            return 0;
        };
        let bytes = std::slice::from_raw_parts(bytes, length).to_vec();
        with_runtime(|runtime| runtime.try_write(handle, bytes)).unwrap_or(0)
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_pull(handle: u64, target: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if target.is_null() || capacity == 0 {
            return 0;
        }
        let Some(bytes) = with_runtime(|runtime| runtime.pull(handle, capacity)).flatten() else {
            return 0;
        };
        ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        bytes.len()
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_exchange(
    handle: u64,
    credit: usize,
    target: *mut u8,
    capacity: usize,
) -> isize {
    catch_unwind(AssertUnwindSafe(|| {
        if target.is_null() || capacity == 0 {
            return -1;
        }
        let Some(bytes) =
            with_runtime(|runtime| runtime.exchange(handle, credit, capacity)).flatten()
        else {
            return -1;
        };
        ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        bytes.len() as isize
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn ptyi_credit(handle: u64, bytes: usize) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.credit(handle, bytes)).unwrap_or(false)
    }))
    .unwrap_or(false)
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
pub extern "C" fn ptyi_flush_ready(handle: u64, sequence: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.flush_ready(handle, sequence)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_wait_capacity(handle: u64, required: usize, waiter: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        wait_result(with_runtime(|runtime| {
            runtime.wait_capacity(handle, required, waiter)
        }))
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn ptyi_wait_flush(handle: u64, sequence: u64, waiter: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        wait_result(with_runtime(|runtime| {
            runtime.wait_flush(handle, sequence, waiter)
        }))
    }))
    .unwrap_or(0)
}

fn wait_result(result: Option<WaitResult>) -> i32 {
    match result {
        Some(WaitResult::Ready) => 1,
        Some(WaitResult::Armed) => 2,
        Some(WaitResult::Failed) | None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_exit_status(handle: u64, status: *mut i32) -> bool {
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
        with_runtime(|runtime| runtime.pid(handle))
            .flatten()
            .map_or(-1, i64::from)
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
            return false;
        };
        ptr::copy_nonoverlapping(size.as_ptr(), values, size.len());
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
        with_runtime(|runtime| runtime.resize(handle, [rows, columns, pixel_width, pixel_height]))
            .unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_signal(handle: u64, signal: i32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.signal(handle, signal))
            .flatten()
            .map_or(-1, i32::from)
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn ptyi_mode(handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.mode(handle))
            .flatten()
            .map_or(-1, |mode| {
                i32::from(mode[0]) | (i32::from(mode[1]) << 1) | (i32::from(mode[2]) << 2)
            })
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn ptyi_tty_name(handle: u64, target: *mut u8, capacity: usize) -> isize {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(name) = with_runtime(|runtime| runtime.tty_name(handle)).flatten() else {
            return -1;
        };
        if target.is_null() || capacity < name.len() {
            return name.len() as isize;
        }
        ptr::copy_nonoverlapping(name.as_ptr(), target, name.len());
        name.len() as isize
    }))
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn ptyi_output_total(handle: u64) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.output_total(handle))
            .flatten()
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn ptyi_output_done(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.output_done(handle)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_close(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        with_runtime(|runtime| runtime.close(handle)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_destroy(handle: u64) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let removed = with_runtime(|runtime| runtime.destroy(handle)).unwrap_or(false);
        if removed {
            if let Ok(mut entries) = ports().lock() {
                entries.remove(&handle);
            }
        }
        removed
    }))
    .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn ptyi_counter(key: u32) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        let counters = with_runtime(IntegratedRuntime::counters).unwrap_or_default();
        match key {
            0 => counters.reactor_wakeups,
            1 => counters.reactor_events,
            2 => counters.command_wakeups,
            3 => counters.read_syscalls,
            4 => counters.write_syscalls,
            5 => counters.read_bytes,
            6 => counters.write_bytes,
            7 => counters.notifications,
            _ => 0,
        }
    }))
    .unwrap_or(0)
}

fn dispatch_notice(notice: Notice) {
    let broker_lost = matches!(notice, Notice::BrokerLost(_));
    let handle = match &notice {
        Notice::Output { handle, .. }
        | Notice::InputFailed(handle)
        | Notice::BrokerLost(handle)
        | Notice::OutputDone(handle)
        | Notice::Exit(handle) => *handle,
        Notice::Capacity { handle, .. }
        | Notice::Flush { handle, .. }
        | Notice::WaitFailed { handle, .. } => *handle,
    };
    let Some(entry) = ports()
        .lock()
        .ok()
        .and_then(|entries| entries.get(&handle).copied())
    else {
        cleanup_lost(handle);
        return;
    };
    let posted = unsafe {
        match notice {
            Notice::Output { bytes, .. } => {
                ptyx_dart_post_bytes(entry.output, handle as i64, bytes.as_ptr(), bytes.len())
            }
            Notice::Capacity { waiter, .. } => {
                ptyx_dart_post_integer(entry.event, ((waiter << 3) | 1) as i64)
            }
            Notice::Flush { waiter, .. } => {
                ptyx_dart_post_integer(entry.event, ((waiter << 3) | 2) as i64)
            }
            Notice::WaitFailed { waiter, .. } => {
                ptyx_dart_post_integer(entry.event, ((waiter << 3) | 5) as i64)
            }
            Notice::InputFailed(_) => ptyx_dart_post_integer(entry.event, (handle << 3) as i64),
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
            ptyx_dart_post_integer(entry.event, ((handle << 3) | 6) as i64);
        }
        if let Ok(mut entries) = ports().lock() {
            entries.remove(&handle);
        }
        if let Ok(mut lost) = lost_ports().lock() {
            lost.insert(handle);
        }
        let _ = with_runtime(|runtime| runtime.close(handle));
        cleanup_lost(handle);
    }
}

fn cleanup_lost(handle: u64) {
    let is_lost = lost_ports()
        .lock()
        .ok()
        .is_some_and(|lost| lost.contains(&handle));
    if !is_lost {
        return;
    }
    let exited = with_runtime(|runtime| runtime.exit_status(handle).is_some()).unwrap_or(false);
    if exited && with_runtime(|runtime| runtime.destroy(handle)).unwrap_or(false) {
        if let Ok(mut lost) = lost_ports().lock() {
            lost.remove(&handle);
        }
    }
}

unsafe extern "C" {
    fn Dart_InitializeApiDL(data: *mut c_void) -> libc::intptr_t;
    fn ptyx_dart_post_integer(port: i64, message: i64) -> bool;
    fn ptyx_dart_post_bytes(port: i64, handle: i64, bytes: *const u8, length: usize) -> bool;
    fn ptyx_dart_fail_next_post();
}
