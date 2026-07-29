#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod event;
mod options;
mod runtime;

pub use error::{
    CloseError, ControlError, InvalidSize, MetadataError, RecvError, RuntimeError, SpawnError,
    WriteError,
};
pub use event::{CloseResult, Event, Events, ExitStatus, OutputChunk, TerminalMode};
pub use options::{Size, SpawnOptions};
pub use runtime::{Close, Runtime, RuntimeBuilder, Session, SessionSnapshot, Spawn, Spawned};
