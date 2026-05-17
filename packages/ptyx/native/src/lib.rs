//! Native implementation behind the ptyx C ABI.
//!
//! The public contract is documented in `include/ptyx.h`. Rust modules keep
//! the ABI boundary small: exported functions translate raw C values into
//! checked internal operations, and no panic is allowed to cross the boundary.

mod abi;
mod config;
mod dart_api;
mod error;
mod fd;
mod message;
mod output;
mod owned_buffer;
mod process;
#[cfg(target_os = "linux")]
mod process_linux;
mod reader;
mod runtime;
mod session;
mod term_mode;
mod writer;

use std::os::raw::{c_char, c_void};

/// Returns the ABI major version.
#[no_mangle]
pub extern "C" fn ptyx_abi_version_major() -> u32 {
    abi::PTYX_ABI_VERSION_MAJOR
}

/// Returns the ABI minor version.
#[no_mangle]
pub extern "C" fn ptyx_abi_version_minor() -> u32 {
    abi::PTYX_ABI_VERSION_MINOR
}

/// Returns a static name for a status code.
#[no_mangle]
pub extern "C" fn ptyx_status_string(status: u32) -> *const c_char {
    abi::status_string(status)
}

/// Writes default session options into caller-owned storage.
#[no_mangle]
pub extern "C" fn ptyx_session_options_init(options: *mut abi::ptyx_session_options_t) {
    abi::session_options_init(options);
}

/// Initializes Dart native API access for this library.
#[no_mangle]
pub extern "C" fn ptyx_init(dart_initialize_api_dl_data: *mut c_void) -> u32 {
    abi::ffi_status(|| init_dart_api(dart_initialize_api_dl_data))
}

/// Starts a session and returns an owned session handle.
#[no_mangle]
pub extern "C" fn ptyx_spawn(
    options: *const abi::ptyx_session_options_t,
    out_session: *mut *mut abi::ptyx_session,
) -> u32 {
    abi::ffi_status(|| {
        if !dart_api::is_initialized() {
            return Err(error::PtyxError::new(
                error::PtyxErrorKind::InvalidArgument,
                "ptyx_init must be called before ptyx_spawn",
            ));
        }
        let options = abi::session_options_from_ptr(options)?;
        abi::prepare_session_out(out_session)?;
        let session = runtime::spawn(options)?;
        abi::write_session_out(out_session, session);
        Ok(())
    })
}

/// Copies bytes into the session write queue.
#[no_mangle]
pub extern "C" fn ptyx_write(
    session: *mut abi::ptyx_session,
    data: *const u8,
    length: usize,
) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let bytes = abi::bytes_from_ptr(data, length)?;
        runtime::write(session, bytes)
    })
}

/// Allocates a native buffer for zero-copy writes.
#[no_mangle]
pub extern "C" fn ptyx_buffer_alloc(
    capacity: usize,
    out_buffer: *mut *mut abi::ptyx_owned_buffer,
) -> u32 {
    abi::ffi_status(|| abi::alloc_owned_buffer(capacity, out_buffer))
}

/// Returns the writable data pointer for an owned buffer.
#[no_mangle]
pub extern "C" fn ptyx_buffer_data(buffer: *mut abi::ptyx_owned_buffer) -> *mut u8 {
    abi::owned_buffer_data(buffer)
}

/// Releases an owned buffer that was not transferred to a session.
#[no_mangle]
pub extern "C" fn ptyx_buffer_free(buffer: *mut abi::ptyx_owned_buffer) {
    abi::free_owned_buffer(buffer);
}

/// Transfers an owned buffer into the session write queue.
#[no_mangle]
pub extern "C" fn ptyx_write_owned(
    session: *mut abi::ptyx_session,
    buffer: *mut abi::ptyx_owned_buffer,
    length: usize,
) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let mut buffer = abi::take_owned_buffer(buffer, length)?;
        let bytes = abi::take_owned_buffer_bytes(&mut buffer);
        match runtime::write_owned(session, bytes) {
            Ok(()) => Ok(()),
            Err((error, bytes)) => {
                abi::return_owned_buffer(buffer, bytes);
                Err(error)
            }
        }
    })
}

/// Acknowledges output bytes delivered to the receiver.
#[no_mangle]
pub extern "C" fn ptyx_ack_output(session: *mut abi::ptyx_session, byte_count: u64) -> u32 {
    abi::ffi_status(|| runtime::ack_output(abi::session_from_ptr(session)?, byte_count))
}

/// Changes the pseudo terminal size.
#[no_mangle]
pub extern "C" fn ptyx_resize(session: *mut abi::ptyx_session, size: abi::ptyx_size_t) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let size = abi::to_pty_size(size)?;
        session::resize(session, size)
    })
}

/// Reads the current pseudo terminal size.
#[no_mangle]
pub extern "C" fn ptyx_get_size(
    session: *mut abi::ptyx_session,
    out_size: *mut abi::ptyx_size_t,
) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let size = session::get_size(session)?;
        abi::write_size(out_size, size)
    })
}

/// Reads the current terminal mode snapshot.
#[no_mangle]
pub extern "C" fn ptyx_get_term_mode(
    session: *mut abi::ptyx_session,
    out_mode: *mut abi::ptyx_term_mode_t,
) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let mode = term_mode::get_term_mode(session)?;
        abi::write_term_mode(out_mode, mode)
    })
}

/// Reads the child process identifier.
#[no_mangle]
pub extern "C" fn ptyx_get_child_pid(session: *mut abi::ptyx_session, out_pid: *mut u64) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let pid = session::get_child_pid(session)?;
        abi::write_u64(out_pid, pid, "out_pid")
    })
}

/// Reads the pseudo terminal device name into a caller buffer.
#[no_mangle]
pub extern "C" fn ptyx_get_tty_name(
    session: *mut abi::ptyx_session,
    buffer: *mut c_char,
    inout_len: *mut usize,
) -> u32 {
    abi::ffi_status(|| {
        let session = abi::session_from_ptr(session)?;
        let tty_name = session::get_tty_name(session)?;
        abi::fill_string(tty_name, buffer, inout_len)
    })
}

/// Sends a signal or native termination request to the child.
#[no_mangle]
pub extern "C" fn ptyx_kill(session: *mut abi::ptyx_session, signal: i32) -> bool {
    let Ok(session) = abi::session_from_ptr(session) else {
        return false;
    };
    session::kill(session, signal)
}

/// Closes a session handle and releases native resources.
#[no_mangle]
pub extern "C" fn ptyx_close(session: *mut abi::ptyx_session) {
    abi::free_session(session);
}

/// Returns the last error message for the current thread.
#[no_mangle]
pub extern "C" fn ptyx_last_error_message() -> *const c_char {
    abi::last_error_message()
}

fn init_dart_api(dart_initialize_api_dl_data: *mut c_void) -> Result<(), error::PtyxError> {
    dart_api::init(dart_initialize_api_dl_data).map_err(|error| {
        let kind = match error {
            dart_api::DartApiError::NullInitializeData => error::PtyxErrorKind::InvalidArgument,
            dart_api::DartApiError::InitializeFailed(_) => error::PtyxErrorKind::NativeError,
        };
        error::PtyxError::new(kind, error.to_string())
    })
}
