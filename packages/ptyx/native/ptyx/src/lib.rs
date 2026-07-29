#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(unix)]
#[allow(unsafe_code)]
mod broker;
#[allow(missing_docs, unsafe_code)]
mod engine;
mod error;
mod event;
mod options;
mod runtime;

/// Private native-adapter surface.
///
/// This module is not part of the supported application API.
#[cfg(feature = "__private_adapter")]
#[doc(hidden)]
pub mod __private_adapter {
    #[cfg(unix)]
    pub use crate::broker::broker_path;
    pub use crate::engine::{BrokerSpawn, CopyWriteResult, Failure, IntegratedRuntime, Notice};
    pub use crate::error::FailureKind;
}

/// Private entry points that let fuzz targets exercise production decoders.
#[cfg(all(fuzzing, any(target_os = "linux", target_os = "macos")))]
#[doc(hidden)]
pub mod __fuzzing {
    /// Exercises the controller-side broker protocol frame decoder.
    pub fn broker_protocol_frame(bytes: &[u8]) {
        crate::engine::fuzz_broker_protocol_frame(bytes);
    }
}

pub use error::{
    CloseError, ControlError, FailureKind, InvalidSize, MetadataError, Operation, OperationError,
    RecvError, RuntimeError, SpawnError, WriteError, WriteErrorKind,
};
pub use event::{CloseResult, Event, Events, ExitStatus, OutputChunk, TerminalMode};
pub use options::{Size, SpawnOptions};
pub use runtime::{Close, Runtime, RuntimeBuilder, Session, SessionSnapshot, Spawn, Spawned};
