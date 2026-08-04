use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::RawFd;
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

#[cfg(unix)]
fn complete_wake(mut send: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match send() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // A full nonblocking descriptor already contains a wake token.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
pub(crate) fn wake_socket(fd: RawFd) -> io::Result<()> {
    let byte = [1_u8];
    complete_wake(|| {
        let result = unsafe {
            libc::send(
                fd,
                byte.as_ptr().cast(),
                byte.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if result >= 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

#[cfg(unix)]
pub(crate) fn fail_wake_socket(fd: RawFd) {
    unsafe {
        libc::shutdown(fd, libc::SHUT_RDWR);
    }
}

#[cfg(all(test, unix))]
mod wake_tests {
    use super::complete_wake;
    use std::collections::VecDeque;
    use std::io;

    #[test]
    fn interrupted_wake_is_retried() {
        let mut results =
            VecDeque::from([Err(io::Error::from(io::ErrorKind::Interrupted)), Ok(())]);

        complete_wake(|| results.pop_front().unwrap()).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn full_wake_descriptor_means_a_wake_is_already_pending() {
        complete_wake(|| Err(io::Error::from(io::ErrorKind::WouldBlock))).unwrap();
    }

    #[test]
    fn fatal_wake_failure_is_reported() {
        let error = complete_wake(|| Err(io::Error::from(io::ErrorKind::BrokenPipe)))
            .expect_err("broken wake descriptor must fail");

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}

pub(crate) struct ControlQueue {
    credits: Mutex<HashMap<u64, usize>>,
    lifecycle: Mutex<VecDeque<Control>>,
}

const MAX_CONTROL_ENTRIES: usize = 4096;
const MAX_LIFECYCLE_ENTRIES: usize = 1024;

impl ControlQueue {
    pub(crate) fn new() -> Self {
        Self {
            credits: Mutex::new(HashMap::new()),
            lifecycle: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn push(&self, value: Control) -> bool {
        match value {
            Control::Credit { handle, bytes } => {
                let mut credits = self
                    .credits
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(total) = credits.get_mut(&handle) {
                    let Some(updated) = total.checked_add(bytes) else {
                        return false;
                    };
                    *total = updated;
                    true
                } else {
                    if credits.len() >= MAX_CONTROL_ENTRIES {
                        return false;
                    }
                    credits.insert(handle, bytes);
                    true
                }
            }
            Control::Abandon { handle } => {
                let mut lifecycle = self
                    .lifecycle
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if lifecycle.iter().any(|control| {
                    matches!(control, Control::Abandon { handle: existing } if *existing == handle)
                }) {
                    return true;
                }
                if lifecycle.len() >= MAX_LIFECYCLE_ENTRIES {
                    return false;
                }
                lifecycle.push_back(Control::Abandon { handle });
                true
            }
        }
    }

    pub(crate) fn swap_into(&self, target: &mut VecDeque<Control>) {
        // Lifecycle controls are applied before coalesced credits. An abandon
        // closes the session route, so a later credit must not resurrect or
        // otherwise extend a session that is already being reclaimed.
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.append(&mut lifecycle);
        let mut credits = self
            .credits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        target.extend(
            credits
                .drain()
                .map(|(handle, bytes)| Control::Credit { handle, bytes }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Control, ControlQueue, WakeGate, MAX_CONTROL_ENTRIES};
    use std::collections::VecDeque;

    #[test]
    fn wake_gate_coalesces_until_the_reactor_clears_it() {
        let gate = WakeGate::new();

        assert!(gate.request());
        assert!(!gate.request());
        assert!(!gate.request());
        gate.clear();
        assert!(gate.request());
    }

    #[test]
    fn control_queue_coalesces_credit_without_unbounded_growth() {
        let queue = ControlQueue::new();
        for _ in 0..MAX_CONTROL_ENTRIES {
            assert!(queue.push(Control::Credit {
                handle: 1,
                bytes: 1
            }));
        }
        assert!(queue.push(Control::Credit {
            handle: 1,
            bytes: 1
        }));
        assert!(queue.push(Control::Credit {
            handle: 2,
            bytes: 1
        }));
        let mut target = VecDeque::new();
        queue.swap_into(&mut target);
        assert_eq!(target.len(), 2);
        assert!(queue.push(Control::Abandon { handle: 1 }));
    }
}
