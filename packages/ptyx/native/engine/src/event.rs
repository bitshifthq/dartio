use std::collections::{HashMap, VecDeque};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    InvalidInput,
    Backpressure,
    Unsupported,
    NotFound,
    PermissionDenied,
    Infrastructure,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub kind: FailureKind,
    pub native_code: Option<i32>,
}

impl From<&io::Error> for Failure {
    fn from(error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => FailureKind::InvalidInput,
            io::ErrorKind::WouldBlock => FailureKind::Backpressure,
            io::ErrorKind::Unsupported => FailureKind::Unsupported,
            io::ErrorKind::NotFound => FailureKind::NotFound,
            io::ErrorKind::PermissionDenied => FailureKind::PermissionDenied,
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted => {
                FailureKind::Infrastructure
            }
            _ => FailureKind::Other,
        };
        Self {
            kind,
            native_code: error.raw_os_error(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CloseResult {
    pub input_failed: bool,
    pub output_failed: bool,
    pub cleanup_failed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    Output { handle: u64, bytes: Bytes },
    InputFailed(u64),
    OutputFailed(u64),
    BrokerLost(u64),
    OutputDone(u64),
    Exit { handle: u64, status: i64 },
    Closed { handle: u64, result: CloseResult },
    SpawnReady { request: u64, handle: u64 },
    SpawnFailed { request: u64, failure: Failure },
    ModeChanged { handle: u64, modes: [bool; 3] },
}

impl Notice {
    pub fn handle(&self) -> u64 {
        match self {
            Self::Output { handle, .. }
            | Self::InputFailed(handle)
            | Self::OutputFailed(handle)
            | Self::BrokerLost(handle)
            | Self::OutputDone(handle)
            | Self::Closed { handle, .. } => *handle,
            Self::Exit { handle, .. } => *handle,
            Self::SpawnReady { handle, .. } => *handle,
            Self::SpawnFailed { request, .. } => *request,
            Self::ModeChanged { handle, .. } => *handle,
        }
    }
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

struct State<T> {
    queues: HashMap<u64, VecDeque<T>>,
    ready: VecDeque<u64>,
    session_signals: HashMap<u64, Weak<SessionSignal>>,
    senders: usize,
}

struct SessionSignal {
    ready: Condvar,
    waker: Mutex<Option<Waker>>,
}

pub(crate) struct Sender<T> {
    shared: Arc<Shared<T>>,
}

pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

pub struct SessionReceiver<T> {
    session: u64,
    signal: Arc<SessionSignal>,
    shared: Arc<Shared<T>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiverClosed;

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queues: HashMap::new(),
            ready: VecDeque::new(),
            session_signals: HashMap::new(),
            senders: 1,
        }),
        ready: Condvar::new(),
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        if let Ok(mut state) = self.shared.state.lock() {
            state.senders += 1;
        }
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, session: u64, event: T) -> Result<(), T> {
        let Ok(mut state) = self.shared.state.lock() else {
            return Err(event);
        };
        if state.senders == 0 {
            return Err(event);
        }
        let queue = state.queues.entry(session).or_default();
        let was_empty = queue.is_empty();
        queue.push_back(event);
        if was_empty {
            state.ready.push_back(session);
            self.shared.ready.notify_one();
        }
        let signal = state.session_signals.get(&session).and_then(Weak::upgrade);
        drop(state);
        if let Some(signal) = signal {
            signal.ready.notify_one();
            signal.wake();
        }
        Ok(())
    }

    /// Replaces an already queued event selected by `matches`, or enqueues it.
    ///
    /// The boolean result is `true` when a new queue entry was added and
    /// `false` when an existing entry was replaced.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn send_coalesced(
        &self,
        session: u64,
        event: T,
        matches: impl Fn(&T) -> bool,
    ) -> Result<bool, T> {
        let Ok(mut state) = self.shared.state.lock() else {
            return Err(event);
        };
        if state.senders == 0 {
            return Err(event);
        }
        let queue = state.queues.entry(session).or_default();
        if let Some(pending) = queue.iter_mut().rev().find(|pending| matches(pending)) {
            *pending = event;
            return Ok(false);
        }
        let was_empty = queue.is_empty();
        queue.push_back(event);
        if was_empty {
            state.ready.push_back(session);
            self.shared.ready.notify_one();
        }
        let signal = state.session_signals.get(&session).and_then(Weak::upgrade);
        drop(state);
        if let Some(signal) = signal {
            signal.ready.notify_one();
            signal.wake();
        }
        Ok(true)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let Ok(mut state) = self.shared.state.lock() else {
            return;
        };
        state.senders -= 1;
        if state.senders == 0 {
            self.shared.ready.notify_all();
            let signals: Vec<_> = state
                .session_signals
                .values()
                .filter_map(Weak::upgrade)
                .collect();
            drop(state);
            for signal in signals {
                signal.ready.notify_all();
                signal.wake();
            }
        }
    }
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Option<(u64, T)> {
        let mut state = self.shared.state.lock().ok()?;
        loop {
            if let Some(event) = pop_next(&mut state) {
                return Some(event);
            }
            if state.senders == 0 {
                return None;
            }
            state = self.shared.ready.wait(state).ok()?;
        }
    }

    pub fn session(&self, session: u64) -> Option<SessionReceiver<T>> {
        let signal = Arc::new(SessionSignal {
            ready: Condvar::new(),
            waker: Mutex::new(None),
        });
        let mut state = self.shared.state.lock().ok()?;
        if state
            .session_signals
            .get(&session)
            .and_then(Weak::upgrade)
            .is_some()
        {
            return None;
        }
        state
            .session_signals
            .insert(session, Arc::downgrade(&signal));
        drop(state);
        Some(SessionReceiver {
            session,
            signal,
            shared: Arc::clone(&self.shared),
        })
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn try_recv(&self) -> Result<(u64, T), TryRecvError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| TryRecvError::Disconnected)?;
        pop_next(&mut state).ok_or_else(|| {
            if state.senders == 0 {
                TryRecvError::Disconnected
            } else {
                TryRecvError::Empty
            }
        })
    }
}

impl<T> SessionReceiver<T> {
    pub fn recv(&self) -> Option<T> {
        let mut state = self.shared.state.lock().ok()?;
        loop {
            if let Some(event) = pop_session(&mut state, self.session) {
                return Some(event);
            }
            if state.senders == 0 {
                return None;
            }
            state = self.signal.ready.wait(state).ok()?;
        }
    }

    pub fn try_recv(&self) -> Result<Option<T>, ReceiverClosed> {
        let mut state = self.shared.state.lock().map_err(|_| ReceiverClosed)?;
        if let Some(event) = pop_session(&mut state, self.session) {
            Ok(Some(event))
        } else if state.senders == 0 {
            Err(ReceiverClosed)
        } else {
            Ok(None)
        }
    }

    pub fn poll_recv(&self, context: &mut Context<'_>) -> Poll<Option<T>> {
        let mut state = match self.shared.state.lock() {
            Ok(state) => state,
            Err(_) => return Poll::Ready(None),
        };
        if let Some(event) = pop_session(&mut state, self.session) {
            return Poll::Ready(Some(event));
        }
        if state.senders == 0 {
            return Poll::Ready(None);
        }
        if let Ok(mut waker) = self.signal.waker.lock() {
            if waker
                .as_ref()
                .is_none_or(|waker| !waker.will_wake(context.waker()))
            {
                *waker = Some(context.waker().clone());
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for SessionReceiver<T> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            let owns_registration = state
                .session_signals
                .get(&self.session)
                .and_then(Weak::upgrade)
                .is_some_and(|signal| Arc::ptr_eq(&signal, &self.signal));
            if owns_registration {
                state.session_signals.remove(&self.session);
            }
        }
    }
}

impl SessionSignal {
    fn wake(&self) {
        if let Ok(mut waker) = self.waker.lock() {
            if let Some(waker) = waker.take() {
                waker.wake();
            }
        }
    }
}

fn pop_next<T>(state: &mut State<T>) -> Option<(u64, T)> {
    let session = state.ready.pop_front()?;
    let (event, remains_ready) = {
        let queue = state.queues.get_mut(&session)?;
        let event = queue.pop_front()?;
        (event, !queue.is_empty())
    };
    if remains_ready {
        state.ready.push_back(session);
    } else {
        state.queues.remove(&session);
    }
    Some((session, event))
}

fn pop_session<T>(state: &mut State<T>, session: u64) -> Option<T> {
    let (event, empty) = {
        let queue = state.queues.get_mut(&session)?;
        let event = queue.pop_front()?;
        (event, queue.is_empty())
    };
    if empty {
        state.queues.remove(&session);
        if let Some(index) = state.ready.iter().position(|ready| *ready == session) {
            state.ready.remove(index);
        }
    }
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::channel;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn rotates_ready_sessions_after_each_event() {
        let (sender, receiver) = channel();
        sender.send(1, "first").unwrap();
        sender.send(1, "second").unwrap();
        sender.send(2, "peer").unwrap();

        let first = receiver.recv();
        let peer = receiver.recv();
        let second = receiver.recv();

        assert_eq!(first, Some((1, "first")));
        assert_eq!(peer, Some((2, "peer")));
        assert_eq!(second, Some((1, "second")));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn coalesced_events_keep_only_the_latest_pending_state() {
        let (sender, receiver) = channel();
        sender.send(7, "output").unwrap();
        assert_eq!(
            sender.send_coalesced(7, "mode-1", |event| event.starts_with("mode")),
            Ok(true)
        );
        assert_eq!(
            sender.send_coalesced(7, "mode-2", |event| event.starts_with("mode")),
            Ok(false)
        );

        assert_eq!(receiver.recv(), Some((7, "output")));
        assert_eq!(receiver.recv(), Some((7, "mode-2")));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn coalescing_bounds_a_stalled_state_consumer() {
        let (sender, receiver) = channel();
        for state in 0..100_000 {
            let inserted = sender
                .send_coalesced(7, state, |_| true)
                .expect("event channel remains open");
            assert_eq!(inserted, state == 0);
        }

        assert_eq!(receiver.recv(), Some((7, 99_999)));
        drop(sender);
        assert_eq!(receiver.recv(), None);
    }

    #[test]
    fn session_poll_replaces_stale_waker_without_missing_delivery() {
        let (sender, receiver) = channel();
        let session = receiver.session(7).expect("register session receiver");
        let first = Arc::new(CountWake(AtomicUsize::new(0)));
        let second = Arc::new(CountWake(AtomicUsize::new(0)));
        let first_waker = Waker::from(Arc::clone(&first));
        let second_waker = Waker::from(Arc::clone(&second));

        assert_eq!(
            session.poll_recv(&mut Context::from_waker(&first_waker)),
            Poll::Pending
        );
        assert_eq!(
            session.poll_recv(&mut Context::from_waker(&second_waker)),
            Poll::Pending
        );
        sender.send(7, "ready").expect("send event");

        assert_eq!(first.0.load(Ordering::Relaxed), 0);
        assert_eq!(second.0.load(Ordering::Relaxed), 1);
        assert_eq!(
            session.poll_recv(&mut Context::from_waker(&second_waker)),
            Poll::Ready(Some("ready"))
        );
    }

    #[test]
    fn dropping_polled_session_receiver_releases_its_waker() {
        let (sender, receiver) = channel();
        let session = receiver.session(7).expect("register session receiver");
        let counter = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        assert_eq!(
            session.poll_recv(&mut Context::from_waker(&waker)),
            Poll::Pending
        );

        drop(session);
        sender.send(7, "unobserved").expect("queue remains usable");

        assert_eq!(counter.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wakes_a_blocked_receiver_when_the_last_sender_closes() {
        let (sender, receiver) = channel::<()>();
        let waiting = thread::spawn(move || receiver.recv());

        drop(sender);
        let result = waiting.join().unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn session_receiver_does_not_consume_peer_events() {
        let (sender, receiver) = channel();
        let first = receiver.session(1).unwrap();
        sender.send(2, "peer").unwrap();
        sender.send(1, "mine").unwrap();

        assert_eq!(first.recv(), Some("mine"));
        assert_eq!(receiver.recv(), Some((2, "peer")));
    }
}
