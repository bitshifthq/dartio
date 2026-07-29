use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};

enum Value<T> {
    Pending,
    Complete(T),
    Closed,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

struct State<T> {
    value: Value<T>,
    senders: usize,
    receiver_open: bool,
    waker: Option<Waker>,
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
            value: Value::Pending,
            senders: 1,
            receiver_open: true,
            waker: None,
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
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, value: T) -> bool {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.receiver_open || !matches!(state.value, Value::Pending) {
            return false;
        }
        state.value = Value::Complete(value);
        let waker = state.waker.take();
        self.shared.ready.notify_all();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut senders = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        senders.senders -= 1;
        if senders.senders != 0 {
            return;
        }
        if matches!(senders.value, Value::Pending) {
            senders.value = Value::Closed;
            let waker = senders.waker.take();
            self.shared.ready.notify_all();
            drop(senders);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl<T> Receiver<T> {
    pub(crate) fn recv(self) -> Option<T> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match std::mem::replace(&mut state.value, Value::Closed) {
                Value::Complete(value) => return Some(value),
                Value::Closed => return None,
                Value::Pending => {
                    state.value = Value::Pending;
                    state = self
                        .shared
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }
    }

    pub(crate) fn try_recv(&mut self) -> Result<Option<T>, ()> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::mem::replace(&mut state.value, Value::Closed) {
            Value::Complete(value) => Ok(Some(value)),
            Value::Closed => Err(()),
            Value::Pending => {
                state.value = Value::Pending;
                Ok(None)
            }
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::mem::replace(&mut state.value, Value::Closed) {
            Value::Complete(value) => Poll::Ready(Some(value)),
            Value::Closed => Poll::Ready(None),
            Value::Pending => {
                state.value = Value::Pending;
                if state
                    .waker
                    .as_ref()
                    .is_none_or(|waker| !waker.will_wake(context.waker()))
                {
                    state.waker = Some(context.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.receiver_open = false;
        if matches!(state.value, Value::Pending) {
            state.value = Value::Closed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::channel;

    #[test]
    fn delivers_one_value_without_thread_local_channel_state() {
        let (sender, receiver) = channel();

        assert!(sender.send(7));
        assert_eq!(receiver.recv(), Some(7));
    }

    #[test]
    fn reports_sender_disconnection() {
        let (sender, receiver) = channel::<u8>();
        drop(sender);

        assert_eq!(receiver.recv(), None);
    }

    #[test]
    fn rejects_a_result_after_receiver_cancellation() {
        let (sender, receiver) = channel();
        drop(receiver);

        assert!(!sender.send(7));
    }
}
