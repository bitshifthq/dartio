use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const BROKER: &[u8] = include_bytes!(env!("PTYX_BROKER_BINARY"));
const BROKER_ID: &str = env!("PTYX_BROKER_ID");

pub(crate) fn broker_path() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("PTYI_BROKER") {
        let path = PathBuf::from(path);
        validate_override(&path)?;
        return Ok(path);
    }

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
            Ok(lock_file) => {
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

fn install(root: &Path, destination: &Path) -> io::Result<()> {
    if validate_embedded(destination).is_ok() {
        return Ok(());
    }
    let temporary = root.join(format!(".broker-{BROKER_ID}.{}.tmp", std::process::id()));
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
}

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
    let mut bytes = Vec::with_capacity(BROKER.len());
    file.read_to_end(&mut bytes)?;
    if bytes != BROKER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker cache entry content rejected",
        ));
    }
    Ok(())
}
