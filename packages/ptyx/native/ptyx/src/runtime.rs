use crate::error::{
    CloseError, ControlError, FailureKind, MetadataError, OperationError, RuntimeError, SpawnError,
    WriteError, WriteErrorKind,
};
use crate::event::{CloseResult, Events, TerminalMode};
use crate::options::{SpawnOptions, SpawnParts};
use crate::Size;
use bytes::Bytes;
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};

/// Configures one native PTY runtime.
#[derive(Default)]
pub struct RuntimeBuilder {
    broker_path: Option<PathBuf>,
}

impl RuntimeBuilder {
    /// Creates a builder with platform defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects the companion Unix broker executable.
    ///
    /// A qualified Unix release securely materializes its packaged matching
    /// broker by default. Source builds without that target asset require this
    /// option or `PTYX_BROKER`. Windows ignores this override.
    #[must_use]
    pub fn broker_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.broker_path = Some(path.into());
        self
    }

    /// Creates the native runtime.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let native = {
            let broker = match self.broker_path {
                Some(path) => path,
                None => crate::broker::broker_path().map_err(RuntimeError::Broker)?,
            };
            crate::engine::IntegratedRuntime::try_new(&broker).map_err(RuntimeError::Native)?
        };
        #[cfg(windows)]
        let native = crate::engine::IntegratedRuntime::try_new().map_err(RuntimeError::Native)?;

        let events = native.take_notifications().ok_or_else(|| {
            RuntimeError::Native(std::io::Error::other("event source unavailable"))
        })?;
        let native = Arc::new(native);

        Ok(Runtime {
            inner: Arc::new(RuntimeInner { native, events }),
        })
    }
}

/// A shared native PTY runtime.
///
/// A runtime uses shared native readiness infrastructure for all sessions.
/// Per-session event receivers do not create facade worker threads.
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    /// Creates a runtime using build-provided platform support.
    ///
    /// On Unix this returns [`RuntimeError::Broker`] when the build contains
    /// no target-matched broker and `PTYX_BROKER` is unset. Repository source
    /// builds intentionally require that explicit broker selection.
    pub fn new() -> Result<Self, RuntimeError> {
        RuntimeBuilder::new().build()
    }

    /// Creates a runtime builder.
    #[must_use]
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Submits an asynchronous child spawn.
    ///
    /// Validation and owned option conversion finish before this method
    /// returns. Waiting never retains references to caller-owned collections.
    ///
    /// ```no_run
    /// # use ptyx::{Runtime, SpawnOptions};
    /// # fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let runtime = Runtime::new()?;
    /// let spawn = runtime.spawn(SpawnOptions::new("sh"))?;
    /// let (_session, _events) = spawn.wait()?.into_parts();
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn(&self, options: SpawnOptions) -> Result<Spawn, SpawnError> {
        let parts = options.into_parts();
        validate_capacity("input", parts.input_capacity)?;
        validate_capacity("output", parts.output_capacity)?;
        if parts.graceful_close_timeout > std::time::Duration::from_secs(60) {
            return Err(SpawnError::InvalidCloseTimeout);
        }
        let input_capacity = parts.input_capacity;
        let output_capacity = parts.output_capacity;
        let native_options = native_spawn_options(parts)?;
        let completion = self
            .inner
            .native
            .spawn_start(native_options, input_capacity, output_capacity)
            .map_err(SpawnError::Native)?;

        Ok(Spawn {
            runtime: Arc::clone(&self.inner),
            completion: Some(completion),
        })
    }

    /// Spawns and publishes one session, blocking until native ownership is
    /// established.
    pub fn spawn_blocking(&self, options: SpawnOptions) -> Result<Spawned, SpawnError> {
        self.spawn(options)?.wait()
    }
}

/// Completion of one engine-owned asynchronous spawn.
#[must_use = "spawn does not complete unless it is awaited or waited"]
pub struct Spawn {
    runtime: Arc<RuntimeInner>,
    completion: Option<crate::engine::Completion<std::io::Result<u64>>>,
}

impl Spawn {
    fn resolve(&self, result: Option<std::io::Result<u64>>) -> Result<Spawned, SpawnError> {
        let handle = result
            .ok_or(SpawnError::Activation)?
            .map_err(SpawnError::Native)?;
        publish_session(&self.runtime, handle)
    }

    /// Blocks until the child is attached and its session is published.
    pub fn wait(mut self) -> Result<Spawned, SpawnError> {
        let completion = self
            .completion
            .take()
            .expect("spawn completion is present until resolved");
        self.resolve(completion.wait())
    }
}

impl Future for Spawn {
    type Output = Result<Spawned, SpawnError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let completion = self
            .completion
            .as_mut()
            .expect("completed spawn futures must not be polled again");
        let result = match Pin::new(completion).poll(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        self.completion = None;
        Poll::Ready(self.resolve(result))
    }
}

impl Drop for Spawn {
    fn drop(&mut self) {
        let Some(mut completion) = self.completion.take() else {
            return;
        };
        if let Ok(Some(Ok(handle))) = completion.try_take() {
            let _ = self.runtime.native.try_abandon(handle);
        }
    }
}

fn publish_session(runtime: &Arc<RuntimeInner>, handle: u64) -> Result<Spawned, SpawnError> {
    let Some(receiver) = runtime.events.session(handle) else {
        let _ = runtime.native.try_abandon(handle);
        return Err(SpawnError::Activation);
    };
    let control = Arc::new(SessionControl {
        runtime: Arc::clone(runtime),
        handle,
        closing: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        close: CloseShared::default(),
        output_cancelled: AtomicBool::new(false),
    });
    if !runtime.native.activate(handle) {
        let _ = runtime.native.try_abandon(handle);
        return Err(SpawnError::Activation);
    }
    Ok(Spawned {
        session: Session {
            control: Arc::clone(&control),
        },
        events: Events {
            receiver,
            control,
            observing_modes: false,
        },
    })
}

/// A newly published PTY session and its single event consumer.
pub struct Spawned {
    session: Session,
    events: Events,
}

impl Spawned {
    /// Separates the concurrent session control and ordered event halves.
    #[must_use]
    pub fn into_parts(self) -> (Session, Events) {
        (self.session, self.events)
    }
}

/// Control operations for one PTY session.
///
/// The value is safe to share by reference across threads. Writes are ordered
/// by native admission.
pub struct Session {
    control: Arc<SessionControl>,
}

impl Session {
    /// Synchronously admits the complete buffer for ordered native delivery.
    ///
    /// Success transfers the owned buffer without another facade or engine
    /// copy. The operation never waits for PTY writability or queue capacity.
    /// An empty buffer is a successful no-op, matching the C ABI.
    pub fn write(&self, bytes: Bytes) -> Result<(), WriteError> {
        if self.control.closing.load(Ordering::Acquire) {
            return Err(WriteError::new(WriteErrorKind::Closed, bytes, None));
        }
        self.control
            .runtime
            .native
            .write(self.control.handle, bytes)
    }

    /// Returns the current terminal dimensions.
    pub fn size(&self) -> Result<Size, MetadataError> {
        self.control
            .runtime
            .native
            .size(self.control.handle)
            .and_then(|size| {
                Size::from_native(size).ok_or_else(|| {
                    OperationError::new(
                        crate::error::Operation::Size,
                        FailureKind::NativeFailure,
                        None,
                    )
                })
            })
            .map_err(MetadataError::new)
    }

    /// Returns the direct-child process identifier.
    pub fn process_id(&self) -> Result<u64, MetadataError> {
        self.control
            .runtime
            .native
            .process_id(self.control.handle)
            .and_then(|pid| {
                u64::try_from(pid).map_err(|_| {
                    OperationError::new(
                        crate::error::Operation::ProcessId,
                        FailureKind::NativeFailure,
                        None,
                    )
                })
            })
            .map_err(MetadataError::new)
    }

    /// Returns terminal mode information when supported by the platform.
    pub fn terminal_mode(&self) -> Result<Option<TerminalMode>, MetadataError> {
        match self
            .control
            .runtime
            .native
            .terminal_mode(self.control.handle)
        {
            Ok(mode) => Ok(Some(TerminalMode::from_engine(mode))),
            Err(failure) if failure.kind() == FailureKind::Unsupported => Ok(None),
            Err(failure) => Err(MetadataError::new(failure)),
        }
    }

    /// Returns the controller terminal name when supported by the platform.
    pub fn terminal_name(&self) -> Result<Option<OsString>, MetadataError> {
        match self
            .control
            .runtime
            .native
            .terminal_name(self.control.handle)
        {
            Ok(name) => Ok(Some(native_terminal_name(name))),
            Err(failure) if failure.kind() == FailureKind::Unsupported => Ok(None),
            Err(failure) => Err(MetadataError::new(failure)),
        }
    }

    /// Changes the terminal cell and optional pixel dimensions.
    pub fn resize(&self, size: Size) -> Result<(), ControlError> {
        self.control
            .runtime
            .native
            .resize(self.control.handle, size.native())
            .map_err(ControlError::new)
    }

    /// Requests termination of the owned terminal job.
    ///
    /// Returns `false` when the direct child already exited.
    pub fn terminate(&self) -> Result<bool, ControlError> {
        self.control
            .runtime
            .native
            .signal(self.control.handle, 15)
            .map_err(ControlError::new)
    }

    /// Starts idempotent native shutdown and returns its completion.
    pub fn close(&self) -> Result<Close, CloseError> {
        self.control.start_close()?;
        self.control.closing.store(true, Ordering::Release);
        Ok(Close {
            control: Arc::clone(&self.control),
            waker: None,
        })
    }
}

/// Completion of explicit native cleanup.
#[must_use = "close does not complete unless it is awaited or waited"]
pub struct Close {
    control: Arc<SessionControl>,
    waker: Option<usize>,
}

impl Close {
    /// Blocks until native cleanup reaches a terminal result.
    pub fn wait(self) -> Result<(), CloseError> {
        self.control.wait_close().and_then(close_result)
    }
}

impl Future for Close {
    type Output = Result<(), CloseError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let control = Arc::clone(&self.control);
        control.poll_close(context, &mut self.waker)
    }
}

impl Drop for Close {
    fn drop(&mut self) {
        self.control.unregister_close_waker(self.waker.take());
    }
}

fn close_result(result: CloseResult) -> Result<(), CloseError> {
    if result.is_success() {
        Ok(())
    } else {
        Err(CloseError::Failed(result))
    }
}

pub(crate) struct SessionControl {
    runtime: Arc<RuntimeInner>,
    handle: u64,
    closing: AtomicBool,
    closed: AtomicBool,
    close: CloseShared,
    output_cancelled: AtomicBool,
}

impl std::fmt::Debug for SessionControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionControl")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl SessionControl {
    fn start_close(&self) -> Result<(), CloseError> {
        let mut state = self.close.state();
        if state.completion.is_none() && !state.completion_waiting && state.result.is_none() {
            state.completion = Some(
                self.runtime
                    .native
                    .close_start(self.handle)
                    .map_err(CloseError::Native)?,
            );
        }
        Ok(())
    }

    fn wait_close(&self) -> Result<CloseResult, CloseError> {
        let mut state = self.close.state();
        loop {
            if let Some(result) = state.result {
                return result.result();
            }
            if let Some(completion) = state.completion.take() {
                state.completion_waiting = true;
                drop(state);
                let result = completion
                    .wait()
                    .map(CloseResult::from_engine)
                    .map(CachedClose::Complete)
                    .unwrap_or(CachedClose::Lost);
                return self.finish_close(result);
            }
            state = self
                .close
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn poll_close(
        &self,
        context: &mut Context<'_>,
        waker_index: &mut Option<usize>,
    ) -> Poll<Result<(), CloseError>> {
        let mut state = self.close.state();
        if let Some(result) = state.result {
            return Poll::Ready(result.result().and_then(close_result));
        }
        let Some(completion) = state.completion.as_mut() else {
            register_waker(&mut state.wakers, waker_index, context.waker());
            return Poll::Pending;
        };
        match Pin::new(completion).poll(context) {
            Poll::Pending => {
                register_waker(&mut state.wakers, waker_index, context.waker());
                Poll::Pending
            }
            Poll::Ready(result) => {
                state.completion = None;
                state.completion_waiting = true;
                let result = result
                    .map(CloseResult::from_engine)
                    .map(CachedClose::Complete)
                    .unwrap_or(CachedClose::Lost);
                drop(state);
                Poll::Ready(self.finish_close(result).and_then(close_result))
            }
        }
    }

    fn finish_close(&self, result: CachedClose) -> Result<CloseResult, CloseError> {
        // A lost completion means the reactor stopped before it could report
        // the terminal close result.  Do not let that transport failure turn
        // into an abandoned native session: enqueue the idempotent abandon
        // operation while the runtime is still owned by this control.
        if matches!(result, CachedClose::Lost) {
            let _ = self.runtime.native.try_abandon(self.handle);
        }
        let (result, wakers) = {
            let mut state = self.close.state();
            if state.result.is_none() {
                state.result = Some(result);
            }
            state.completion_waiting = false;
            self.closed.store(true, Ordering::Release);
            (
                state.result.expect("close result was stored"),
                std::mem::take(&mut state.wakers)
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
            )
        };
        self.close.ready.notify_all();
        for waker in wakers {
            waker.wake();
        }
        result.result()
    }

    fn unregister_close_waker(&self, index: Option<usize>) {
        let Some(index) = index else {
            return;
        };
        let mut state = self.close.state();
        clear_waker(&mut state.wakers, index);
    }

    pub(crate) fn release_output(&self, bytes: usize) {
        if bytes != 0 {
            let _ = self.runtime.native.credit_async(self.handle, bytes);
        }
    }

    pub(crate) fn cancel_output(&self) -> Result<(), ControlError> {
        if self.closed.load(Ordering::Acquire) || self.output_cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.runtime
            .native
            .cancel_output(self.handle)
            .map(|()| self.output_cancelled.store(true, Ordering::Release))
            .map_err(ControlError::new)
    }

    pub(crate) fn observe_modes(&self, observe: bool) -> Result<(), ControlError> {
        if self.closed.load(Ordering::Acquire) {
            return (!observe).then_some(()).ok_or_else(|| {
                ControlError::new(OperationError::new(
                    crate::error::Operation::TerminalMode,
                    crate::error::FailureKind::Closed,
                    None,
                ))
            });
        }
        self.runtime
            .native
            .observe_mode(self.handle, observe)
            .map_err(ControlError::new)
    }
}

#[derive(Clone, Copy)]
enum CachedClose {
    Complete(CloseResult),
    Lost,
}

impl CachedClose {
    fn result(self) -> Result<CloseResult, CloseError> {
        match self {
            Self::Complete(result) => Ok(result),
            Self::Lost => Err(CloseError::CompletionLost),
        }
    }
}

#[derive(Default)]
struct CloseShared {
    state: Mutex<CloseState>,
    ready: Condvar,
}

impl CloseShared {
    fn state(&self) -> std::sync::MutexGuard<'_, CloseState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Default)]
struct CloseState {
    completion: Option<crate::engine::Completion<crate::engine::CloseResult>>,
    completion_waiting: bool,
    result: Option<CachedClose>,
    wakers: Vec<Option<std::task::Waker>>,
}

fn register_waker(
    wakers: &mut Vec<Option<std::task::Waker>>,
    index: &mut Option<usize>,
    waker: &std::task::Waker,
) {
    if let Some(existing) = index
        .as_ref()
        .and_then(|index| wakers.get_mut(*index))
        .and_then(Option::as_mut)
    {
        existing.clone_from(waker);
    } else {
        let vacant = wakers.iter().position(Option::is_none);
        let vacant = vacant.unwrap_or_else(|| {
            wakers.push(None);
            wakers.len() - 1
        });
        wakers[vacant] = Some(waker.clone());
        *index = Some(vacant);
    }
}

fn clear_waker(wakers: &mut Vec<Option<std::task::Waker>>, index: usize) {
    if let Some(slot) = wakers.get_mut(index) {
        *slot = None;
    }
    while wakers.last().is_some_and(Option::is_none) {
        wakers.pop();
    }
}

impl Drop for SessionControl {
    fn drop(&mut self) {
        if !self.closed.load(Ordering::Acquire) {
            let _ = self.runtime.native.try_abandon(self.handle);
        }
    }
}

struct RuntimeInner {
    native: Arc<crate::engine::IntegratedRuntime>,
    events: crate::engine::RuntimeEvents,
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let _ = self.native.shutdown();
    }
}

fn validate_capacity(field: &'static str, bytes: usize) -> Result<(), SpawnError> {
    if (1..=crate::engine::MAX_SESSION_CAPACITY).contains(&bytes) {
        Ok(())
    } else {
        Err(SpawnError::InvalidCapacity { field, bytes })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_terminal_name(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;

    OsString::from_vec(bytes)
}

#[cfg(windows)]
fn native_terminal_name(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

fn native_spawn_options(parts: SpawnParts) -> Result<crate::engine::BrokerSpawn, SpawnError> {
    if parts.executable.is_empty() {
        return Err(SpawnError::EmptyExecutable);
    }
    validate_native_string(&parts.executable, "executable")?;
    for argument in &parts.arguments {
        validate_native_string(argument, "argument")?;
    }
    if let Some(environment) = &parts.environment {
        for (key, value) in environment {
            validate_environment_key(key)?;
            validate_native_string(value, "environment")?;
        }
    }
    if let Some(cwd) = &parts.cwd {
        validate_native_string(cwd.as_os_str(), "working directory")?;
    }
    let size = parts.initial_size.native();

    Ok(crate::engine::BrokerSpawn {
        executable: parts.executable,
        arguments: parts.arguments,
        environment: parts.environment,
        cwd: parts.cwd,
        rows: size[0],
        columns: size[1],
        pixel_width: size[2],
        pixel_height: size[3],
        graceful_close_timeout: parts.graceful_close_timeout,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_native_string(value: &OsStr, field: &'static str) -> Result<(), SpawnError> {
    use std::os::unix::ffi::OsStrExt;

    if value.as_bytes().contains(&0) {
        Err(SpawnError::ContainsNul { field })
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn validate_native_string(value: &OsStr, field: &'static str) -> Result<(), SpawnError> {
    use std::os::windows::ffi::OsStrExt;

    if value.encode_wide().any(|unit| unit == 0) {
        Err(SpawnError::ContainsNul { field })
    } else {
        Ok(())
    }
}

fn validate_environment_key(key: &OsStr) -> Result<(), SpawnError> {
    if key.is_empty() || os_string_contains_equals(key) {
        Err(SpawnError::InvalidEnvironmentKey)
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn os_string_contains_equals(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().contains(&b'=')
}

#[cfg(windows)]
fn os_string_contains_equals(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().any(|unit| unit == u16::from(b'='))
}

#[cfg(test)]
mod tests {
    use super::{clear_waker, register_waker};
    use std::task::Waker;

    #[test]
    fn dropped_close_waiters_do_not_accumulate_waker_slots() {
        let mut wakers = Vec::new();
        for _ in 0..10_000 {
            let mut index = None;
            register_waker(&mut wakers, &mut index, Waker::noop());
            clear_waker(&mut wakers, index.expect("waker slot registered"));
        }

        assert!(wakers.is_empty());
    }
}
