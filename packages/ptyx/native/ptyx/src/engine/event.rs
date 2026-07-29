use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
#[cfg(feature = "__private_adapter")]
use std::io;

#[cfg(feature = "__private_adapter")]
use crate::error::FailureKind;
use crate::error::OperationError;

#[cfg(feature = "__private_adapter")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub kind: FailureKind,
    pub native_code: Option<i32>,
}

#[cfg(feature = "__private_adapter")]
impl From<&io::Error> for Failure {
    fn from(error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
                FailureKind::InvalidArgument
            }
            io::ErrorKind::WouldBlock => FailureKind::Backpressure,
            io::ErrorKind::Unsupported => FailureKind::Unsupported,
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted => {
                FailureKind::InfrastructureLost
            }
            _ => FailureKind::NativeFailure,
        };
        Self {
            kind,
            native_code: error.raw_os_error(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CloseResult {
    pub input_failure: Option<OperationError>,
    pub output_failure: Option<OperationError>,
    pub cleanup_failure: Option<OperationError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    Output {
        handle: u64,
        bytes: Bytes,
    },
    InputFailed {
        handle: u64,
        failure: OperationError,
    },
    OutputFailed {
        handle: u64,
        failure: OperationError,
    },
    ExitFailed {
        handle: u64,
        failure: OperationError,
    },
    BrokerLost {
        handle: u64,
        failure: OperationError,
    },
    OutputDone(u64),
    Exit {
        handle: u64,
        status: i64,
    },
    Closed {
        handle: u64,
        result: CloseResult,
    },
    #[cfg(feature = "__private_adapter")]
    SpawnReady {
        request: u64,
        handle: u64,
    },
    #[cfg(feature = "__private_adapter")]
    SpawnFailed {
        request: u64,
        failure: Failure,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ModeChanged {
        handle: u64,
        modes: [bool; 3],
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ModeFailed {
        handle: u64,
        failure: OperationError,
    },
}

impl Notice {
    pub fn handle(&self) -> u64 {
        match self {
            Self::Output { handle, .. }
            | Self::InputFailed { handle, .. }
            | Self::OutputFailed { handle, .. }
            | Self::ExitFailed { handle, .. }
            | Self::BrokerLost { handle, .. }
            | Self::OutputDone(handle)
            | Self::Closed { handle, .. } => *handle,
            Self::Exit { handle, .. } => *handle,
            #[cfg(feature = "__private_adapter")]
            Self::SpawnReady { handle, .. } => *handle,
            #[cfg(feature = "__private_adapter")]
            Self::SpawnFailed { request, .. } => *request,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::ModeChanged { handle, .. } | Self::ModeFailed { handle, .. } => *handle,
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
    closed_sessions: HashSet<u64>,
    retired_sessions: HashSet<u64>,
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
            closed_sessions: HashSet::new(),
            retired_sessions: HashSet::new(),
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
    /// Forgets a closed route after its producer can no longer send events.
    pub(crate) fn retire_session(&self, session: u64) {
        let signal = if let Ok(mut state) = self.shared.state.lock() {
            if state.closed_sessions.remove(&session) {
                None
            } else {
                let signal = state.session_signals.get(&session).and_then(Weak::upgrade);
                if signal.is_some() {
                    state.retired_sessions.insert(session);
                } else {
                    state.session_signals.remove(&session);
                }
                signal
            }
        } else {
            None
        };
        if let Some(signal) = signal {
            signal.ready.notify_all();
            signal.wake();
        }
    }

    pub(crate) fn send(&self, session: u64, event: T) -> Result<(), T> {
        let Ok(mut state) = self.shared.state.lock() else {
            return Err(event);
        };
        if state.senders == 0 {
            return Err(event);
        }
        if state.closed_sessions.contains(&session) || state.retired_sessions.contains(&session) {
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
        if state.closed_sessions.contains(&session) || state.retired_sessions.contains(&session) {
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
    #[cfg(any(feature = "__private_adapter", test))]
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
        if state.closed_sessions.contains(&session) {
            return None;
        }
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
    /// Atomically closes this session route and returns all queued events.
    ///
    /// Once this method acquires the channel lock, later sends for this
    /// session fail rather than creating an orphaned queue.
    pub fn close(&self) -> Vec<T> {
        let Ok(mut state) = self.shared.state.lock() else {
            return Vec::new();
        };
        close_session(&mut state, self.session, &self.signal)
    }

    pub fn recv(&self) -> Option<T> {
        let mut state = self.shared.state.lock().ok()?;
        loop {
            if let Some(event) = pop_session(&mut state, self.session) {
                return Some(event);
            }
            if state.retired_sessions.contains(&self.session) {
                return None;
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
        } else if state.retired_sessions.contains(&self.session) || state.senders == 0 {
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
        if state.retired_sessions.contains(&self.session) {
            return Poll::Ready(None);
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
        drop(self.close());
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

#[cfg(any(feature = "__private_adapter", test))]
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

fn close_session<T>(state: &mut State<T>, session: u64, signal: &Arc<SessionSignal>) -> Vec<T> {
    let owns_registration = state
        .session_signals
        .get(&session)
        .and_then(Weak::upgrade)
        .is_some_and(|registered| Arc::ptr_eq(&registered, signal));
    if !owns_registration {
        return Vec::new();
    }

    state.session_signals.remove(&session);
    if !state.retired_sessions.remove(&session) {
        state.closed_sessions.insert(session);
    }
    state.ready.retain(|ready| *ready != session);
    state
        .queues
        .remove(&session)
        .map_or_else(Vec::new, VecDeque::into)
}

#[cfg(test)]
mod tests {
    use super::channel;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
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
        assert_eq!(sender.send(7, "unobserved"), Err("unobserved"));

        assert_eq!(counter.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dropping_session_receiver_purges_queued_events_and_rejects_later_sends() {
        let (sender, receiver) = channel();
        let session = receiver.session(7).expect("register session receiver");
        sender.send(7, "first").expect("queue event");
        sender.send(7, "second").expect("queue event");

        drop(session);

        assert_eq!(sender.send(7, "late"), Err("late"));
        sender.send(8, "peer").expect("peer route remains open");
        assert_eq!(receiver.recv(), Some((8, "peer")));
    }

    #[test]
    fn close_racing_send_cannot_leave_an_orphaned_session_queue() {
        let (sender, receiver) = channel();
        let session = receiver.session(7).expect("register session receiver");
        let gate = Arc::new(Barrier::new(2));
        let sending = thread::spawn({
            let gate = Arc::clone(&gate);
            move || {
                gate.wait();
                for event in 0.. {
                    if sender.send(7, event).is_err() {
                        return event;
                    }
                }
                unreachable!("the receiver close eventually rejects the sender")
            }
        });

        gate.wait();
        drop(session);
        let _rejected_at = sending.join().expect("sender thread");
        assert_eq!(receiver.recv(), None);
    }

    #[test]
    fn retired_session_routes_do_not_accumulate_during_churn() {
        let (sender, receiver) = channel();
        for session_id in 1..=10_000 {
            let session = receiver
                .session(session_id)
                .expect("register fresh session receiver");
            sender.send(session_id, session_id).expect("queue event");
            drop(session);
            sender.retire_session(session_id);
        }

        let state = sender.shared.state.lock().expect("channel state");
        assert!(state.queues.is_empty());
        assert!(state.ready.is_empty());
        assert!(state.session_signals.is_empty());
        assert!(state.closed_sessions.is_empty());
        assert!(state.retired_sessions.is_empty());
    }

    #[test]
    fn producer_retirement_preserves_queued_events_until_receiver_close() {
        let (sender, receiver) = channel();
        let session = receiver.session(7).expect("register session receiver");
        sender.send(7, "terminal").expect("queue terminal event");

        sender.retire_session(7);

        assert_eq!(sender.send(7, "late"), Err("late"));
        assert_eq!(session.recv(), Some("terminal"));
        assert_eq!(session.recv(), None);
        drop(session);
        let state = sender.shared.state.lock().expect("channel state");
        assert!(!state.retired_sessions.contains(&7));
        assert!(!state.closed_sessions.contains(&7));
    }

    #[test]
    fn producer_retirement_wakes_and_terminates_a_pending_session_receiver() {
        let (sender, receiver) = channel::<()>();
        let session = receiver.session(7).expect("register session receiver");
        let wake = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&wake));
        let mut context = Context::from_waker(&waker);
        assert_eq!(session.poll_recv(&mut context), Poll::Pending);

        sender.retire_session(7);

        assert_eq!(wake.0.load(Ordering::Relaxed), 1);
        assert_eq!(session.poll_recv(&mut context), Poll::Ready(None));
        assert_eq!(session.try_recv(), Err(super::ReceiverClosed));
    }

    #[test]
    fn global_receiver_retirement_does_not_accumulate_route_markers() {
        let (sender, receiver) = channel();
        for session_id in 1..=10_000 {
            sender
                .send(session_id, session_id)
                .expect("queue terminal event");
            sender.retire_session(session_id);
        }

        let state = sender.shared.state.lock().expect("channel state");
        assert!(state.closed_sessions.is_empty());
        assert!(state.retired_sessions.is_empty());
        drop(state);

        for session_id in 1..=10_000 {
            assert_eq!(receiver.recv(), Some((session_id, session_id)));
        }
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
