use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub(crate) enum Control {
    Credit { handle: u64, bytes: usize },
    Abandon { handle: u64 },
}

pub(crate) struct WakeGate {
    pending: AtomicBool,
}

impl WakeGate {
    pub(crate) const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    pub(crate) fn request(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn clear(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

pub(crate) struct ControlQueue {
    values: Mutex<VecDeque<Control>>,
}

impl ControlQueue {
    pub(crate) fn new() -> Self {
        Self {
            values: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn push(&self, value: Control) {
        self.values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(value);
    }

    pub(crate) fn swap_into(&self, target: &mut VecDeque<Control>) {
        std::mem::swap(
            &mut *self
                .values
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            target,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::WakeGate;

    #[test]
    fn wake_gate_coalesces_until_the_reactor_clears_it() {
        let gate = WakeGate::new();

        assert!(gate.request());
        assert!(!gate.request());
        assert!(!gate.request());
        gate.clear();
        assert!(gate.request());
    }
}
