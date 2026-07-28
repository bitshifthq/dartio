use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Notice {
    Output { handle: u64, bytes: Vec<u8> },
    InputFailed(u64),
    OutputFailed(u64),
    BrokerLost(u64),
    OutputDone(u64),
    Exit(u64),
}

impl Notice {
    pub(crate) fn handle(&self) -> u64 {
        match self {
            Self::Output { handle, .. }
            | Self::InputFailed(handle)
            | Self::OutputFailed(handle)
            | Self::BrokerLost(handle)
            | Self::OutputDone(handle)
            | Self::Exit(handle) => *handle,
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
    senders: usize,
}

pub(crate) struct Sender<T> {
    shared: Arc<Shared<T>>,
}

pub(crate) struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queues: HashMap::new(),
            ready: VecDeque::new(),
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
        Ok(())
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
        }
    }
}

impl<T> Receiver<T> {
    pub(crate) fn recv(&self) -> Option<(u64, T)> {
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

    #[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::channel;
    use std::thread;

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
    fn wakes_a_blocked_receiver_when_the_last_sender_closes() {
        let (sender, receiver) = channel::<()>();
        let waiting = thread::spawn(move || receiver.recv());

        drop(sender);
        let result = waiting.join().unwrap();

        assert_eq!(result, None);
    }
}
