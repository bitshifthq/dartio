#![cfg(any(target_os = "linux", target_os = "macos", windows))]

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::RawFd;

const MAX_NOTICE_GENERATION: u32 = ((i64::MAX as u64 >> 3) >> 32) as u32;

#[derive(Debug)]
pub(crate) enum WriteRejection {
    Backpressure(bytes::Bytes),
    Closed {
        bytes: bytes::Bytes,
        failure: Option<crate::error::OperationError>,
    },
    Infrastructure {
        bytes: bytes::Bytes,
        failure: crate::error::OperationError,
    },
}

#[cfg(feature = "__private_adapter")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyWriteResult {
    Accepted,
    Backpressure,
    Closed(Option<crate::error::OperationError>),
    Infrastructure(crate::error::OperationError),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod broker_client;
mod completion;
mod control;
mod event;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod integrated;
mod oneshot;
mod session;
mod spawn;

#[cfg(any(
    feature = "__private_adapter",
    all(test, any(target_os = "linux", target_os = "macos"))
))]
pub const WRITE_INFRASTRUCTURE_FAILURE: i64 = -2;

/// Reactor-atomic session metadata shared by native adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    /// Native direct-child process identifier.
    pub pid: i64,
    /// Rows, columns, pixel width, and pixel height.
    pub size: [u32; 4],
    /// Canonical, echo, and signal modes when supported.
    pub mode: Option<[bool; 3]>,
    /// Platform-native controller terminal name bytes when supported.
    pub tty_name: Option<Vec<u8>>,
}
#[cfg(windows)]
mod windows;

pub use completion::Completion;
#[cfg(feature = "__private_adapter")]
pub use event::Failure;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use event::Receiver as EventReceiver;
#[cfg(windows)]
pub use event::ReceiverClosed;
pub use event::{CloseResult, Notice, SessionReceiver};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use integrated::IntegratedRuntime;
pub use spawn::BrokerSpawn;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type RuntimeEvents = EventReceiver<Notice>;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type SessionEvents = SessionReceiver<Notice>;
#[cfg(windows)]
pub use windows::{
    IntegratedRuntime, NoticeReceiver as RuntimeEvents, SessionNoticeReceiver as SessionEvents,
};

#[cfg(all(fuzzing, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn fuzz_broker_protocol_frame(bytes: &[u8]) {
    broker_client::fuzz_protocol_frame(bytes);
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub(crate) struct GenerationRegistry<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> GenerationRegistry<T> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub(crate) fn insert(&mut self, value: T) -> u64 {
        let index = self.free.pop().unwrap_or_else(|| {
            self.slots.push(Slot {
                generation: 1,
                value: None,
            });
            self.slots.len() - 1
        });
        let slot = &mut self.slots[index];
        slot.value = Some(value);
        ((slot.generation as u64) << 32) | (index as u64 + 1)
    }

    pub(crate) fn get(&self, handle: u64) -> Option<&T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub(crate) fn get_mut(&mut self, handle: u64) -> Option<&mut T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        (slot.generation == generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }

    pub(crate) fn handles(&self) -> Vec<u64> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.value
                    .as_ref()
                    .map(|_| ((slot.generation as u64) << 32) | (index as u64 + 1))
            })
            .collect()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u64, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value
                .as_ref()
                .map(|value| (((slot.generation as u64) << 32) | (index as u64 + 1), value))
        })
    }

    pub(crate) fn remove(&mut self, handle: u64) -> Option<T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        if slot.generation < MAX_NOTICE_GENERATION {
            slot.generation += 1;
            self.free.push(index);
        }
        Some(value)
    }
}

fn decode_handle(handle: u64) -> Option<(usize, u32)> {
    let index = (handle as u32).checked_sub(1)? as usize;
    let generation = (handle >> 32) as u32;
    (generation != 0).then_some((index, generation))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn dup_cloexec(fd: RawFd) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;

    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(test)]
mod tests {
    use super::{GenerationRegistry, MAX_NOTICE_GENERATION};

    #[test]
    fn generation_registry_model_rejects_every_retired_handle() {
        let mut registry = GenerationRegistry::new();
        let mut live = Vec::new();
        let mut retired = Vec::new();
        let mut state = 0xd1b5_4a32_8f07_c6e9_u64;
        let operations = option_env!("PTYX_MODEL_ITERATIONS")
            .map(|value| value.parse::<u64>().expect("valid model iteration count"))
            .unwrap_or(20_000);

        for value in 1..=operations {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if !live.is_empty() && (live.len() >= 64 || state & 3 == 0) {
                let index = state as usize % live.len();
                let (handle, expected) = live.swap_remove(index);
                assert_eq!(registry.remove(handle), Some(expected));
                assert!(registry.get(handle).is_none());
                assert!(registry.get_mut(handle).is_none());
                assert_eq!(registry.remove(handle), None);
                retired.push(handle);
            } else {
                let handle = registry.insert(value);
                assert_eq!(registry.get(handle), Some(&value));
                live.push((handle, value));
            }

            for &(handle, expected) in &live {
                assert_eq!(registry.get(handle), Some(&expected));
            }
            for &handle in retired.iter().rev().take(64) {
                assert!(registry.get(handle).is_none());
            }
        }
    }

    #[test]
    fn exhausted_generations_retire_the_slot_instead_of_wrapping() {
        let mut registry = GenerationRegistry::new();
        let handle = registry.insert(1_u8);
        registry.slots[0].generation = MAX_NOTICE_GENERATION;
        let exhausted = (u64::from(MAX_NOTICE_GENERATION) << 32) | (handle as u32 as u64);

        assert_eq!(registry.remove(exhausted), Some(1));
        let replacement = registry.insert(2);

        assert_eq!(replacement as u32, 2);
        assert!(registry.get(exhausted).is_none());
        assert_eq!(registry.get(replacement), Some(&2));
    }
}
