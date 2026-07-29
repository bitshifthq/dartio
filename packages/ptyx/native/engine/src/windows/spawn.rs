use std::cmp::Ordering as CompareOrdering;
use std::ffi::{c_void, OsString};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::spawn::BrokerSpawn;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Globalization::CompareStringOrdinal;
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Console::{CreatePseudoConsole, COORD};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

use super::handles::{AttributeList, OwnedHandle, OwnedPseudoConsole, PipeSecurity};

// Build 26100 made ClosePseudoConsole nonblocking. Older implementations can
// retain a process handle per session even after the child, pipes, HPCON, and
// job are closed, so they cannot satisfy the package cleanup contract.
const MINIMUM_WINDOWS_BUILD: u32 = 26_100;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
static PIPE_FALLBACK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct SpawnedSession {
    pub(crate) input: OwnedHandle,
    pub(crate) output: OwnedHandle,
    pub(crate) pseudoconsole: OwnedPseudoConsole,
    pub(crate) process: OwnedHandle,
    pub(crate) job: OwnedHandle,
    pub(crate) pid: u32,
    pub(crate) size: [u32; 4],
}

pub(crate) fn validate_windows_build() -> io::Result<()> {
    let mut version = RtlOsVersionInfo {
        size: size_of::<RtlOsVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "RtlGetVersion failed with NTSTATUS {status:#x}"
        )));
    }
    if version.major < 10 || version.build < MINIMUM_WINDOWS_BUILD {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Windows build {} is unsupported; build {MINIMUM_WINDOWS_BUILD} or newer is required",
                version.build
            ),
        ));
    }
    Ok(())
}

pub(crate) fn spawn(config: BrokerSpawn) -> io::Result<SpawnedSession> {
    config.validate()?;
    let terminal_size = conpty_size(config.rows, config.columns)?;
    let executable: Vec<u16> = config.executable.encode_wide().collect();
    if executable.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable is empty",
        ));
    }
    let arguments = config
        .arguments
        .iter()
        .map(|argument| argument.encode_wide().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let cwd = config
        .cwd
        .as_ref()
        .map(|value| value.as_os_str().encode_wide().collect::<Vec<_>>());

    let mut input_pipe = NamedPipePair::new(PipeDirection::ControllerWrites)?;
    let mut output_pipe = NamedPipePair::new(PipeDirection::ControllerReads)?;
    let mut raw_pseudoconsole = 0;
    let result = unsafe {
        CreatePseudoConsole(
            terminal_size,
            input_pipe.conpty().raw(),
            output_pipe.conpty().raw(),
            0,
            &mut raw_pseudoconsole,
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    let pseudoconsole = OwnedPseudoConsole::new(raw_pseudoconsole);
    input_pipe.close_conpty();
    output_pipe.close_conpty();

    let job = create_job()?;
    let mut attributes = AttributeList::new(2)?;
    // The pseudoconsole attribute encodes HPCON directly as lpValue. Other
    // handle-list attributes retain pointers to their values until
    // CreateProcessW consumes the attribute list.
    let mut job_attribute = [job.raw()];
    attributes.set_pseudoconsole(pseudoconsole.raw())?;
    attributes.set_job(&mut job_attribute)?;

    let application = nul_terminated_wide(&executable, "executable")?;
    let application_pointer = if uses_search_path(&executable) {
        null()
    } else {
        application.as_ptr()
    };
    let mut command_line =
        build_command_line_wide(std::iter::once(&executable).chain(arguments.iter()))?;
    let environment = config.environment.map(build_environment_wide).transpose()?;
    let cwd = cwd
        .as_deref()
        .map(|value| nul_terminated_wide(value, "working directory"))
        .transpose()?;
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Prevent inherited runner or embedding-process standard handles from
    // bypassing ConPTY. The pseudoconsole attribute supplies the child's
    // console streams; explicit invalid standard handles leave no fallback.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.raw();
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    let created = unsafe {
        CreateProcessW(
            application_pointer,
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment
                .as_ref()
                .map_or(null(), |block| block.as_ptr().cast::<c_void>()),
            cwd.as_ref().map_or(null(), |value| value.as_ptr()),
            (&startup as *const STARTUPINFOEXW).cast::<STARTUPINFOW>(),
            &mut process,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let process_handle = OwnedHandle::from_known(process.hProcess);
    let primary_thread = OwnedHandle::from_known(process.hThread);
    drop(primary_thread);
    drop(attributes);

    Ok(SpawnedSession {
        input: input_pipe.take_controller(),
        output: output_pipe.take_controller(),
        pseudoconsole,
        process: process_handle,
        job,
        pid: process.dwProcessId,
        size: [
            config.rows,
            config.columns,
            config.pixel_width,
            config.pixel_height,
        ],
    })
}

fn uses_search_path(executable: &[u16]) -> bool {
    !executable
        .iter()
        .any(|value| matches!(*value, 92 | 47 | 58))
}

fn conpty_size(rows: u32, columns: u32) -> io::Result<COORD> {
    let rows = i16::try_from(rows)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rows exceed ConPTY bounds"))?;
    let columns = i16::try_from(columns)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "columns exceed ConPTY bounds")
        })?;
    Ok(COORD {
        X: columns,
        Y: rows,
    })
}

#[cfg(test)]
pub(crate) fn build_command_line<'a>(
    arguments: impl IntoIterator<Item = &'a str>,
) -> io::Result<Vec<u16>> {
    let arguments: Vec<Vec<u16>> = arguments
        .into_iter()
        .map(|argument| argument.encode_utf16().collect())
        .collect();
    build_command_line_wide(arguments.iter())
}

fn build_command_line_wide<'a>(
    arguments: impl IntoIterator<Item = &'a Vec<u16>>,
) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for argument in arguments {
        if argument.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "argument contains NUL",
            ));
        }
        if !command.is_empty() {
            command.push(u16::from(b' '));
        }
        append_quoted_argument(&mut command, argument);
    }
    command.push(0);
    Ok(command)
}

#[cfg(test)]
pub(crate) fn build_environment_block(environment: Vec<(String, String)>) -> io::Result<Vec<u16>> {
    let mut sorted = Vec::<(Vec<u16>, String, String)>::new();
    for (key, value) in environment {
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment entry",
            ));
        }
        let wide: Vec<u16> = key.encode_utf16().collect();
        if let Some(existing) = sorted
            .iter_mut()
            .find(|(candidate, _, _)| ordinal_compare(candidate, &wide) == CompareOrdering::Equal)
        {
            *existing = (wide, key, value);
        } else {
            sorted.push((wide, key, value));
        }
    }
    let system_root_key: Vec<u16> = "SystemRoot".encode_utf16().collect();
    if !sorted
        .iter()
        .any(|(key, _, _)| ordinal_compare(key, &system_root_key) == CompareOrdering::Equal)
    {
        let system_root = std::env::var_os("SystemRoot")
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "SystemRoot is required to create a Windows environment",
                )
            })?;
        sorted.push((system_root_key, "SystemRoot".to_owned(), system_root));
    }
    sorted.sort_by(|left, right| ordinal_compare(&left.0, &right.0));

    let mut block = Vec::new();
    for (_, key, value) in sorted {
        block.extend(key.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(value.encode_utf16());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn build_environment_wide(environment: Vec<(OsString, OsString)>) -> io::Result<Vec<u16>> {
    let mut sorted = Vec::<(Vec<u16>, Vec<u16>)>::new();
    for (key, value) in environment {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        if key.is_empty() || key.contains(&(b'=' as u16)) || key.contains(&0) || value.contains(&0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment entry",
            ));
        }
        if let Some(existing) = sorted
            .iter_mut()
            .find(|(candidate, _)| ordinal_compare(candidate, &key) == CompareOrdering::Equal)
        {
            *existing = (key, value);
        } else {
            sorted.push((key, value));
        }
    }
    let system_root_key: Vec<u16> = "SystemRoot".encode_utf16().collect();
    if !sorted
        .iter()
        .any(|(key, _)| ordinal_compare(key, &system_root_key) == CompareOrdering::Equal)
    {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot is required"))?
            .encode_wide()
            .collect();
        sorted.push((system_root_key, system_root));
    }
    sorted.sort_by(|left, right| ordinal_compare(&left.0, &right.0));
    let mut block = Vec::new();
    for (key, value) in sorted {
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn ordinal_compare(left: &[u16], right: &[u16]) -> CompareOrdering {
    let result = unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    };
    match result {
        1 => CompareOrdering::Less,
        2 => CompareOrdering::Equal,
        3 => CompareOrdering::Greater,
        _ => left.cmp(right),
    }
}

fn append_quoted_argument(command: &mut Vec<u16>, argument: &[u16]) {
    let needs_quotes =
        argument.is_empty() || argument.iter().any(|unit| matches!(*unit, 9 | 32 | 34));
    if !needs_quotes {
        command.extend_from_slice(argument);
        return;
    }
    command.push(u16::from(b'"'));
    let mut backslashes = 0;
    for unit in argument {
        if *unit == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if *unit == u16::from(b'"') {
                command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2 + 1));
            } else {
                command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
            }
            backslashes = 0;
            command.push(*unit);
        }
    }
    command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
    command.push(u16::from(b'"'));
}

fn nul_terminated(value: &str) -> io::Result<Vec<u16>> {
    if value.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "string contains NUL",
        ));
    }
    Ok(value.encode_utf16().chain([0]).collect())
}

fn nul_terminated_wide(value: &[u16], label: &str) -> io::Result<Vec<u16>> {
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} contains NUL"),
        ));
    }
    let mut value = value.to_vec();
    value.push(0);
    Ok(value)
}

fn create_job() -> io::Result<OwnedHandle> {
    let job = OwnedHandle::new(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

enum PipeDirection {
    ControllerReads,
    ControllerWrites,
}

struct NamedPipePair {
    controller: Option<OwnedHandle>,
    conpty: Option<OwnedHandle>,
}

impl NamedPipePair {
    fn new(direction: PipeDirection) -> io::Result<Self> {
        let name = secure_pipe_name()?;
        let name = nul_terminated(&name)?;
        let mut security = PipeSecurity::current_owner_and_system()?;
        let (open_mode, client_access) = match direction {
            PipeDirection::ControllerReads => (
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                GENERIC_WRITE,
            ),
            PipeDirection::ControllerWrites => (
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                GENERIC_READ,
            ),
        };
        let controller = OwnedHandle::new(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                security.attributes(),
            )
        })?;
        let conpty = OwnedHandle::new(unsafe {
            CreateFileW(
                name.as_ptr(),
                client_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        })?;
        let connected = OwnedHandle::new(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        let mut overlapped: OVERLAPPED = unsafe { zeroed() };
        overlapped.hEvent = connected.raw();
        if unsafe { ConnectNamedPipe(controller.raw(), &mut overlapped) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_IO_PENDING {
                if unsafe { WaitForSingleObject(connected.raw(), INFINITE) } != WAIT_OBJECT_0 {
                    return Err(io::Error::last_os_error());
                }
                let mut transferred = 0;
                if unsafe {
                    GetOverlappedResult(controller.raw(), &overlapped, &mut transferred, 0)
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
            } else if error != ERROR_PIPE_CONNECTED {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        Ok(Self {
            controller: Some(controller),
            conpty: Some(conpty),
        })
    }

    fn conpty(&self) -> &OwnedHandle {
        self.conpty.as_ref().expect("ConPTY pipe is owned")
    }

    fn close_conpty(&mut self) {
        self.conpty.take();
    }

    fn take_controller(&mut self) -> OwnedHandle {
        self.controller.take().expect("controller pipe is owned")
    }
}

fn secure_pipe_name() -> io::Result<String> {
    let mut random = [0_u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        let fallback = PIPE_FALLBACK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        return Err(io::Error::other(format!(
            "BCryptGenRandom failed with NTSTATUS {status:#x} at sequence {fallback}"
        )));
    }
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(r"\\.\pipe\ptyx-{token}"))
}

#[repr(C)]
struct RtlOsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut RtlOsVersionInfo) -> i32;
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{self, Read, Write};
    use std::mem::zeroed;
    use std::ptr::null;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, ResizePseudoConsole, CONSOLE_SCREEN_BUFFER_INFO,
        STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    use super::{
        build_command_line, build_environment_block, conpty_size, spawn, uses_search_path,
        BrokerSpawn, OwnedHandle, COORD,
    };

    const RESIZE_PROBE_ENVIRONMENT: &str = "PTYX_CONPTY_RESIZE_PROBE_CHILD";
    const RESIZE_PROBE_TEST: &str = "windows::spawn::tests::conpty_resize_probe_child";
    const RESIZE_PROBE_BEFORE: &str = "PTYX_RESIZE_BEFORE";
    const RESIZE_PROBE_AFTER: &str = "PTYX_RESIZE_AFTER";
    const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ConsoleDimensions {
        buffer_columns: i16,
        buffer_rows: i16,
        window_columns: i16,
        window_rows: i16,
    }

    impl ConsoleDimensions {
        fn report(self, marker: &str) {
            println!(
                "{marker} dw={}x{} window={}x{}",
                self.buffer_columns, self.buffer_rows, self.window_columns, self.window_rows
            );
            io::stdout().flush().expect("flush ConPTY resize probe");
        }

        fn matches(self, rows: i16, columns: i16) -> bool {
            self.buffer_columns == columns
                && self.buffer_rows == rows
                && self.window_columns == columns
                && self.window_rows == rows
        }
    }

    #[test]
    fn conpty_resize_probe_child() {
        if std::env::var_os(RESIZE_PROBE_ENVIRONMENT).is_none() {
            return;
        }

        query_console_dimensions()
            .expect("query initial child console dimensions")
            .report(RESIZE_PROBE_BEFORE);
        let mut trigger = [0_u8; 1];
        io::stdin()
            .read_exact(&mut trigger)
            .expect("read parent resize trigger");

        let deadline = Instant::now() + PROBE_TIMEOUT;
        let observed = loop {
            let dimensions =
                query_console_dimensions().expect("query resized child console dimensions");
            if dimensions.matches(42, 120) || Instant::now() >= deadline {
                break dimensions;
            }
            thread::sleep(Duration::from_millis(25));
        };
        observed.report(RESIZE_PROBE_AFTER);
    }

    #[test]
    fn conpty_resize_updates_child_buffer_and_viewport() {
        if std::env::var_os(RESIZE_PROBE_ENVIRONMENT).is_some() {
            return;
        }

        let executable = std::env::current_exe()
            .expect("resolve native test executable")
            .into_os_string();
        let arguments = [
            OsString::from("--exact"),
            OsString::from(RESIZE_PROBE_TEST),
            OsString::from("--nocapture"),
        ];
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.push((
            OsString::from(RESIZE_PROBE_ENVIRONMENT),
            OsString::from("1"),
        ));
        let session = spawn(BrokerSpawn {
            executable,
            arguments: arguments.into(),
            environment: Some(environment),
            cwd: None,
            rows: 18,
            columns: 70,
            pixel_width: 0,
            pixel_height: 0,
            graceful_close_timeout: std::time::Duration::from_millis(250),
        })
        .expect("spawn ConPTY resize probe child");

        let mut transcript = Vec::new();
        let before =
            read_through_marker(session.output.raw(), &mut transcript, RESIZE_PROBE_BEFORE)
                .expect("read initial child console dimensions");
        eprintln!("ConPTY resize probe before: {before}");

        let result =
            unsafe { ResizePseudoConsole(session.pseudoconsole.raw(), COORD { X: 120, Y: 42 }) };
        eprintln!("ConPTY resize probe ResizePseudoConsole HRESULT: {result:#010x} ({result})");
        assert!(
            result >= 0,
            "ResizePseudoConsole failed with HRESULT {result:#010x}; transcript: {}",
            String::from_utf8_lossy(&transcript)
        );
        write_overlapped(session.input.raw(), b"\r").expect("trigger child resize query");

        let after = read_through_marker(session.output.raw(), &mut transcript, RESIZE_PROBE_AFTER)
            .expect("read resized child console dimensions");
        eprintln!("ConPTY resize probe after: {after}");
        assert!(
            after.contains("dw=120x42") && after.contains("window=120x42"),
            "successful ResizePseudoConsole did not propagate 120x42 to the child; \
             HRESULT={result:#010x}; transcript: {}",
            String::from_utf8_lossy(&transcript)
        );
        assert_eq!(
            unsafe {
                WaitForSingleObject(
                    session.process.raw(),
                    PROBE_TIMEOUT.as_millis().try_into().unwrap(),
                )
            },
            WAIT_OBJECT_0,
            "ConPTY resize probe child did not exit; transcript: {}",
            String::from_utf8_lossy(&transcript)
        );
    }

    fn query_console_dimensions() -> io::Result<ConsoleDimensions> {
        let mut information: CONSOLE_SCREEN_BUFFER_INFO = unsafe { zeroed() };
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if unsafe { GetConsoleScreenBufferInfo(output, &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ConsoleDimensions {
            buffer_columns: information.dwSize.X,
            buffer_rows: information.dwSize.Y,
            window_columns: information.srWindow.Right - information.srWindow.Left + 1,
            window_rows: information.srWindow.Bottom - information.srWindow.Top + 1,
        })
    }

    fn read_through_marker(
        handle: windows_sys::Win32::Foundation::HANDLE,
        transcript: &mut Vec<u8>,
        marker: &str,
    ) -> io::Result<String> {
        loop {
            if let Some(offset) = find_bytes(transcript, marker.as_bytes()) {
                let end = transcript[offset..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(transcript.len(), |length| offset + length);
                return Ok(String::from_utf8_lossy(&transcript[offset..end]).into_owned());
            }
            transcript.extend(read_overlapped(handle)?);
            if transcript.len() > 64 * 1024 {
                return Err(io::Error::other(
                    "ConPTY resize probe exceeded transcript limit",
                ));
            }
        }
    }

    fn find_bytes(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
        bytes
            .windows(pattern.len())
            .position(|candidate| candidate == pattern)
    }

    fn read_overlapped(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<Vec<u8>> {
        let event = OwnedHandle::new(unsafe { CreateEventW(null(), 0, 0, null()) })?;
        let mut operation: OVERLAPPED = unsafe { zeroed() };
        operation.hEvent = event.raw();
        let mut bytes = vec![0_u8; 4096];
        let mut transferred = 0;
        let started = unsafe {
            ReadFile(
                handle,
                bytes.as_mut_ptr().cast(),
                bytes.len().try_into().unwrap(),
                &mut transferred,
                &mut operation,
            )
        };
        if started == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_BROKEN_PIPE {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ConPTY resize probe output closed",
                ));
            }
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            wait_overlapped(handle, &mut operation, &mut transferred)?;
        }
        bytes.truncate(transferred as usize);
        Ok(bytes)
    }

    fn write_overlapped(
        handle: windows_sys::Win32::Foundation::HANDLE,
        bytes: &[u8],
    ) -> io::Result<()> {
        let event = OwnedHandle::new(unsafe { CreateEventW(null(), 0, 0, null()) })?;
        let mut operation: OVERLAPPED = unsafe { zeroed() };
        operation.hEvent = event.raw();
        let mut transferred = 0;
        let started = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr().cast(),
                bytes.len().try_into().unwrap(),
                &mut transferred,
                &mut operation,
            )
        };
        if started == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            wait_overlapped(handle, &mut operation, &mut transferred)?;
        }
        if transferred as usize != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "partial ConPTY resize probe trigger",
            ));
        }
        Ok(())
    }

    fn wait_overlapped(
        handle: windows_sys::Win32::Foundation::HANDLE,
        operation: &mut OVERLAPPED,
        transferred: &mut u32,
    ) -> io::Result<()> {
        let status = unsafe {
            WaitForSingleObject(
                operation.hEvent,
                PROBE_TIMEOUT.as_millis().try_into().unwrap(),
            )
        };
        if status == WAIT_TIMEOUT {
            unsafe {
                CancelIoEx(handle, operation);
                GetOverlappedResult(handle, operation, transferred, 1);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ConPTY resize probe I/O timed out",
            ));
        }
        if status != WAIT_OBJECT_0
            || unsafe { GetOverlappedResult(handle, operation, transferred, 0) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[test]
    fn command_line_preserves_empty_argument() {
        let command = build_command_line(["program.exe", ""]).unwrap();

        assert_eq!(
            String::from_utf16_lossy(&command[..command.len() - 1]),
            r#"program.exe """#
        );
    }

    #[test]
    fn command_line_escapes_quotes_and_trailing_backslashes() {
        let command = build_command_line(["program.exe", r#"quote"inside"#, r"tail\\"]).unwrap();

        assert_eq!(
            String::from_utf16_lossy(&command[..command.len() - 1]),
            r#"program.exe "quote\"inside" tail\\"#
        );
    }

    #[test]
    fn bare_executables_use_windows_search_path() {
        let wide = |value: &str| value.encode_utf16().collect::<Vec<_>>();
        assert!(uses_search_path(&wide("cmd.exe")));
        assert!(uses_search_path(&wide("tool")));
        assert!(!uses_search_path(&wide(r"C:\tools\tool.exe")));
        assert!(!uses_search_path(&wide(r".\tool.exe")));
        assert!(!uses_search_path(&wide(r"\\server\share\tool.exe")));
    }

    #[test]
    fn environment_sorts_case_insensitively() {
        let block = build_environment_block(vec![
            ("zeta".to_owned(), "2".to_owned()),
            ("Alpha".to_owned(), "1".to_owned()),
            ("SystemRoot".to_owned(), r"C:\Windows".to_owned()),
        ])
        .unwrap();

        assert_eq!(
            String::from_utf16_lossy(&block),
            "Alpha=1\0SystemRoot=C:\\Windows\0zeta=2\0\0"
        );
    }

    #[test]
    fn later_case_variant_replaces_earlier_environment_entry() {
        let block = build_environment_block(vec![
            ("Path".to_owned(), "old".to_owned()),
            ("PATH".to_owned(), "new".to_owned()),
            ("SystemRoot".to_owned(), r"C:\Windows".to_owned()),
        ])
        .unwrap();

        assert!(String::from_utf16_lossy(&block).contains("PATH=new\0"));
    }

    #[test]
    fn environment_rejects_separator_in_key() {
        let result = build_environment_block(vec![
            ("BAD=KEY".to_owned(), "value".to_owned()),
            ("SystemRoot".to_owned(), r"C:\Windows".to_owned()),
        ]);

        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn dimensions_accept_signed_coordinate_maximum() {
        let size = conpty_size(i16::MAX as u32, i16::MAX as u32).unwrap();

        assert_eq!((size.Y, size.X), (i16::MAX, i16::MAX));
    }

    #[test]
    fn dimensions_reject_zero_rows() {
        let result = conpty_size(0, 80);

        assert!(matches!(
            result,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn dimensions_reject_unsigned_coordinate_range() {
        let result = conpty_size(i16::MAX as u32 + 1, 80);

        assert!(matches!(
            result,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput
        ));
    }
}
