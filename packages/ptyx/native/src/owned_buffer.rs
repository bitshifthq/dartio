//! Owned write buffers passed across the C ABI.
//!
//! Callers can fill a native allocation and transfer it to the writer queue
//! without copying. Ownership returns to the caller only when enqueueing fails.

use crate::abi::{PTYX_STATUS_INVALID_ARGUMENT, PTYX_STATUS_OK, PTYX_STATUS_OUT_OF_MEMORY};
use crate::error::PtyxError;

#[allow(non_camel_case_types, reason = "Opaque C ABI handle mirrors ptyx.h.")]
pub struct ptyx_owned_buffer {
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn alloc(
    capacity: usize,
    out_buffer: *mut *mut ptyx_owned_buffer,
) -> Result<u32, PtyxError> {
    if out_buffer.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "out_buffer must not be null",
        ));
    }

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| allocation_error())?;
    bytes.resize(capacity, 0);

    unsafe {
        // `out_buffer` is non-null and points to caller-owned writable memory.
        *out_buffer = Box::into_raw(Box::new(ptyx_owned_buffer { bytes }));
    }
    Ok(PTYX_STATUS_OK)
}

pub(crate) fn data(buffer: *mut ptyx_owned_buffer) -> *mut u8 {
    if buffer.is_null() {
        return std::ptr::null_mut();
    }
    // The caller owns the buffer and may write up to its allocated capacity.
    unsafe { (*buffer).bytes.as_mut_ptr() }
}

pub(crate) fn free(buffer: *mut ptyx_owned_buffer) {
    if buffer.is_null() {
        return;
    }
    unsafe {
        // `buffer` was created by `alloc` or returned after a failed transfer.
        drop(Box::from_raw(buffer));
    }
}

pub(crate) fn take(
    buffer: *mut ptyx_owned_buffer,
    length: usize,
) -> Result<Box<ptyx_owned_buffer>, PtyxError> {
    if buffer.is_null() {
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "buffer must not be null",
        ));
    }

    // Taking reconstructs the Rust box. On validation failure it is converted
    // back to a raw pointer so the caller still owns it.
    let mut buffer = unsafe { Box::from_raw(buffer) };
    if length > buffer.bytes.len() {
        let _ = Box::into_raw(buffer);
        return Err(PtyxError::new(
            PTYX_STATUS_INVALID_ARGUMENT,
            "write length exceeds buffer capacity",
        ));
    }

    buffer.bytes.truncate(length);
    Ok(buffer)
}

fn allocation_error() -> PtyxError {
    PtyxError::new(PTYX_STATUS_OUT_OF_MEMORY, "failed to allocate write buffer")
}
