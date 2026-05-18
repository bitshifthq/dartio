//! Message shapes sent to native ports.
//!
//! Output messages go to the output port. Lifecycle, error, and terminal mode
//! messages go to the event port. The receiver keeps the numeric tags in sync
//! with the constants in `abi.rs`.

use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use crate::abi::{PTYX_ERROR_SOURCE_MODE, PTYX_EVENT_TERM_MODE};
use crate::abi::{
    PTYX_ERROR_SOURCE_OUTPUT, PTYX_ERROR_SOURCE_WAIT, PTYX_ERROR_SOURCE_WRITE, PTYX_EVENT_ERROR,
    PTYX_EVENT_EXIT, PTYX_MESSAGE_CLOSED, PTYX_MESSAGE_OUTPUT,
};
use crate::dart_api::{post_array, DartValue};
use crate::error::PtyxError;
#[cfg(unix)]
use crate::term_mode::TermMode;

#[derive(Clone, Copy)]
pub(crate) enum ErrorSource {
    Output,
    Write,
    Wait,
    #[cfg(unix)]
    Mode,
}

pub(crate) fn post_output_copied(port: i64, data: *const u8, len: usize) -> bool {
    post_array(
        port,
        [
            DartValue::int64(PTYX_MESSAGE_OUTPUT),
            DartValue::typed_data(data, len),
        ],
    )
}

struct ExternalOutputPeer {
    bytes: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
}

impl Drop for ExternalOutputPeer {
    fn drop(&mut self) {
        self.outstanding
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

extern "C" fn external_output_finalizer(_isolate_data: *mut c_void, peer: *mut c_void) {
    if peer.is_null() {
        return;
    }
    // The receiver calls this once for the peer pointer supplied with the
    // external typed data object.
    unsafe { drop(Box::from_raw(peer.cast::<ExternalOutputPeer>())) };
}

pub(crate) fn post_output_external(
    port: i64,
    bytes: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
) -> bool {
    let len = bytes.len();
    let mut peer = Box::new(ExternalOutputPeer { bytes, outstanding });
    let data = peer.bytes.as_mut_ptr();
    let peer_ptr = Box::into_raw(peer).cast::<c_void>();
    if post_array(
        port,
        [
            DartValue::int64(PTYX_MESSAGE_OUTPUT),
            DartValue::external_typed_data(data, len, peer_ptr, external_output_finalizer),
        ],
    ) {
        true
    } else {
        // Posting failed, so the receiver will not run the finalizer.
        unsafe {
            drop(Box::from_raw(peer_ptr.cast::<ExternalOutputPeer>()));
        }
        false
    }
}

pub(crate) fn post_output_closed(port: i64) -> bool {
    post_array(port, [DartValue::int64(PTYX_MESSAGE_CLOSED)])
}

pub(crate) fn post_exit(port: i64, exit_code: i32) -> bool {
    post_array(
        port,
        [
            DartValue::int64(PTYX_EVENT_EXIT),
            DartValue::int64(exit_code as i64),
        ],
    )
}

pub(crate) fn post_error(port: i64, source: ErrorSource, error: PtyxError) -> bool {
    let status = crate::abi::status_for_error_kind(error.kind);
    let message = CString::new(error.message).unwrap_or_else(|_| CString::new("error").unwrap());
    post_error_message(
        port,
        error_source_value(source),
        status as i64,
        message.as_ptr(),
    )
}

fn error_source_value(source: ErrorSource) -> i64 {
    match source {
        ErrorSource::Output => PTYX_ERROR_SOURCE_OUTPUT,
        ErrorSource::Write => PTYX_ERROR_SOURCE_WRITE,
        ErrorSource::Wait => PTYX_ERROR_SOURCE_WAIT,
        #[cfg(unix)]
        ErrorSource::Mode => PTYX_ERROR_SOURCE_MODE,
    }
}

fn post_error_message(port: i64, source: i64, status: i64, message: *const c_char) -> bool {
    post_array(
        port,
        [
            DartValue::int64(PTYX_EVENT_ERROR),
            DartValue::int64(source),
            DartValue::int64(status),
            DartValue::string(if message.is_null() {
                c"".as_ptr()
            } else {
                message
            }),
        ],
    )
}

#[cfg(unix)]
pub(crate) fn post_term_mode(port: i64, mode: TermMode) -> bool {
    let mode = crate::abi::ptyx_term_mode_t::from(mode);
    post_array(
        port,
        [
            DartValue::int64(PTYX_EVENT_TERM_MODE),
            DartValue::int64(mode.valid_fields as i64),
            DartValue::int64(mode.canonical as i64),
            DartValue::int64(mode.echo as i64),
            DartValue::int64(mode.signals as i64),
        ],
    )
}
