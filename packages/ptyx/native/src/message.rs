//! Message shapes sent to native ports.
//!
//! Output messages go to the output port. Lifecycle, error, and terminal mode
//! messages go to the event port. The receiver keeps the numeric tags in sync
//! with the constants in `abi.rs`.

use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::abi::{
    ptyx_term_mode_t, PTYX_EVENT_ERROR, PTYX_EVENT_EXIT, PTYX_EVENT_TERM_MODE, PTYX_MESSAGE_CLOSED,
    PTYX_MESSAGE_OUTPUT,
};
use crate::error::PtyxError;

#[cfg(ptyx_dart_dl)]
use crate::dart_api::{post_array, DartCObject};

#[cfg(not(ptyx_dart_dl))]
use crate::dart_api::post_array;

pub(crate) fn post_output_copied(port: i64, data: *const u8, len: isize) -> bool {
    #[cfg(ptyx_dart_dl)]
    {
        let mut values = [
            DartCObject::int64(PTYX_MESSAGE_OUTPUT),
            DartCObject::typed_data(data, len),
        ];
        post_array(port, &mut values)
    }
    #[cfg(not(ptyx_dart_dl))]
    {
        let _ = (data, len);
        post_array(port)
    }
}

#[cfg(ptyx_dart_dl)]
struct ExternalOutputPeer {
    bytes: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
}

#[cfg(ptyx_dart_dl)]
impl Drop for ExternalOutputPeer {
    fn drop(&mut self) {
        self.outstanding
            .fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

#[cfg(ptyx_dart_dl)]
extern "C" fn external_output_finalizer(_isolate_data: *mut c_void, peer: *mut c_void) {
    if peer.is_null() {
        return;
    }
    // The receiver calls this once for the peer pointer supplied with the
    // external typed data object.
    unsafe { drop(Box::from_raw(peer.cast::<ExternalOutputPeer>())) };
}

#[cfg(ptyx_dart_dl)]
pub(crate) fn post_output_external(
    port: i64,
    bytes: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
) -> bool {
    let len = bytes.len();
    let mut peer = Box::new(ExternalOutputPeer { bytes, outstanding });
    let data = peer.bytes.as_mut_ptr();
    let peer_ptr = Box::into_raw(peer).cast::<c_void>();
    let mut values = [
        DartCObject::int64(PTYX_MESSAGE_OUTPUT),
        DartCObject::external_typed_data(data, len as isize, peer_ptr, external_output_finalizer),
    ];
    if post_array(port, &mut values) {
        true
    } else {
        // Posting failed, so the receiver will not run the finalizer.
        unsafe {
            drop(Box::from_raw(peer_ptr.cast::<ExternalOutputPeer>()));
        }
        false
    }
}

#[cfg(not(ptyx_dart_dl))]
pub(crate) fn post_output_external(
    _port: i64,
    bytes: Vec<u8>,
    outstanding: Arc<AtomicUsize>,
) -> bool {
    outstanding.fetch_sub(bytes.len(), Ordering::AcqRel);
    false
}

pub(crate) fn post_output_closed(port: i64) -> bool {
    #[cfg(ptyx_dart_dl)]
    {
        let mut values = [DartCObject::int64(PTYX_MESSAGE_CLOSED)];
        post_array(port, &mut values)
    }
    #[cfg(not(ptyx_dart_dl))]
    {
        post_array(port)
    }
}

pub(crate) fn post_exit(port: i64, exit_code: i32) -> bool {
    #[cfg(ptyx_dart_dl)]
    {
        let mut values = [
            DartCObject::int64(PTYX_EVENT_EXIT),
            DartCObject::int64(exit_code as i64),
        ];
        post_array(port, &mut values)
    }
    #[cfg(not(ptyx_dart_dl))]
    {
        let _ = exit_code;
        post_array(port)
    }
}

pub(crate) fn post_error(port: i64, source: i64, error: PtyxError) -> bool {
    let message = CString::new(error.message).unwrap_or_else(|_| CString::new("error").unwrap());
    post_error_message(port, source, error.status as i64, message.as_ptr())
}

fn post_error_message(port: i64, source: i64, status: i64, message: *const c_char) -> bool {
    #[cfg(ptyx_dart_dl)]
    {
        let mut values = [
            DartCObject::int64(PTYX_EVENT_ERROR),
            DartCObject::int64(source),
            DartCObject::int64(status),
            DartCObject::string(if message.is_null() {
                c"".as_ptr()
            } else {
                message
            }),
        ];
        post_array(port, &mut values)
    }
    #[cfg(not(ptyx_dart_dl))]
    {
        let _ = (source, status, message);
        post_array(port)
    }
}

pub(crate) fn post_term_mode(port: i64, mode: ptyx_term_mode_t) -> bool {
    #[cfg(ptyx_dart_dl)]
    {
        let mut values = [
            DartCObject::int64(PTYX_EVENT_TERM_MODE),
            DartCObject::int64(mode.valid_fields as i64),
            DartCObject::int64(mode.canonical as i64),
            DartCObject::int64(mode.echo as i64),
            DartCObject::int64(mode.signals as i64),
        ];
        post_array(port, &mut values)
    }
    #[cfg(not(ptyx_dart_dl))]
    {
        let _ = mode;
        post_array(port)
    }
}
