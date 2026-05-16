use crate::error::{PtyxError, PtyxErrorKind};

pub(crate) struct OwnedBuffer {
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn alloc(capacity: usize) -> Result<OwnedBuffer, PtyxError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| allocation_error())?;
    bytes.resize(capacity, 0);
    Ok(OwnedBuffer { bytes })
}

pub(crate) fn truncate(buffer: &mut OwnedBuffer, length: usize) -> Result<(), PtyxError> {
    if length > buffer.bytes.len() {
        return Err(PtyxError::new(
            PtyxErrorKind::InvalidArgument,
            "write length exceeds buffer capacity",
        ));
    }

    buffer.bytes.truncate(length);
    Ok(())
}

fn allocation_error() -> PtyxError {
    PtyxError::new(
        PtyxErrorKind::OutOfMemory,
        "failed to allocate write buffer",
    )
}
