#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PtyxErrorKind {
    Error,
    InvalidArgument,
    Unsupported,
    OutOfMemory,
    Closed,
    WouldBlock,
    BufferTooSmall,
    SpawnFailed,
    IoFailed,
    WaitFailed,
    BrokenPipe,
    PermissionDenied,
    Busy,
    NativeError,
}

#[derive(Clone, Debug)]
pub(crate) struct PtyxError {
    pub(crate) kind: PtyxErrorKind,
    pub(crate) message: String,
}

impl PtyxError {
    pub(crate) fn new(kind: PtyxErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn io(kind: PtyxErrorKind, error: impl std::fmt::Display) -> Self {
        Self::new(kind, error.to_string())
    }
}
