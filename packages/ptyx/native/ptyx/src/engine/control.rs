use std::collections::VecDeque;
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
