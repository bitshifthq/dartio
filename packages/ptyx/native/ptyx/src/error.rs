use std::error::Error;
use std::fmt;
use std::io;

use crate::event::CloseResult;
use bytes::Bytes;

/// Stable operation associated with a native session failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Native runtime construction or ownership.
    Runtime,
    /// Child and terminal spawn.
    Spawn,
    /// Terminal input delivery.
    Write,
    /// Terminal output delivery or cancellation.
    Output,
    /// Terminal resize.
    Resize,
    /// Child or terminal-job termination.
    Terminate,
    /// Direct-child exit-status observation.
    Exit,
    /// Atomic metadata snapshot or terminal-mode observation.
    Metadata,
    /// Session cleanup.
    Close,
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Runtime => "runtime",
            Self::Spawn => "spawn",
            Self::Write => "write",
            Self::Output => "output",
            Self::Resize => "resize",
            Self::Terminate => "termination",
            Self::Exit => "exit observation",
            Self::Metadata => "metadata",
            Self::Close => "close",
        })
    }
}

/// Stable category of a native session failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    /// Caller input was invalid.
    InvalidArgument,
    /// A bounded queue could not accept the complete operation.
    Backpressure,
    /// The operation does not apply in the current lifecycle state.
    WrongState,
    /// The requested capability is unavailable.
    Unsupported,
    /// The session or one transport direction is terminal.
    Closed,
    /// An operating-system operation failed.
    NativeFailure,
    /// Runtime ownership or event infrastructure was lost.
    InfrastructureLost,
}

impl fmt::Display for FailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArgument => "invalid argument",
            Self::Backpressure => "backpressure",
            Self::WrongState => "wrong state",
            Self::Unsupported => "unsupported operation",
            Self::Closed => "closed state",
            Self::NativeFailure => "native failure",
            Self::InfrastructureLost => "infrastructure loss",
        })
    }
}

/// Allocation-free detail retained from a native session failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationError {
    operation: Operation,
    kind: FailureKind,
    native_code: Option<i32>,
}

impl OperationError {
    /// Creates a stable failure value.
    #[must_use]
    pub const fn new(operation: Operation, kind: FailureKind, native_code: Option<i32>) -> Self {
        Self {
            operation,
            kind,
            native_code,
        }
    }

    /// Operation that failed.
    #[must_use]
    pub const fn operation(self) -> Operation {
        self.operation
    }

    /// Stable category of the failure.
    #[must_use]
    pub const fn kind(self) -> FailureKind {
        self.kind
    }

    /// Operating-system error code captured at the failure site.
    #[must_use]
    pub const fn native_code(self) -> Option<i32> {
        self.native_code
    }

    pub(crate) fn from_io(operation: Operation, error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
                FailureKind::InvalidArgument
            }
            io::ErrorKind::WouldBlock => FailureKind::Backpressure,
            io::ErrorKind::Unsupported => FailureKind::Unsupported,
            _ => FailureKind::NativeFailure,
        };
        Self::new(operation, kind, error.raw_os_error())
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed with {}", self.operation, self.kind)?;
        if let Some(code) = self.native_code {
            write!(formatter, " (native code {code})")?;
        }
        Ok(())
    }
}

impl Error for OperationError {}

/// A terminal size is outside the native PTY contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSize;

impl fmt::Display for InvalidSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("terminal rows and columns must be in 1..=32767")
    }
}

impl Error for InvalidSize {}

/// A native runtime could not be created.
#[derive(Debug)]
pub enum RuntimeError {
    /// A Unix broker could not be selected or prepared for execution.
    Broker(io::Error),
    /// Native runtime initialization failed.
    Native(io::Error),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Broker(error) => write!(formatter, "Unix broker selection failed: {error}"),
            Self::Native(error) => {
                write!(formatter, "native runtime initialization failed: {error}")
            }
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Broker(error) | Self::Native(error) => Some(error),
        }
    }
}

/// A child could not be attached to a pseudo terminal.
#[derive(Debug)]
pub enum SpawnError {
    /// The executable is empty.
    EmptyExecutable,
    /// A value contains a NUL byte and cannot cross the native process API.
    ContainsNul {
        /// Spawn field containing the NUL byte.
        field: &'static str,
    },
    /// An environment key is empty or contains `=`.
    InvalidEnvironmentKey,
    /// An input or output byte capacity is outside the supported range.
    InvalidCapacity {
        /// Capacity field that failed validation.
        field: &'static str,
        /// Rejected byte count.
        bytes: usize,
    },
    /// The graceful-close timeout exceeds 60 seconds.
    InvalidCloseTimeout,
    /// The native spawn operation failed.
    Native(io::Error),
    /// Native ownership was acquired but session publication failed.
    Activation,
}

impl fmt::Display for SpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyExecutable => formatter.write_str("the executable must not be empty"),
            Self::ContainsNul { field } => write!(formatter, "{field} contains a NUL byte"),
            Self::InvalidEnvironmentKey => {
                formatter.write_str("environment keys must be nonempty and must not contain '='")
            }
            Self::InvalidCapacity { field, bytes } => {
                write!(
                    formatter,
                    "{field} capacity {bytes} is outside 1..=67108864"
                )
            }
            Self::InvalidCloseTimeout => {
                formatter.write_str("graceful close timeout must not exceed 60 seconds")
            }
            Self::Native(error) => write!(formatter, "native spawn failed: {error}"),
            Self::Activation => formatter.write_str("native session activation failed"),
        }
    }
}

impl Error for SpawnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            _ => None,
        }
    }
}

/// A write was not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteErrorKind {
    /// Empty writes are not valid input operations.
    Empty,
    /// The bounded input queue cannot accept the complete buffer now.
    Backpressure,
    /// The input direction is permanently closed or failed.
    Closed,
    /// The native runtime cannot establish whether it owns the session.
    Infrastructure,
}

impl fmt::Display for WriteErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "an input write must contain at least one byte",
            Self::Backpressure => "the bounded input queue is full",
            Self::Closed => "the session input is closed",
            Self::Infrastructure => "the native runtime is unavailable",
        })
    }
}

impl Error for WriteErrorKind {}

/// A write was rejected without transferring ownership of its bytes.
#[derive(Debug, Eq, PartialEq)]
pub struct WriteError {
    kind: WriteErrorKind,
    bytes: Bytes,
    failure: Option<OperationError>,
}

impl WriteError {
    pub(crate) const fn new(
        kind: WriteErrorKind,
        bytes: Bytes,
        failure: Option<OperationError>,
    ) -> Self {
        Self {
            kind,
            bytes,
            failure,
        }
    }

    /// Reason the complete write was rejected.
    #[must_use]
    pub const fn kind(&self) -> WriteErrorKind {
        self.kind
    }

    /// Sticky native cause when input previously failed or infrastructure was lost.
    #[must_use]
    pub const fn failure(&self) -> Option<OperationError> {
        self.failure
    }

    /// Recovers the buffer that was not accepted.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl Error for WriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.failure
            .as_ref()
            .map(|failure| failure as &(dyn Error + 'static))
    }
}

/// A synchronous session control operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlError {
    failure: OperationError,
}

impl ControlError {
    pub(crate) const fn new(failure: OperationError) -> Self {
        Self { failure }
    }

    /// Structured native failure.
    #[must_use]
    pub const fn failure(&self) -> OperationError {
        self.failure
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.failure)
    }
}

/// Explicit close could not be started or observed to completion.
#[derive(Debug)]
pub enum CloseError {
    /// Native close admission failed.
    Native(io::Error),
    /// The runtime stopped without publishing the terminal cleanup result.
    CompletionLost,
    /// Cleanup completed with one or more retained failures.
    Failed(CloseResult),
}

impl fmt::Display for CloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(error) => write!(formatter, "native close failed: {error}"),
            Self::CompletionLost => {
                formatter.write_str("native close completion was not published")
            }
            Self::Failed(result) => {
                write!(formatter, "native close completed with failures")?;
                if let Some(failure) = result.primary_failure() {
                    write!(formatter, ": {failure}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for CloseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            Self::CompletionLost | Self::Failed(_) => None,
        }
    }
}

/// The session event source has closed and contains no further events.
///
/// This is expected when reading again after the terminal route was consumed.
/// It indicates premature loss only when no terminal event was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the native event source closed")
    }
}

impl Error for RecvError {}

/// Session metadata could not be captured atomically.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataError {
    failure: OperationError,
}

impl MetadataError {
    pub(crate) const fn new(failure: OperationError) -> Self {
        Self { failure }
    }

    /// Structured native failure.
    #[must_use]
    pub const fn failure(&self) -> OperationError {
        self.failure
    }
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl Error for MetadataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.failure)
    }
}

#[cfg(test)]
mod tests {
    use super::{FailureKind, Operation, OperationError};
    use std::io;

    #[test]
    fn operating_system_pipe_failure_is_not_normalized_to_clean_closure() {
        let error = io::Error::new(io::ErrorKind::BrokenPipe, "write failed");

        let failure = OperationError::from_io(Operation::Write, &error);

        assert_eq!(failure.kind(), FailureKind::NativeFailure);
    }
}
