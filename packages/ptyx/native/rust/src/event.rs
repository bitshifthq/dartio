use crate::error::{ControlError, RecvError};
use crate::runtime::SessionControl;
use bytes::Bytes;
use futures_core::Stream;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Termination status of the direct child process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    /// The child returned a numeric exit code.
    Exited(u32),
    /// A Unix signal terminated the child.
    Signaled(u32),
}

impl ExitStatus {
    pub(crate) fn from_engine(status: i64) -> Option<Self> {
        if status < 0 {
            status
                .checked_neg()
                .and_then(|signal| u32::try_from(signal).ok())
                .map(Self::Signaled)
        } else {
            u32::try_from(status).ok().map(Self::Exited)
        }
    }
}

/// Canonical terminal input flags observed from the controller endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalMode {
    /// Whether canonical line input is enabled.
    pub canonical: bool,
    /// Whether input echo is enabled.
    pub echo: bool,
    /// Whether terminal-generated signals are enabled.
    pub signals: bool,
}

impl TerminalMode {
    pub(crate) const fn from_engine(modes: [bool; 3]) -> Self {
        Self {
            canonical: modes[0],
            echo: modes[1],
            signals: modes[2],
        }
    }
}

/// An ordered session event.
#[derive(Debug)]
pub enum Event {
    /// Bytes read from the pseudo terminal.
    Output(OutputChunk),
    /// Every safely readable output byte has been delivered.
    OutputDone,
    /// Accepted input could not be written completely.
    InputFailed,
    /// Native output ended with an error.
    OutputFailed,
    /// Runtime or process ownership was lost.
    InfrastructureFailed,
    /// The direct child terminated.
    Exited(ExitStatus),
    /// Explicit session cleanup reached a terminal result.
    Closed(CloseResult),
    /// An observed terminal mode changed.
    ModeChanged(TerminalMode),
}

/// Terminal result of explicit native session cleanup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CloseResult {
    /// Accepted input was lost before native delivery completed.
    pub input_failed: bool,
    /// Native output ended with an error.
    pub output_failed: bool,
    /// Complete native resource reclamation could not be established.
    pub cleanup_failed: bool,
}

/// A zero-copy output view that retains its native output credit.
///
/// Dropping the chunk returns its byte credit to the native runtime. Keeping a
/// chunk intentionally applies backpressure to further PTY reads.
pub struct OutputChunk {
    bytes: Bytes,
    control: Arc<SessionControl>,
}

impl std::fmt::Debug for OutputChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputChunk")
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl OutputChunk {
    pub(crate) fn new(bytes: Bytes, control: Arc<SessionControl>) -> Self {
        Self { bytes, control }
    }

    /// Number of bytes in this chunk.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether this chunk contains no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl AsRef<[u8]> for OutputChunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Deref for OutputChunk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl Drop for OutputChunk {
    fn drop(&mut self) {
        self.control.release_output(self.bytes.len());
    }
}

/// The single consumer of one session's ordered events.
pub struct Events {
    pub(crate) receiver: ptyx_engine::SessionEvents,
    pub(crate) control: Arc<SessionControl>,
    pub(crate) observing_modes: bool,
}

impl Events {
    /// Blocks until the next ordered event is available.
    pub fn recv(&mut self) -> Result<Event, RecvError> {
        let notice = self.receiver.recv().ok_or(RecvError)?;
        Ok(self.convert(notice))
    }

    /// Returns an event immediately when one is available.
    pub fn try_recv(&mut self) -> Result<Option<Event>, RecvError> {
        match self.receiver.try_recv() {
            Ok(Some(notice)) => Ok(Some(self.convert(notice))),
            Ok(None) => Ok(None),
            Err(_) => Err(RecvError),
        }
    }

    /// Stops output delivery and commits native drain-and-discard.
    ///
    /// Lifecycle events, including direct-child exit, remain available.
    pub fn cancel_output(&mut self) -> Result<(), ControlError> {
        self.control.cancel_output()
    }

    /// Enables or disables native terminal-mode observation.
    ///
    /// Enabling observation first emits the current mode, followed by later
    /// changes.
    ///
    /// Platforms without terminal-mode observation return an error.
    pub fn observe_modes(&mut self, observe: bool) -> Result<(), ControlError> {
        if self.observing_modes == observe {
            return Ok(());
        }
        self.control.observe_modes(observe)?;
        self.observing_modes = observe;
        Ok(())
    }

    fn convert(&self, notice: ptyx_engine::Notice) -> Event {
        match notice {
            ptyx_engine::Notice::Output { bytes, .. } => {
                Event::Output(OutputChunk::new(bytes, Arc::clone(&self.control)))
            }
            ptyx_engine::Notice::InputFailed(_) => Event::InputFailed,
            ptyx_engine::Notice::OutputFailed(_) => Event::OutputFailed,
            ptyx_engine::Notice::BrokerLost(_) => Event::InfrastructureFailed,
            ptyx_engine::Notice::OutputDone(_) => Event::OutputDone,
            ptyx_engine::Notice::Exit { status, .. } => ExitStatus::from_engine(status)
                .map(Event::Exited)
                .unwrap_or(Event::InfrastructureFailed),
            ptyx_engine::Notice::Closed { result, .. } => {
                Event::Closed(CloseResult::from_engine(result))
            }
            ptyx_engine::Notice::SpawnReady { .. } | ptyx_engine::Notice::SpawnFailed { .. } => {
                Event::InfrastructureFailed
            }
            ptyx_engine::Notice::ModeChanged { modes, .. } => {
                Event::ModeChanged(TerminalMode::from_engine(modes))
            }
        }
    }
}

impl Stream for Events {
    type Item = Result<Event, RecvError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let events = self.get_mut();
        match events.receiver.poll_recv(context) {
            Poll::Ready(Some(notice)) => Poll::Ready(Some(Ok(events.convert(notice)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        let notices = self.receiver.close();
        if self.observing_modes {
            let _ = self.control.observe_modes(false);
        }
        let _ = self.control.cancel_output();
        for notice in notices {
            if let ptyx_engine::Notice::Output { bytes, .. } = notice {
                self.control.release_output(bytes.len());
            }
        }
    }
}

impl CloseResult {
    pub(crate) const fn from_engine(result: ptyx_engine::CloseResult) -> Self {
        Self {
            input_failed: result.input_failed,
            output_failed: result.output_failed,
            cleanup_failed: result.cleanup_failed,
        }
    }

    /// Whether cleanup completed without a retained direction or cleanup
    /// failure.
    #[must_use]
    pub const fn is_success(self) -> bool {
        !self.input_failed && !self.output_failed && !self.cleanup_failed
    }
}
