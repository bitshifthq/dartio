//! C ABI values and conversion helpers.
//!
//! The structs in this module mirror `include/ptyx.h`. They stay plain and
//! copyable so callers can initialize them on the C side and pass them across
//! FFI without Rust ownership.

#![allow(non_camel_case_types, reason = "C ABI type names mirror ptyx headers.")]

use portable_pty::PtySize;
use std::ffi::{CStr, OsString};
use std::os::raw::c_char;
use std::ptr;

use crate::error::PtyxError;

/// ABI-breaking version.
pub(crate) const PTYX_ABI_VERSION_MAJOR: u32 = 11;
/// ABI-compatible feature version.
pub(crate) const PTYX_ABI_VERSION_MINOR: u32 = 0;

pub(crate) const KIB: usize = 1024;
pub(crate) const MIB: usize = KIB * KIB;
pub(crate) const DEFAULT_INITIAL_ROWS: u32 = 24;
pub(crate) const DEFAULT_INITIAL_COLUMNS: u32 = 80;
pub(crate) const DEFAULT_READ_BUFFER_SIZE: u32 = (256 * KIB) as u32;
pub(crate) const DEFAULT_OUTPUT_BATCH_MAX_BYTES: u32 = (128 * KIB) as u32;
pub(crate) const DEFAULT_OUTPUT_BATCH_DELAY_US: u32 = 1_000;
pub(crate) const DEFAULT_MODE_POLL_INTERVAL_MS: u32 = 50;
pub(crate) const DEFAULT_MAX_INFLIGHT_BYTES: u64 = (4 * MIB) as u64;
pub(crate) const DEFAULT_MAX_EXTERNAL_OUTPUT_BYTES: u64 = (64 * MIB) as u64;
pub(crate) const DEFAULT_WRITE_QUEUE_MAX_BYTES: usize = 64 * MIB;

pub(crate) const PTYX_STATUS_OK: u32 = 0;
pub(crate) const PTYX_STATUS_ERROR: u32 = 1;
pub(crate) const PTYX_STATUS_INVALID_ARGUMENT: u32 = 2;
pub(crate) const PTYX_STATUS_UNSUPPORTED: u32 = 3;
pub(crate) const PTYX_STATUS_OUT_OF_MEMORY: u32 = 4;
pub(crate) const PTYX_STATUS_CLOSED: u32 = 6;
pub(crate) const PTYX_STATUS_WOULD_BLOCK: u32 = 7;
pub(crate) const PTYX_STATUS_BUFFER_TOO_SMALL: u32 = 8;
pub(crate) const PTYX_STATUS_SPAWN_FAILED: u32 = 9;
pub(crate) const PTYX_STATUS_IO_FAILED: u32 = 10;
pub(crate) const PTYX_STATUS_WAIT_FAILED: u32 = 11;
pub(crate) const PTYX_STATUS_TIMEOUT: u32 = 12;
pub(crate) const PTYX_STATUS_EOF: u32 = 14;
pub(crate) const PTYX_STATUS_BROKEN_PIPE: u32 = 15;
pub(crate) const PTYX_STATUS_PERMISSION_DENIED: u32 = 16;
pub(crate) const PTYX_STATUS_BUSY: u32 = 18;
pub(crate) const PTYX_STATUS_NATIVE_ERROR: u32 = 19;

pub(crate) const PTYX_ENV_INHERIT: u32 = 0;
pub(crate) const PTYX_ENV_OVERLAY: u32 = 1;
pub(crate) const PTYX_ENV_REPLACE: u32 = 2;
pub(crate) const PTYX_ENV_CLEAR: u32 = 3;

pub(crate) const PTYX_TERM_MODE_CANONICAL_VALID: u32 = 1 << 0;
pub(crate) const PTYX_TERM_MODE_ECHO_VALID: u32 = 1 << 1;
pub(crate) const PTYX_TERM_MODE_SIGNALS_VALID: u32 = 1 << 2;

pub(crate) const PTYX_MESSAGE_OUTPUT: i64 = 1;
pub(crate) const PTYX_MESSAGE_CLOSED: i64 = 2;

pub(crate) const PTYX_EVENT_EXIT: i64 = 1;
pub(crate) const PTYX_EVENT_ERROR: i64 = 2;
pub(crate) const PTYX_EVENT_TERM_MODE: i64 = 3;

pub(crate) const PTYX_ERROR_SOURCE_OUTPUT: i64 = 1;
pub(crate) const PTYX_ERROR_SOURCE_WRITE: i64 = 2;
pub(crate) const PTYX_ERROR_SOURCE_WAIT: i64 = 3;
pub(crate) const PTYX_ERROR_SOURCE_MODE: i64 = 4;

pub(crate) const PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA: u32 = 1 << 0;
pub(crate) const PTYX_SESSION_ENABLE_MODE_EVENTS: u32 = 1 << 1;
pub(crate) const PTYX_SESSION_REQUIRE_OUTPUT_ACKS: u32 = 1 << 2;
pub(crate) const PTYX_SESSION_SUPPORTED_FLAGS: u32 = PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA
    | PTYX_SESSION_ENABLE_MODE_EVENTS
    | PTYX_SESSION_REQUIRE_OUTPUT_ACKS;

/// Borrowed UTF-8 string passed through the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ptyx_string_t {
    pub data: *const c_char,
    pub len: usize,
}

/// Terminal size as exposed through the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ptyx_size_t {
    pub rows: u32,
    pub columns: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Terminal mode snapshot with per-field validity bits.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ptyx_term_mode_t {
    pub valid_fields: u32,
    pub canonical: bool,
    pub echo: bool,
    pub signals: bool,
}

/// Session spawn options passed through the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ptyx_session_options_t {
    pub flags: u32,
    pub executable: ptyx_string_t,
    pub argv: *const ptyx_string_t,
    pub argc: usize,
    pub env_items: *const ptyx_string_t,
    pub env_count: usize,
    pub env_mode: u32,
    pub cwd: ptyx_string_t,
    pub initial_size: ptyx_size_t,
    pub output_port: i64,
    pub event_port: i64,
    pub read_buffer_size: u32,
    pub output_batch_max_bytes: u32,
    pub output_batch_max_delay_us: u32,
    pub mode_poll_interval_ms: u32,
    pub max_inflight_bytes: u64,
    pub max_external_output_bytes: u64,
    pub write_queue_max_bytes: u64,
}

pub(crate) fn status_string(status: u32) -> *const c_char {
    match status {
        PTYX_STATUS_OK => c"OK".as_ptr(),
        PTYX_STATUS_ERROR => c"ERROR".as_ptr(),
        PTYX_STATUS_INVALID_ARGUMENT => c"INVALID_ARGUMENT".as_ptr(),
        PTYX_STATUS_UNSUPPORTED => c"UNSUPPORTED".as_ptr(),
        PTYX_STATUS_OUT_OF_MEMORY => c"OUT_OF_MEMORY".as_ptr(),
        PTYX_STATUS_CLOSED => c"CLOSED".as_ptr(),
        PTYX_STATUS_WOULD_BLOCK => c"WOULD_BLOCK".as_ptr(),
        PTYX_STATUS_BUFFER_TOO_SMALL => c"BUFFER_TOO_SMALL".as_ptr(),
        PTYX_STATUS_SPAWN_FAILED => c"SPAWN_FAILED".as_ptr(),
        PTYX_STATUS_IO_FAILED => c"IO_FAILED".as_ptr(),
        PTYX_STATUS_WAIT_FAILED => c"WAIT_FAILED".as_ptr(),
        PTYX_STATUS_TIMEOUT => c"TIMEOUT".as_ptr(),
        PTYX_STATUS_EOF => c"EOF".as_ptr(),
        PTYX_STATUS_BROKEN_PIPE => c"BROKEN_PIPE".as_ptr(),
        PTYX_STATUS_PERMISSION_DENIED => c"PERMISSION_DENIED".as_ptr(),
        PTYX_STATUS_BUSY => c"BUSY".as_ptr(),
        PTYX_STATUS_NATIVE_ERROR => c"NATIVE_ERROR".as_ptr(),
        _ => c"UNKNOWN".as_ptr(),
    }
}

pub(crate) fn session_options_init(options: *mut ptyx_session_options_t) {
    if !options.is_null() {
        // `options` is non-null and points to writable caller-owned storage.
        unsafe { options.write(default_session_options()) };
    }
}

pub(crate) fn session_options_from_ptr(
    ptr: *const ptyx_session_options_t,
) -> Result<ptyx_session_options_t, PtyxError> {
    if ptr.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "session options must not be null",
        ));
    }
    // `ptr` is non-null and points to a copyable C options struct.
    Ok(unsafe { *ptr })
}

pub(crate) fn string_to_string(value: ptyx_string_t) -> Result<String, PtyxError> {
    if value.len == 0 {
        return Ok(String::new());
    }
    if value.data.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "string data must not be null when length is non-zero",
        ));
    }
    // `data` is non-null for non-empty strings and valid for `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(value.data.cast::<u8>(), value.len) };
    String::from_utf8(bytes.to_vec()).map_err(|e| {
        PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            format!("string is not valid UTF-8: {e}"),
        )
    })
}

pub(crate) fn string_to_os_string(value: ptyx_string_t) -> Result<OsString, PtyxError> {
    Ok(OsString::from(string_to_string(value)?))
}

pub(crate) fn string_array(
    ptr: *const ptyx_string_t,
    count: usize,
) -> Result<Vec<OsString>, PtyxError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "string array pointer must not be null when count is non-zero",
        ));
    }
    // `ptr` is non-null when `count` is non-zero and points to `count` items.
    let items = unsafe { std::slice::from_raw_parts(ptr, count) };
    items.iter().copied().map(string_to_os_string).collect()
}

pub(crate) fn to_pty_size(size: ptyx_size_t) -> Result<PtySize, PtyxError> {
    if size.rows == 0 || size.columns == 0 {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "rows and columns must be positive",
        ));
    }
    Ok(PtySize {
        rows: u16::try_from(size.rows)
            .map_err(|_| PtyxError::new(PTYX_STATUS_INVALID_ARGUMENT, "rows exceed u16"))?,
        cols: u16::try_from(size.columns)
            .map_err(|_| PtyxError::new(PTYX_STATUS_INVALID_ARGUMENT, "columns exceed u16"))?,
        pixel_width: u16::try_from(size.pixel_width)
            .map_err(|_| PtyxError::new(PTYX_STATUS_INVALID_ARGUMENT, "pixel width exceeds u16"))?,
        pixel_height: u16::try_from(size.pixel_height).map_err(|_| {
            PtyxError::new(PTYX_STATUS_INVALID_ARGUMENT, "pixel height exceeds u16")
        })?,
    })
}

pub(crate) fn fill_string(
    value: &CStr,
    buffer: *mut c_char,
    inout_len: *mut usize,
) -> Result<u32, PtyxError> {
    let bytes = value.to_bytes();
    // Callers pass a valid in/out length pointer. The required length is
    // reported even when the destination buffer is too small.
    let capacity = unsafe { *inout_len };
    unsafe { *inout_len = bytes.len() };
    if buffer.is_null() || capacity <= bytes.len() {
        return Err(PtyxError::new(
            PTYX_STATUS_BUFFER_TOO_SMALL,
            "buffer is too small",
        ));
    }
    unsafe {
        // The capacity check above leaves room for the trailing NUL byte.
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    Ok(PTYX_STATUS_OK)
}

pub(crate) fn default_string() -> ptyx_string_t {
    ptyx_string_t {
        data: ptr::null(),
        len: 0,
    }
}

pub(crate) fn default_session_options() -> ptyx_session_options_t {
    ptyx_session_options_t {
        flags: PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA
            | PTYX_SESSION_ENABLE_MODE_EVENTS
            | PTYX_SESSION_REQUIRE_OUTPUT_ACKS,
        executable: default_string(),
        argv: ptr::null(),
        argc: 0,
        env_items: ptr::null(),
        env_count: 0,
        env_mode: PTYX_ENV_OVERLAY,
        cwd: default_string(),
        initial_size: ptyx_size_t {
            rows: DEFAULT_INITIAL_ROWS,
            columns: DEFAULT_INITIAL_COLUMNS,
            pixel_width: 0,
            pixel_height: 0,
        },
        output_port: 0,
        event_port: 0,
        read_buffer_size: DEFAULT_READ_BUFFER_SIZE,
        output_batch_max_bytes: DEFAULT_OUTPUT_BATCH_MAX_BYTES,
        output_batch_max_delay_us: DEFAULT_OUTPUT_BATCH_DELAY_US,
        mode_poll_interval_ms: DEFAULT_MODE_POLL_INTERVAL_MS,
        max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
        max_external_output_bytes: DEFAULT_MAX_EXTERNAL_OUTPUT_BYTES,
        write_queue_max_bytes: DEFAULT_WRITE_QUEUE_MAX_BYTES as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_helpers_set_defaults() {
        let mut options = default_session_options();
        session_options_init(&mut options);
        assert_eq!(options.initial_size.rows, DEFAULT_INITIAL_ROWS);
        assert_eq!(options.initial_size.columns, DEFAULT_INITIAL_COLUMNS);
        assert_eq!(
            options.flags,
            PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA
                | PTYX_SESSION_ENABLE_MODE_EVENTS
                | PTYX_SESSION_REQUIRE_OUTPUT_ACKS
        );
    }

    #[test]
    fn rejects_invalid_string_pointer() {
        let value = ptyx_string_t {
            data: ptr::null(),
            len: 1,
        };
        assert!(string_to_string(value).is_err());
    }

    #[test]
    fn validates_size_ranges() {
        assert!(to_pty_size(ptyx_size_t {
            rows: 24,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .is_ok());
        assert!(to_pty_size(ptyx_size_t {
            rows: 0,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .is_err());
        assert!(to_pty_size(ptyx_size_t {
            rows: u16::MAX as u32 + 1,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .is_err());
    }
}
