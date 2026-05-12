//! Error translation for the C ABI.
//!
//! Exported functions return status codes. The matching message is stored in
//! thread-local storage so callers can read it immediately after a failure.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::abi::{PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_OK};

#[derive(Clone, Debug)]
pub(crate) struct PtyxError {
    pub(crate) status: u32,
    pub(crate) message: String,
}

impl PtyxError {
    pub(crate) fn new(status: u32, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn io(status: u32, error: impl std::fmt::Display) -> Self {
        Self::new(status, error.to_string())
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
    F: FnOnce() -> Result<u32, PtyxError>,
{
    // No unwind may cross the C ABI. Convert panics into the same status and
    // last-error channel as ordinary native failures.
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(status)) => {
            if status == PTYX_STATUS_OK {
                clear_last_error();
            }
            status
        }
        Ok(Err(error)) => {
            let status = error.status;
            set_last_error(error);
            status
        }
        Err(_) => {
            set_last_error(PtyxError::new(
                PTYX_STATUS_NATIVE_ERROR,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_is_caught_at_status_boundary() {
        let status = ffi_status(|| -> Result<u32, PtyxError> {
            panic!("boom");
        });
        assert_eq!(status, PTYX_STATUS_NATIVE_ERROR);
    }
}
