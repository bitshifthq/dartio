use portable_pty::PtySize;
use std::ffi::OsString;
use std::time::Duration;

use crate::output::OutputConfig;

pub(crate) struct SessionOptions {
    pub(crate) spawn: SpawnConfig,
    pub(crate) runtime: RuntimeConfig,
}

pub(crate) struct SpawnConfig {
    pub(crate) executable: OsString,
    pub(crate) argv: Vec<OsString>,
    pub(crate) env_items: Vec<OsString>,
    pub(crate) env_mode: EnvironmentMode,
    pub(crate) cwd: OsString,
    pub(crate) size: PtySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnvironmentMode {
    Inherit,
    Overlay,
    Replace,
    Clear,
}

pub(crate) struct RuntimeConfig {
    pub(crate) output: OutputConfig,
    pub(crate) require_acks: bool,
    pub(crate) enable_mode_events: bool,
    pub(crate) mode_poll_interval: Duration,
    pub(crate) write_queue_max_bytes: usize,
}
