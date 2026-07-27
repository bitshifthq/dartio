use std::cmp::Ordering as CompareOrdering;
use std::ffi::{c_void, CStr, CString};
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    WAIT_OBJECT_0,
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
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use super::handles::{AttributeList, OwnedHandle, OwnedPseudoConsole, PipeSecurity};

// ConPTY first shipped in Windows 10 version 1809. Older releases cannot
// satisfy the package contract because CreatePseudoConsole is unavailable.
const MINIMUM_WINDOWS_BUILD: u32 = 17_763;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
static PIPE_FALLBACK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct BrokerSpawn {
    pub(crate) executable: CString,
    pub(crate) arguments: Vec<CString>,
    pub(crate) environment: Option<Vec<CString>>,
    pub(crate) cwd: Option<CString>,
    pub(crate) rows: u32,
    pub(crate) columns: u32,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
}

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
    let terminal_size = conpty_size(config.rows, config.columns)?;
    let executable = decode_utf8(&config.executable, "executable")?;
    if executable.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable is empty",
        ));
    }
    let arguments = config
        .arguments
        .iter()
        .map(|argument| decode_utf8(argument, "argument"))
        .collect::<io::Result<Vec<_>>>()?;
    let cwd = config
        .cwd
        .as_ref()
        .map(|value| decode_utf8(value, "working directory"))
        .transpose()?;

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
    attributes.set_pseudoconsole(pseudoconsole.raw())?;
    attributes.set_job(job.raw())?;

    let application = nul_terminated(&executable)?;
    let application_pointer = if uses_search_path(&executable) {
        null()
    } else {
        application.as_ptr()
    };
    let mut command_line = build_command_line(
        std::iter::once(executable.as_str()).chain(arguments.iter().map(String::as_str)),
    )?;
    let environment = config
        .environment
        .map(build_environment_from_entries)
        .transpose()?;
    let cwd = cwd.as_deref().map(nul_terminated).transpose()?;
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
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

fn uses_search_path(executable: &str) -> bool {
    !executable.contains(['\\', '/', ':'])
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

pub(crate) fn build_command_line<'a>(
    arguments: impl IntoIterator<Item = &'a str>,
) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for argument in arguments {
        let argument: Vec<u16> = argument.encode_utf16().collect();
        if argument.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "argument contains NUL",
            ));
        }
        if !command.is_empty() {
            command.push(u16::from(b' '));
        }
        append_quoted_argument(&mut command, &argument);
    }
    command.push(0);
    Ok(command)
}

pub(crate) fn build_environment_from_entries(entries: Vec<CString>) -> io::Result<Vec<u16>> {
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = decode_utf8(&entry, "environment")?;
        let Some((key, value)) = entry.split_once('=') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment entry has no separator",
            ));
        };
        values.push((key.to_owned(), value.to_owned()));
    }
    build_environment_block(values)
}

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

fn decode_utf8(value: &CStr, label: &str) -> io::Result<String> {
    value.to_str().map(str::to_owned).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} is not valid UTF-8"),
        )
    })
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
    use super::{build_command_line, build_environment_block, conpty_size, uses_search_path};

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
        assert!(uses_search_path("cmd.exe"));
        assert!(uses_search_path("tool"));
        assert!(!uses_search_path(r"C:\tools\tool.exe"));
        assert!(!uses_search_path(r".\tool.exe"));
        assert!(!uses_search_path(r"\\server\share\tool.exe"));
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
