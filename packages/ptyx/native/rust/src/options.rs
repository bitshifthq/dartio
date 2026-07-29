use crate::InvalidSize;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_INPUT_CAPACITY: usize = 1024 * 1024;
const DEFAULT_OUTPUT_CAPACITY: usize = 256 * 1024;
const DEFAULT_GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_CELL_DIMENSION: u16 = i16::MAX as u16;

pub(crate) struct SpawnParts {
    pub(crate) executable: OsString,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Option<Vec<(OsString, OsString)>>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) initial_size: Size,
    pub(crate) input_capacity: usize,
    pub(crate) output_capacity: usize,
    pub(crate) graceful_close_timeout: Duration,
}

/// Portable cell and optional pixel dimensions of a pseudo terminal.
///
/// Rows and columns are limited to `32767` on every platform so a validated
/// value has identical semantics on Unix PTYs and Windows ConPTY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    rows: u16,
    columns: u16,
    pixel_width: u16,
    pixel_height: u16,
}

impl Size {
    /// Creates a size with cell dimensions and no pixel dimensions.
    ///
    /// # Panics
    ///
    /// Panics when either cell dimension is zero. Use [`Size::try_new`] when
    /// dimensions are not already validated.
    #[must_use]
    pub const fn new(rows: u16, columns: u16) -> Self {
        assert!(
            rows != 0
                && columns != 0
                && rows <= MAX_CELL_DIMENSION
                && columns <= MAX_CELL_DIMENSION,
            "terminal rows and columns must be in 1..=32767"
        );
        Self {
            rows,
            columns,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Validates and creates a size with cell dimensions.
    pub const fn try_new(rows: u16, columns: u16) -> Result<Self, InvalidSize> {
        if rows == 0 || columns == 0 || rows > MAX_CELL_DIMENSION || columns > MAX_CELL_DIMENSION {
            return Err(InvalidSize);
        }
        Ok(Self::new(rows, columns))
    }

    /// Adds optional pixel dimensions.
    #[must_use]
    pub const fn with_pixels(mut self, width: u16, height: u16) -> Self {
        self.pixel_width = width;
        self.pixel_height = height;
        self
    }

    /// Number of terminal rows.
    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }

    /// Number of terminal columns.
    #[must_use]
    pub const fn columns(self) -> u16 {
        self.columns
    }

    /// Optional terminal pixel width, or zero when unspecified.
    #[must_use]
    pub const fn pixel_width(self) -> u16 {
        self.pixel_width
    }

    /// Optional terminal pixel height, or zero when unspecified.
    #[must_use]
    pub const fn pixel_height(self) -> u16 {
        self.pixel_height
    }

    pub(crate) const fn native(self) -> [u32; 4] {
        [
            self.rows as u32,
            self.columns as u32,
            self.pixel_width as u32,
            self.pixel_height as u32,
        ]
    }

    pub(crate) fn from_native(size: [u32; 4]) -> Option<Self> {
        Some(Self {
            rows: u16::try_from(size[0]).ok().filter(|value| *value != 0)?,
            columns: u16::try_from(size[1]).ok().filter(|value| *value != 0)?,
            pixel_width: u16::try_from(size[2]).ok()?,
            pixel_height: u16::try_from(size[3]).ok()?,
        })
    }
}

/// Owned configuration for starting one pseudo-terminal child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnOptions {
    executable: OsString,
    arguments: Vec<OsString>,
    environment: Option<Vec<(OsString, OsString)>>,
    cwd: Option<PathBuf>,
    initial_size: Size,
    input_capacity: usize,
    output_capacity: usize,
    graceful_close_timeout: Duration,
}

impl SpawnOptions {
    /// Creates options for an executable without invoking a shell.
    #[must_use]
    pub fn new(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            environment: None,
            cwd: None,
            initial_size: Size::new(24, 80),
            input_capacity: DEFAULT_INPUT_CAPACITY,
            output_capacity: DEFAULT_OUTPUT_CAPACITY,
            graceful_close_timeout: DEFAULT_GRACEFUL_CLOSE_TIMEOUT,
        }
    }

    /// Appends one argument after the executable.
    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Replaces the complete argument list.
    #[must_use]
    pub fn with_arguments(
        mut self,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the child environment instead of inheriting it.
    #[must_use]
    pub fn environment(
        mut self,
        environment: impl IntoIterator<Item = (impl Into<OsString>, impl Into<OsString>)>,
    ) -> Self {
        self.environment = Some(
            environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        );
        self
    }

    /// Uses an empty child environment.
    #[must_use]
    pub fn clear_environment(mut self) -> Self {
        self.environment = Some(Vec::new());
        self
    }

    /// Selects the child working directory.
    #[must_use]
    pub fn current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.cwd = Some(directory.into());
        self
    }

    /// Selects the initial terminal size.
    #[must_use]
    pub fn size(mut self, size: Size) -> Self {
        self.initial_size = size;
        self
    }

    /// Sets the maximum accepted input bytes retained by this session.
    #[must_use]
    pub fn input_capacity(mut self, bytes: usize) -> Self {
        self.input_capacity = bytes;
        self
    }

    /// Sets the maximum output bytes retained by this session.
    #[must_use]
    pub fn output_capacity(mut self, bytes: usize) -> Self {
        self.output_capacity = bytes;
        self
    }

    /// Sets the grace period before Unix close escalates to forced cleanup.
    ///
    /// Windows terminates the owned job immediately.
    #[must_use]
    pub fn graceful_close_timeout(mut self, timeout: Duration) -> Self {
        self.graceful_close_timeout = timeout;
        self
    }

    /// Executable passed directly to the platform process facility.
    #[must_use]
    pub fn executable(&self) -> &OsStr {
        &self.executable
    }

    /// Arguments following the executable.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Replacement environment, or `None` when the parent is inherited.
    #[must_use]
    pub fn environment_ref(&self) -> Option<&[(OsString, OsString)]> {
        self.environment.as_deref()
    }

    /// Child working directory, or `None` to use the process default.
    #[must_use]
    pub fn current_directory(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Initial terminal size.
    #[must_use]
    pub const fn initial_size(&self) -> Size {
        self.initial_size
    }

    pub(crate) fn into_parts(self) -> SpawnParts {
        SpawnParts {
            executable: self.executable,
            arguments: self.arguments,
            environment: self.environment,
            cwd: self.cwd,
            initial_size: self.initial_size,
            input_capacity: self.input_capacity,
            output_capacity: self.output_capacity,
            graceful_close_timeout: self.graceful_close_timeout,
        }
    }
}
