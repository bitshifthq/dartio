use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

const MAX_ARGUMENTS: usize = 256;
const MAX_ENVIRONMENT: usize = 4096;
const MAX_SPAWN_PAYLOAD: usize = 64 * 1024;
const MAX_CELL_DIMENSION: u32 = i16::MAX as u32;
const MAX_PIXEL_DIMENSION: u32 = u16::MAX as u32;

/// Fully owned process configuration accepted by the native engine.
#[derive(Clone, Debug)]
pub struct BrokerSpawn {
    pub executable: OsString,
    pub arguments: Vec<OsString>,
    pub environment: Option<Vec<(OsString, OsString)>>,
    pub cwd: Option<PathBuf>,
    pub rows: u32,
    pub columns: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub graceful_close_timeout: Duration,
}

impl BrokerSpawn {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.arguments.len() > MAX_ARGUMENTS {
            return invalid("spawn argument count exceeds 256");
        }
        if self
            .environment
            .as_ref()
            .is_some_and(|environment| environment.len() > MAX_ENVIRONMENT)
        {
            return invalid("spawn environment count exceeds 4096");
        }
        if self.rows == 0
            || self.columns == 0
            || self.rows > MAX_CELL_DIMENSION
            || self.columns > MAX_CELL_DIMENSION
            || self.pixel_width > MAX_PIXEL_DIMENSION
            || self.pixel_height > MAX_PIXEL_DIMENSION
        {
            return invalid("terminal dimensions are outside native bounds");
        }
        if self.graceful_close_timeout > Duration::from_secs(60) {
            return invalid("graceful close timeout exceeds 60 seconds");
        }
        if native_units(&self.executable) == 0 || contains_nul(&self.executable) {
            return invalid("executable is empty or contains NUL");
        }
        if self.arguments.iter().any(|value| contains_nul(value))
            || self
                .cwd
                .as_ref()
                .is_some_and(|value| contains_nul(value.as_os_str()))
            || self.environment.as_ref().is_some_and(|environment| {
                environment.iter().any(|(key, value)| {
                    native_units(key) == 0
                        || contains_equals(key)
                        || contains_nul(key)
                        || contains_nul(value)
                })
            })
        {
            return invalid("spawn value contains an invalid NUL or environment key");
        }
        if self.encoded_payload_size()? > MAX_SPAWN_PAYLOAD {
            return invalid("encoded process spawn payload exceeds 64 KiB");
        }
        Ok(())
    }

    fn encoded_payload_size(&self) -> io::Result<usize> {
        let environment_count = self.environment.as_ref().map_or(0, Vec::len);
        let mut size = 36_usize
            .checked_add(
                4_usize
                    .checked_mul(1 + self.arguments.len() + environment_count)
                    .ok_or_else(payload_overflow)?,
            )
            .ok_or_else(payload_overflow)?;
        for value in std::iter::once(&self.executable).chain(self.arguments.iter()) {
            size = size
                .checked_add(native_units(value))
                .ok_or_else(payload_overflow)?;
        }
        if let Some(environment) = &self.environment {
            for (key, value) in environment {
                size = size
                    .checked_add(native_units(key))
                    .and_then(|value_size| value_size.checked_add(native_units(value)))
                    .and_then(|value_size| value_size.checked_add(1))
                    .ok_or_else(payload_overflow)?;
            }
        }
        if let Some(cwd) = &self.cwd {
            size = size
                .checked_add(native_units(cwd.as_os_str()))
                .ok_or_else(payload_overflow)?;
        }
        Ok(size)
    }
}

fn invalid<T>(message: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn payload_overflow() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "encoded process spawn payload size overflowed",
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_units(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().len()
}

#[cfg(windows)]
fn native_units(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().count().saturating_mul(2)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().contains(&0)
}

#[cfg(windows)]
fn contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().any(|unit| unit == 0)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn contains_equals(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().contains(&b'=')
}

#[cfg(windows)]
fn contains_equals(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().any(|unit| unit == u16::from(b'='))
}

#[cfg(test)]
mod tests {
    use super::BrokerSpawn;
    use std::ffi::OsString;
    use std::time::Duration;

    fn valid() -> BrokerSpawn {
        BrokerSpawn {
            executable: OsString::from("echo"),
            arguments: Vec::new(),
            environment: None,
            cwd: None,
            rows: 24,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
            graceful_close_timeout: Duration::from_millis(250),
        }
    }

    #[test]
    fn rejects_platform_inconsistent_cell_dimensions() {
        let mut config = valid();
        config.columns = i16::MAX as u32 + 1;
        assert_eq!(
            config.validate().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn rejects_argument_node_amplification() {
        let mut config = valid();
        config.arguments = vec![OsString::new(); 257];
        assert_eq!(
            config.validate().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn rejects_oversized_encoded_payload() {
        let mut config = valid();
        config.arguments = vec![OsString::from("x".repeat(64 * 1024))];
        assert_eq!(
            config.validate().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
