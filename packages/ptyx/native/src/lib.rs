//! Native implementation behind the ptyx C ABI.
//!
//! The public contract is documented in `include/ptyx.h`. Rust modules keep
//! the ABI boundary small: exported functions translate raw C values into
//! checked internal operations, and no panic is allowed to cross the boundary.

mod abi;
mod dart_api;
mod error;
mod fd;
mod message;
mod output;
mod owned_buffer;
mod process;
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
    dart_api::init(dart_initialize_api_dl_data)
}

/// Starts a session and returns an owned session handle.
#[no_mangle]
pub extern "C" fn ptyx_spawn(
    options: *const abi::ptyx_session_options_t,
    out_session: *mut *mut session::ptyx_session,
) -> u32 {
    runtime::spawn(options, out_session)
}

/// Copies bytes into the session write queue.
#[no_mangle]
pub extern "C" fn ptyx_write(
    session: *mut session::ptyx_session,
    data: *const u8,
    length: usize,
) -> u32 {
    runtime::write(session, data.cast(), length)
}

/// Allocates a native buffer for zero-copy writes.
#[no_mangle]
pub extern "C" fn ptyx_buffer_alloc(
    capacity: usize,
    out_buffer: *mut *mut owned_buffer::ptyx_owned_buffer,
) -> u32 {
    error::ffi_status(|| owned_buffer::alloc(capacity, out_buffer))
}

/// Returns the writable data pointer for an owned buffer.
#[no_mangle]
pub extern "C" fn ptyx_buffer_data(buffer: *mut owned_buffer::ptyx_owned_buffer) -> *mut u8 {
    owned_buffer::data(buffer)
}

/// Releases an owned buffer that was not transferred to a session.
#[no_mangle]
pub extern "C" fn ptyx_buffer_free(buffer: *mut owned_buffer::ptyx_owned_buffer) {
    owned_buffer::free(buffer);
}

/// Transfers an owned buffer into the session write queue.
#[no_mangle]
pub extern "C" fn ptyx_write_owned(
    session: *mut session::ptyx_session,
    buffer: *mut owned_buffer::ptyx_owned_buffer,
    length: usize,
) -> u32 {
    runtime::write_owned(session, buffer, length)
}

/// Acknowledges output bytes delivered to the receiver.
#[no_mangle]
pub extern "C" fn ptyx_ack_output(session: *mut session::ptyx_session, byte_count: u64) -> u32 {
    runtime::ack_output(session, byte_count)
}

/// Changes the pseudo terminal size.
#[no_mangle]
pub extern "C" fn ptyx_resize(session: *mut session::ptyx_session, size: abi::ptyx_size_t) -> u32 {
    session::resize(session, size)
}

/// Reads the current pseudo terminal size.
#[no_mangle]
pub extern "C" fn ptyx_get_size(
    session: *mut session::ptyx_session,
    out_size: *mut abi::ptyx_size_t,
) -> u32 {
    session::get_size(session, out_size)
}

/// Reads the current terminal mode snapshot.
#[no_mangle]
pub extern "C" fn ptyx_get_term_mode(
    session: *mut session::ptyx_session,
    out_mode: *mut abi::ptyx_term_mode_t,
) -> u32 {
    term_mode::get_term_mode(session, out_mode)
}

/// Reads the child process identifier.
#[no_mangle]
pub extern "C" fn ptyx_get_child_pid(
    session: *mut session::ptyx_session,
    out_pid: *mut u64,
) -> u32 {
    session::get_child_pid(session, out_pid)
}

/// Reads the pseudo terminal device name into a caller buffer.
#[no_mangle]
pub extern "C" fn ptyx_get_tty_name(
    session: *mut session::ptyx_session,
    buffer: *mut c_char,
    inout_len: *mut usize,
) -> u32 {
    session::get_tty_name(session, buffer, inout_len)
}

/// Sends a signal or native termination request to the child.
#[no_mangle]
pub extern "C" fn ptyx_kill(session: *mut session::ptyx_session, signal: i32) -> bool {
    session::kill(session, signal)
}

/// Closes a session handle and releases native resources.
#[no_mangle]
pub extern "C" fn ptyx_close(session: *mut session::ptyx_session) {
    session::free(session);
}

/// Returns the last error message for the current thread.
#[no_mangle]
pub extern "C" fn ptyx_last_error_message() -> *const c_char {
    error::last_error_message()
}
