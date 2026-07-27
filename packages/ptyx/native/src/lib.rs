#![cfg(any(target_os = "linux", target_os = "macos", windows))]

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::RawFd;

const MAX_NOTICE_GENERATION: u32 = ((i64::MAX as u64 >> 3) >> 32) as u32;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod broker_client;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod broker_materializer;
mod ffi;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod integrated;
#[cfg(windows)]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use integrated::{IntegratedRuntime, Notice, WaitResult};
#[cfg(windows)]
pub(crate) use windows::{IntegratedRuntime, Notice, WaitResult};

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
