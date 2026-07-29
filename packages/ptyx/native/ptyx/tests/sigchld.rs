#![cfg(target_os = "macos")]

use ptyx::{Runtime, Size, SpawnOptions};
use std::io;
use std::path::PathBuf;

struct SignalDisposition(libc::sigaction);

impl Drop for SignalDisposition {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGCHLD, &self.0, std::ptr::null_mut());
        }
    }
}

#[test]
fn broker_owns_children_when_the_host_ignores_sigchld() -> io::Result<()> {
    let mut ignored: libc::sigaction = unsafe { std::mem::zeroed() };
    ignored.sa_sigaction = libc::SIG_IGN;
    unsafe {
        libc::sigemptyset(&mut ignored.sa_mask);
    }
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, &ignored, &mut previous) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let _restore = SignalDisposition(previous);

    let runtime = Runtime::builder()
        .broker_path(broker_path())
        .build()
        .expect("create runtime while the host ignores SIGCHLD");
    let spawned = runtime
        .spawn_blocking(
            SpawnOptions::new("/bin/sh")
                .with_arguments(["-c", "exit 0"])
                .size(Size::new(24, 80)),
        )
        .expect("spawn a broker-owned child");
    let (session, _events) = spawned.into_parts();

    session
        .close()
        .expect("start close")
        .wait()
        .expect("broker must reap and release the child");
    Ok(())
}

fn broker_path() -> PathBuf {
    std::env::var_os("PTYX_BROKER_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release/ptyx-broker")
        })
}
