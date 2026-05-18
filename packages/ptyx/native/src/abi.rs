//! C ABI values and conversion helpers.
//!
//! The structs in this module mirror `include/ptyx.h`. They stay plain and
//! copyable so callers can initialize them on the C side and pass them across
//! FFI without Rust ownership.

#![allow(non_camel_case_types, reason = "C ABI type names mirror ptyx headers.")]

use portable_pty::PtySize;
use std::cell::RefCell;
use std::ffi::{CStr, CString, OsString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::time::Duration;

use crate::config::{EnvironmentMode, RuntimeConfig, SessionOptions, SpawnConfig};
use crate::error::{PtyxError, PtyxErrorKind};
use crate::output::OutputConfig;
use crate::owned_buffer::{self, OwnedBuffer};
use crate::session::{self, Session};
use crate::term_mode::TermMode;

/// ABI-breaking version.
pub(crate) const PTYX_ABI_VERSION_MAJOR: u32 = 1;
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
#[cfg(unix)]
pub(crate) const PTYX_EVENT_TERM_MODE: i64 = 3;

pub(crate) const PTYX_ERROR_SOURCE_OUTPUT: i64 = 1;
pub(crate) const PTYX_ERROR_SOURCE_WRITE: i64 = 2;
pub(crate) const PTYX_ERROR_SOURCE_WAIT: i64 = 3;
#[cfg(unix)]
pub(crate) const PTYX_ERROR_SOURCE_MODE: i64 = 4;

pub(crate) const PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA: u32 = 1 << 0;
pub(crate) const PTYX_SESSION_ENABLE_MODE_EVENTS: u32 = 1 << 1;
pub(crate) const PTYX_SESSION_REQUIRE_OUTPUT_ACKS: u32 = 1 << 2;
pub(crate) const PTYX_SESSION_SUPPORTED_FLAGS: u32 = PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA
    | PTYX_SESSION_ENABLE_MODE_EVENTS
    | PTYX_SESSION_REQUIRE_OUTPUT_ACKS;

const MIN_OUTPUT_BATCH_DELAY_US: u32 = 1;
const MAX_OUTPUT_BATCH_DELAY_US: u32 = 1_000_000;
const MIN_MODE_POLL_INTERVAL_MS: u32 = 25;
const MAX_MODE_POLL_INTERVAL_MS: u32 = 60_000;
const MIN_READ_BUFFER_SIZE: u32 = (4 * KIB) as u32;
const MAX_READ_BUFFER_SIZE: u32 = MIB as u32;
const MIN_OUTPUT_BATCH_MAX_BYTES: u32 = (64 * KIB) as u32;
const MAX_OUTPUT_BATCH_MAX_BYTES: u32 = (256 * KIB) as u32;
const MIN_WRITE_QUEUE_MAX_BYTES: usize = 64 * KIB;
const MAX_WRITE_QUEUE_MAX_BYTES: usize = 512 * MIB;

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

/// Opaque PTY session handle passed through the C ABI.
pub struct ptyx_session {
    session: Box<Session>,
}

/// Opaque caller-filled buffer passed through the C ABI.
pub struct ptyx_owned_buffer {
    buffer: OwnedBuffer,
}

impl From<TermMode> for ptyx_term_mode_t {
    fn from(mode: TermMode) -> Self {
        Self {
            valid_fields: term_mode_valid_fields(mode),
            canonical: mode.canonical.unwrap_or(false),
            echo: mode.echo.unwrap_or(false),
            signals: mode.signals.unwrap_or(false),
        }
    }
}

impl From<PtySize> for ptyx_size_t {
    fn from(size: PtySize) -> Self {
        Self {
            rows: size.rows as u32,
            columns: size.cols as u32,
            pixel_width: size.pixel_width as u32,
            pixel_height: size.pixel_height as u32,
        }
    }
}

fn term_mode_valid_fields(mode: TermMode) -> u32 {
    let mut fields = 0;
    if mode.canonical.is_some() {
        fields |= PTYX_TERM_MODE_CANONICAL_VALID;
    }
    if mode.echo.is_some() {
        fields |= PTYX_TERM_MODE_ECHO_VALID;
    }
    if mode.signals.is_some() {
        fields |= PTYX_TERM_MODE_SIGNALS_VALID;
    }
    fields
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

pub(crate) fn status_for_error_kind(kind: PtyxErrorKind) -> u32 {
    match kind {
        PtyxErrorKind::Error => PTYX_STATUS_ERROR,
        PtyxErrorKind::InvalidArgument => PTYX_STATUS_INVALID_ARGUMENT,
        PtyxErrorKind::Unsupported => PTYX_STATUS_UNSUPPORTED,
        PtyxErrorKind::OutOfMemory => PTYX_STATUS_OUT_OF_MEMORY,
        PtyxErrorKind::Closed => PTYX_STATUS_CLOSED,
        PtyxErrorKind::WouldBlock => PTYX_STATUS_WOULD_BLOCK,
        PtyxErrorKind::BufferTooSmall => PTYX_STATUS_BUFFER_TOO_SMALL,
        PtyxErrorKind::SpawnFailed => PTYX_STATUS_SPAWN_FAILED,
        PtyxErrorKind::IoFailed => PTYX_STATUS_IO_FAILED,
        PtyxErrorKind::WaitFailed => PTYX_STATUS_WAIT_FAILED,
        PtyxErrorKind::BrokenPipe => PTYX_STATUS_BROKEN_PIPE,
        PtyxErrorKind::PermissionDenied => PTYX_STATUS_PERMISSION_DENIED,
        PtyxErrorKind::Busy => PTYX_STATUS_BUSY,
        PtyxErrorKind::NativeError => PTYX_STATUS_NATIVE_ERROR,
    }
}

#[derive(Clone)]
struct LastError {
    message: CString,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<LastError>> = const { RefCell::new(None) };
}

pub(crate) fn ffi_status<F>(f: F) -> u32
where
    F: FnOnce() -> Result<(), PtyxError>,
{
    // No unwind may cross the C ABI. Convert panics into the same status and
    // last-error channel as ordinary native failures.
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => {
            clear_last_error();
            PTYX_STATUS_OK
        }
        Ok(Err(error)) => {
            let status = status_for_error_kind(error.kind);
            set_last_error(error);
            status
        }
        Err(_) => {
            set_last_error(PtyxError::new(
                PtyxErrorKind::NativeError,
                "panic crossed native boundary",
            ));
            PTYX_STATUS_NATIVE_ERROR
        }
    }
}

pub(crate) fn last_error_message() -> *const c_char {
    LAST_ERROR.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|e| e.message.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

fn set_last_error(error: PtyxError) {
    let message =
        CString::new(error.message).unwrap_or_else(|_| CString::new("ptyx error").unwrap());
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(LastError { message });
    });
}

fn clear_last_error() {
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub(crate) fn prepare_session_out(out_session: *mut *mut ptyx_session) -> Result<(), PtyxError> {
    if out_session.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "out_session must not be null",
        ));
    }
    unsafe {
        *out_session = ptr::null_mut();
    }
    Ok(())
}

pub(crate) fn write_session_out(out_session: *mut *mut ptyx_session, session: Box<Session>) {
    unsafe {
        *out_session = Box::into_raw(Box::new(ptyx_session { session }));
    }
}

pub(crate) fn session_from_ptr<'a>(ptr: *mut ptyx_session) -> Result<&'a Session, PtyxError> {
    if ptr.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "session must not be null",
        ));
    }
    Ok(unsafe { (*ptr).session.as_ref() })
}

pub(crate) fn free_session(ptr: *mut ptyx_session) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let session = Box::from_raw(ptr);
        session::close(session.session.as_ref());
    }
}

pub(crate) fn alloc_owned_buffer(
    capacity: usize,
    out_buffer: *mut *mut ptyx_owned_buffer,
) -> Result<(), PtyxError> {
    if out_buffer.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "out_buffer must not be null",
        ));
    }

    let buffer = owned_buffer::alloc(capacity)?;
    unsafe {
        *out_buffer = Box::into_raw(Box::new(ptyx_owned_buffer { buffer }));
    }
    Ok(())
}

pub(crate) fn owned_buffer_data(buffer: *mut ptyx_owned_buffer) -> *mut u8 {
    if buffer.is_null() {
        return ptr::null_mut();
    }
    unsafe { (*buffer).buffer.bytes.as_mut_ptr() }
}

pub(crate) fn free_owned_buffer(buffer: *mut ptyx_owned_buffer) {
    if buffer.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(buffer));
    }
}

pub(crate) fn take_owned_buffer(
    buffer: *mut ptyx_owned_buffer,
    length: usize,
) -> Result<Box<ptyx_owned_buffer>, PtyxError> {
    if buffer.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "buffer must not be null",
        ));
    }

    let mut buffer = unsafe { Box::from_raw(buffer) };
    if let Err(error) = owned_buffer::truncate(&mut buffer.buffer, length) {
        let _ = Box::into_raw(buffer);
        return Err(error);
    }
    Ok(buffer)
}

pub(crate) fn take_owned_buffer_bytes(buffer: &mut ptyx_owned_buffer) -> Vec<u8> {
    std::mem::take(&mut buffer.buffer.bytes)
}

pub(crate) fn return_owned_buffer(mut buffer: Box<ptyx_owned_buffer>, bytes: Vec<u8>) {
    buffer.buffer.bytes = bytes;
    let _ = Box::into_raw(buffer);
}

pub(crate) fn bytes_from_ptr<'a>(data: *const u8, length: usize) -> Result<&'a [u8], PtyxError> {
    if length == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "data must not be null when length is non-zero",
        ));
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}

pub(crate) fn write_size(out_size: *mut ptyx_size_t, size: PtySize) -> Result<(), PtyxError> {
    if out_size.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "out_size must not be null",
        ));
    }
    unsafe {
        *out_size = size.into();
    }
    Ok(())
}

pub(crate) fn write_u64(out_value: *mut u64, value: u64, name: &str) -> Result<(), PtyxError> {
    if out_value.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            format!("{name} must not be null"),
        ));
    }
    unsafe {
        *out_value = value;
    }
    Ok(())
}

pub(crate) fn write_term_mode(
    out_mode: *mut ptyx_term_mode_t,
    mode: TermMode,
) -> Result<(), PtyxError> {
    if out_mode.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "out_mode must not be null",
        ));
    }
    unsafe {
        *out_mode = mode.into();
    }
    Ok(())
}

pub(crate) fn session_options_init(options: *mut ptyx_session_options_t) {
    if !options.is_null() {
        // `options` is non-null and points to writable caller-owned storage.
        unsafe { options.write(default_session_options()) };
    }
}

pub(crate) fn session_options_from_ptr(
    ptr: *const ptyx_session_options_t,
) -> Result<SessionOptions, PtyxError> {
    session_options_from_abi(copy_session_options_from_ptr(ptr)?)
}

fn copy_session_options_from_ptr(
    ptr: *const ptyx_session_options_t,
) -> Result<ptyx_session_options_t, PtyxError> {
    if ptr.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "session options must not be null",
        ));
    }
    // `ptr` is non-null and points to a copyable C options struct.
    Ok(unsafe { *ptr })
}

fn session_options_from_abi(options: ptyx_session_options_t) -> Result<SessionOptions, PtyxError> {
    if options.flags & !PTYX_SESSION_SUPPORTED_FLAGS != 0 {
        return Err(PtyxError::new(
            PtyxErrorKind::Unsupported,
            "session option flags are unsupported",
        ));
    }
    if options.output_port == 0 || options.event_port == 0 {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "output and event ports must be valid",
        ));
    }

    Ok(SessionOptions {
        spawn: spawn_config_from_options(options)?,
        runtime: runtime_config_from_options(options),
    })
}

pub(crate) fn spawn_config_from_options(
    options: ptyx_session_options_t,
) -> Result<SpawnConfig, PtyxError> {
    let executable = string_to_os_string(options.executable)?;
    if executable.is_empty() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "executable must not be empty",
        ));
    }

    Ok(SpawnConfig {
        executable,
        argv: string_array(options.argv, options.argc)?,
        env_items: string_array(options.env_items, options.env_count)?,
        env_mode: environment_mode_from_abi(options.env_mode)?,
        cwd: string_to_os_string(options.cwd)?,
        size: to_pty_size(options.initial_size)?,
    })
}

fn environment_mode_from_abi(value: u32) -> Result<EnvironmentMode, PtyxError> {
    match value {
        PTYX_ENV_INHERIT => Ok(EnvironmentMode::Inherit),
        PTYX_ENV_OVERLAY => Ok(EnvironmentMode::Overlay),
        PTYX_ENV_REPLACE => Ok(EnvironmentMode::Replace),
        PTYX_ENV_CLEAR => Ok(EnvironmentMode::Clear),
        _ => Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "unknown environment mode",
        )),
    }
}

fn runtime_config_from_options(options: ptyx_session_options_t) -> RuntimeConfig {
    let output_batch_max_delay_us = default_u32(
        options.output_batch_max_delay_us,
        DEFAULT_OUTPUT_BATCH_DELAY_US,
    )
    .clamp(MIN_OUTPUT_BATCH_DELAY_US, MAX_OUTPUT_BATCH_DELAY_US);
    let mode_poll_interval_ms =
        default_u32(options.mode_poll_interval_ms, DEFAULT_MODE_POLL_INTERVAL_MS)
            .clamp(MIN_MODE_POLL_INTERVAL_MS, MAX_MODE_POLL_INTERVAL_MS);

    let require_acks = options.flags & PTYX_SESSION_REQUIRE_OUTPUT_ACKS != 0;
    RuntimeConfig {
        output: OutputConfig {
            require_acks,
            max_inflight: default_u64(options.max_inflight_bytes, DEFAULT_MAX_INFLIGHT_BYTES),
            read_buffer_size: default_u32(options.read_buffer_size, DEFAULT_READ_BUFFER_SIZE)
                .clamp(MIN_READ_BUFFER_SIZE, MAX_READ_BUFFER_SIZE)
                as usize,
            output_batch_max_bytes: default_u32(
                options.output_batch_max_bytes,
                DEFAULT_OUTPUT_BATCH_MAX_BYTES,
            )
            .clamp(MIN_OUTPUT_BATCH_MAX_BYTES, MAX_OUTPUT_BATCH_MAX_BYTES)
                as usize,
            output_batch_max_delay: Duration::from_micros(output_batch_max_delay_us as u64),
            use_external_output: options.flags & PTYX_SESSION_OUTPUT_EXTERNAL_TYPED_DATA != 0,
            max_external_output_bytes: default_u64(
                options.max_external_output_bytes,
                DEFAULT_MAX_EXTERNAL_OUTPUT_BYTES,
            ),
            output_port: options.output_port,
            event_port: options.event_port,
        },
        require_acks,
        enable_mode_events: options.flags & PTYX_SESSION_ENABLE_MODE_EVENTS != 0,
        mode_poll_interval: Duration::from_millis(mode_poll_interval_ms as u64),
        write_queue_max_bytes: default_u64(
            options.write_queue_max_bytes,
            DEFAULT_WRITE_QUEUE_MAX_BYTES as u64,
        )
        .try_into()
        .unwrap_or(usize::MAX)
        .clamp(MIN_WRITE_QUEUE_MAX_BYTES, MAX_WRITE_QUEUE_MAX_BYTES),
    }
}

fn default_u32(value: u32, default: u32) -> u32 {
    if value == 0 {
        default
    } else {
        value
    }
}

fn default_u64(value: u64, default: u64) -> u64 {
    if value == 0 {
        default
    } else {
        value
    }
}

pub(crate) fn string_to_string(value: ptyx_string_t) -> Result<String, PtyxError> {
    if value.len == 0 {
        return Ok(String::new());
    }
    if value.data.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "string data must not be null when length is non-zero",
        ));
    }
    // `data` is non-null for non-empty strings and valid for `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(value.data.cast::<u8>(), value.len) };
    String::from_utf8(bytes.to_vec()).map_err(|e| {
        PtyxError::new(
            PtyxErrorKind::InvalidArgument,
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
            PtyxErrorKind::InvalidArgument,
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
            PtyxErrorKind::InvalidArgument,
            "rows and columns must be positive",
        ));
    }
    Ok(PtySize {
        rows: u16::try_from(size.rows)
            .map_err(|_| PtyxError::new(PtyxErrorKind::InvalidArgument, "rows exceed u16"))?,
        cols: u16::try_from(size.columns)
            .map_err(|_| PtyxError::new(PtyxErrorKind::InvalidArgument, "columns exceed u16"))?,
        pixel_width: u16::try_from(size.pixel_width).map_err(|_| {
            PtyxError::new(PtyxErrorKind::InvalidArgument, "pixel width exceeds u16")
        })?,
        pixel_height: u16::try_from(size.pixel_height).map_err(|_| {
            PtyxError::new(PtyxErrorKind::InvalidArgument, "pixel height exceeds u16")
        })?,
    })
}

pub(crate) fn fill_string(
    value: &CStr,
    buffer: *mut c_char,
    inout_len: *mut usize,
) -> Result<(), PtyxError> {
    if inout_len.is_null() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "inout_len must not be null",
        ));
    }
    let bytes = value.to_bytes();
    let required_len = bytes.len() + 1;
    // The required length includes the trailing NUL so callers can reuse the
    // reported size as the next call's capacity.
    let capacity = unsafe { *inout_len };
    unsafe { *inout_len = required_len };
    if buffer.is_null() || capacity < required_len {
        return Err(PtyxError::new(
            PtyxErrorKind::BufferTooSmall,
            "buffer is too small",
        ));
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    Ok(())
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

    #[test]
    fn panic_is_caught_at_status_boundary() {
        let status = ffi_status(|| -> Result<(), PtyxError> {
            panic!("boom");
        });

        assert_eq!(status, PTYX_STATUS_NATIVE_ERROR);
    }

    #[test]
    fn session_options_rejects_unsupported_flags() {
        let (mut options, _executable) = valid_session_options();
        options.flags = PTYX_SESSION_SUPPORTED_FLAGS | (1 << 31);

        let error = session_options_from_abi(options).err().unwrap();

        assert_eq!(error.kind, PtyxErrorKind::Unsupported);
    }

    #[test]
    fn session_options_rejects_missing_ports() {
        let (mut options, _executable) = valid_session_options();
        options.output_port = 0;

        let error = session_options_from_abi(options).err().unwrap();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn spawn_config_rejects_unknown_environment_mode() {
        let (mut options, _executable) = valid_session_options();
        options.env_mode = u32::MAX;

        let error = spawn_config_from_options(options).err().unwrap();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn spawn_config_converts_environment_mode() {
        let (mut options, _executable) = valid_session_options();
        options.env_mode = PTYX_ENV_CLEAR;

        let config = spawn_config_from_options(options).unwrap();

        assert_eq!(config.env_mode, EnvironmentMode::Clear);
    }

    #[test]
    fn fill_string_rejects_null_length_pointer() {
        let value = CString::new("tty").unwrap();
        let mut buffer = [0 as c_char; 4];

        let error = fill_string(value.as_c_str(), buffer.as_mut_ptr(), ptr::null_mut())
            .err()
            .unwrap();

        assert_eq!(error.kind, PtyxErrorKind::InvalidArgument);
    }

    #[test]
    fn fill_string_reports_required_length_with_terminator() {
        let value = CString::new("tty").unwrap();
        let mut length = 0;

        let error = fill_string(value.as_c_str(), ptr::null_mut(), &mut length)
            .err()
            .unwrap();

        assert_eq!(error.kind, PtyxErrorKind::BufferTooSmall);
        assert_eq!(length, 4);
    }

    #[test]
    fn fill_string_writes_nul_terminated_text() {
        let value = CString::new("tty").unwrap();
        let mut length = 4;
        let mut buffer = [0 as c_char; 4];

        fill_string(value.as_c_str(), buffer.as_mut_ptr(), &mut length).unwrap();

        assert_eq!(length, 4);
        assert_eq!(unsafe { CStr::from_ptr(buffer.as_ptr()) }, value.as_c_str());
    }

    #[test]
    fn term_mode_conversion_marks_present_fields() {
        let mode = TermMode {
            canonical: Some(true),
            echo: None,
            signals: Some(false),
        };

        let mode = ptyx_term_mode_t::from(mode);

        assert_eq!(
            mode.valid_fields,
            PTYX_TERM_MODE_CANONICAL_VALID | PTYX_TERM_MODE_SIGNALS_VALID
        );
        assert!(mode.canonical);
        assert!(!mode.echo);
        assert!(!mode.signals);
    }

    fn valid_session_options() -> (ptyx_session_options_t, CString) {
        let executable = CString::new("/bin/sh").unwrap();
        let mut options = default_session_options();
        options.executable = ptyx_string_t {
            data: executable.as_ptr(),
            len: "/bin/sh".len(),
        };
        options.output_port = 1;
        options.event_port = 2;
        (options, executable)
    }
}
