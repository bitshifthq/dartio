use std::sync::{Arc, Condvar, Mutex};

enum Value<T> {
    Pending,
    Complete(T),
    Closed,
}

struct Shared<T> {
    value: Mutex<Value<T>>,
    ready: Condvar,
    senders: Mutex<usize>,
}

pub(crate) struct Sender<T> {
    shared: Arc<Shared<T>>,
}

pub(crate) struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        value: Mutex::new(Value::Pending),
        ready: Condvar::new(),
        senders: Mutex::new(1),
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
        *self
            .shared
            .senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, value: T) -> bool {
        let mut state = self
            .shared
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(*state, Value::Pending) {
            return false;
        }
        *state = Value::Complete(value);
        self.shared.ready.notify_one();
        true
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut senders = self
            .shared
            .senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *senders -= 1;
        if *senders != 0 {
            return;
        }
        drop(senders);
        let mut state = self
            .shared
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*state, Value::Pending) {
            *state = Value::Closed;
            self.shared.ready.notify_one();
        }
    }
}

impl<T> Receiver<T> {
    pub(crate) fn recv(self) -> Option<T> {
        let mut state = self
            .shared
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match std::mem::replace(&mut *state, Value::Closed) {
                Value::Complete(value) => return Some(value),
                Value::Closed => return None,
                Value::Pending => {
                    *state = Value::Pending;
                    state = self
                        .shared
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
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
}
