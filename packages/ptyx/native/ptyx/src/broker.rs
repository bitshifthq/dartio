use std::env;
use std::fs;
#[cfg(not(ptyx_no_embedded_broker))]
use std::fs::{File, OpenOptions};
#[cfg(ptyx_no_embedded_broker)]
use std::io;
#[cfg(not(ptyx_no_embedded_broker))]
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
#[cfg(not(ptyx_no_embedded_broker))]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(not(ptyx_no_embedded_broker))]
use std::thread;
#[cfg(not(ptyx_no_embedded_broker))]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(not(ptyx_no_embedded_broker))]
const BROKER: &[u8] = include_bytes!(env!("PTYX_BROKER_BINARY"));
#[cfg(not(ptyx_no_embedded_broker))]
const BROKER_ID: &str = env!("PTYX_BROKER_ID");

/// Resolves or materializes the target-matched private Unix broker.
///
/// This is exposed only through the crate's build-internal adapter feature.
pub fn broker_path() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("PTYX_BROKER") {
        let path = PathBuf::from(path);
        validate_override(&path)?;
        return Ok(path);
    }

    packaged_broker_path()
}

#[cfg(ptyx_no_embedded_broker)]
fn packaged_broker_path() -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "this source build contains no target-matched broker; configure \
         RuntimeBuilder::broker_path or PTYX_BROKER",
    ))
}

#[cfg(not(ptyx_no_embedded_broker))]
fn packaged_broker_path() -> io::Result<PathBuf> {
    let root = env::temp_dir().join(format!("ptyx-{}", unsafe { libc::geteuid() }));
    ensure_private_directory(&root)?;
    let destination = root.join(format!("broker-{BROKER_ID}"));
    if validate_embedded(&destination).is_ok() {
        return Ok(destination);
    }

    let lock = root.join(format!("broker-{BROKER_ID}.lock"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock)
        {
            Ok(mut lock_file) => {
                writeln!(lock_file, "{}", std::process::id())?;
                lock_file.sync_all()?;
                let result = install(&root, &destination);
                drop(lock_file);
                let _ = fs::remove_file(&lock);
                result?;
                validate_embedded(&destination)?;
                return Ok(destination);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if validate_embedded(&destination).is_ok() {
                    return Ok(destination);
                }
                reclaim_stale_lock(&lock)?;
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for broker materialization lock",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

fn validate_override(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configured broker is not an executable regular file",
        ));
    }
    Ok(())
}

#[cfg(not(ptyx_no_embedded_broker))]
fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "broker cache directory ownership or mode rejected",
        ));
    }
    Ok(())
}

#[cfg(not(ptyx_no_embedded_broker))]
fn install(root: &Path, destination: &Path) -> io::Result<()> {
    if validate_embedded(destination).is_ok() {
        return Ok(());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = root.join(format!(
        ".broker-{BROKER_ID}.{}.{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temporary)?;
        file.write_all(BROKER)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)?;
        File::open(root)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(ptyx_no_embedded_broker))]
fn reclaim_stale_lock(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "broker materialization lock ownership or mode rejected",
        ));
    }
    let pid = fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok());
    let owner_is_gone = pid.is_none_or(|pid| {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return false;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    });
    let too_old = metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > Duration::from_secs(30));
    if owner_is_gone || too_old {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(ptyx_no_embedded_broker))]
fn validate_embedded(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
        || metadata.len() != BROKER.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "broker cache entry ownership, mode, or size rejected",
        ));
    }
    let mut file = File::open(path)?;
    let mut offset = 0;
    let mut buffer = [0_u8; 16 * 1024];
    while offset < BROKER.len() {
        let count = file.read(&mut buffer)?;
        if count == 0 || buffer[..count] != BROKER[offset..offset + count] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker cache entry content rejected",
            ));
        }
        offset += count;
    }
    Ok(())
}
