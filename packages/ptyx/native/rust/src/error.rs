use std::error::Error;
use std::fmt;
use std::io;

use crate::event::CloseResult;

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
    /// Unix requires the companion broker executable.
    MissingBroker,
    /// Native runtime initialization failed.
    Native(io::Error),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBroker => formatter.write_str("a Unix broker path is required"),
            Self::Native(error) => {
                write!(formatter, "native runtime initialization failed: {error}")
            }
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MissingBroker => None,
            Self::Native(error) => Some(error),
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
pub enum WriteError {
    /// Empty writes are not valid input operations.
    Empty,
    /// The bounded input queue cannot accept the complete buffer now.
    Backpressure,
    /// The input direction is permanently closed or failed.
    Closed,
    /// The native runtime cannot establish whether it owns the session.
    Infrastructure,
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "an input write must contain at least one byte",
            Self::Backpressure => "the bounded input queue is full",
            Self::Closed => "the session input is closed",
            Self::Infrastructure => "the native runtime is unavailable",
        })
    }
}

impl Error for WriteError {}

/// A synchronous session control operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlError {
    operation: &'static str,
}

impl ControlError {
    pub(crate) const fn new(operation: &'static str) -> Self {
        Self { operation }
    }

    /// Operation that failed.
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "native {} failed", self.operation)
    }
}

impl Error for ControlError {}

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
            Self::Failed(result) => write!(
                formatter,
                "native close completed with failures: input={}, output={}, cleanup={}",
                result.input_failed, result.output_failed, result.cleanup_failed
            ),
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

/// The native event source ended before the session reached a terminal event.
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
pub struct MetadataError;

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native session metadata is unavailable")
    }
}

impl Error for MetadataError {}
