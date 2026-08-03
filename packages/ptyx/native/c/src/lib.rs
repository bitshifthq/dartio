#![cfg(any(target_os = "linux", target_os = "macos", windows))]

mod c_api;

/// Private Rust composition surface for product adapter dynamic libraries.
///
/// This is not a supported application API. Every operation is the same
/// exported C ABI function and uses the same C layout as `ptyx.h`.
#[doc(hidden)]
pub mod private {
    pub use crate::c_api::{
        clear_error, error_is_valid, ptyx_event_release, ptyx_runtime_capabilities,
        ptyx_runtime_create, ptyx_runtime_next_event, ptyx_runtime_release, ptyx_runtime_shutdown,
        ptyx_session_get_child_pid, ptyx_session_get_size, ptyx_session_get_term_mode,
        ptyx_session_get_tty_name, ptyx_session_release, ptyx_session_spawn_start, set_error,
        Error, Event, Registry, SpawnOptions, ERROR_DOMAIN_ARGUMENT, ERROR_DOMAIN_PROCESS,
        ERROR_DOMAIN_RUNTIME, ERROR_DOMAIN_STATE, ERROR_INFRASTRUCTURE_LOST,
        ERROR_INVALID_ARGUMENT, ERROR_NATIVE_FAILURE, ERROR_STALE_HANDLE, ERROR_WRONG_STATE,
        OPERATION_CLOSE, OPERATION_EXIT, OPERATION_OUTPUT, OPERATION_RUNTIME_CREATE,
        OPERATION_RUNTIME_SHUTDOWN, STATUS_INTERNAL, STATUS_INVALID_ARGUMENT, STATUS_OK,
        STATUS_STALE_HANDLE, STATUS_WRONG_STATE,
    };
    #[cfg(feature = "test-controls")]
    pub use crate::c_api::{
        test_delay_next_spawn, test_fail_next_write, test_kill_broker, test_spawn_delay_active,
    };
}

/// Retains the complete stable C ABI when this crate is linked into a product
/// adapter dynamic library.
#[doc(hidden)]
pub fn linked_symbols() -> [usize; 20] {
    [
        c_api::ptyx_abi_version as *const () as usize,
        c_api::ptyx_error_format as *const () as usize,
        c_api::ptyx_event_release as *const () as usize,
        c_api::ptyx_runtime_capabilities as *const () as usize,
        c_api::ptyx_runtime_create as *const () as usize,
        c_api::ptyx_runtime_next_event as *const () as usize,
        c_api::ptyx_runtime_release as *const () as usize,
        c_api::ptyx_runtime_shutdown as *const () as usize,
        c_api::ptyx_session_cancel_output as *const () as usize,
        c_api::ptyx_session_close as *const () as usize,
        c_api::ptyx_session_observe_mode as *const () as usize,
        c_api::ptyx_session_release as *const () as usize,
        c_api::ptyx_session_resize as *const () as usize,
        c_api::ptyx_session_get_child_pid as *const () as usize,
        c_api::ptyx_session_get_size as *const () as usize,
        c_api::ptyx_session_get_term_mode as *const () as usize,
        c_api::ptyx_session_get_tty_name as *const () as usize,
        c_api::ptyx_session_spawn_start as *const () as usize,
        c_api::ptyx_session_terminate as *const () as usize,
        c_api::ptyx_session_write as *const () as usize,
    ]
}
