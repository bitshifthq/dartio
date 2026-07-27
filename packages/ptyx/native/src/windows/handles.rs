use std::ffi::c_void;
use std::io;
use std::mem::{size_of, zeroed};
use std::pin::Pin;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_IO_PENDING, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Console::{ClosePseudoConsole, HPCON};
use windows_sys::Win32::System::Threading::{
    CreateEventW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    RegisterWaitForSingleObject, UnregisterWaitEx, UpdateProcThreadAttribute, WaitForSingleObject,
    INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, WT_EXECUTEONLYONCE,
};
use windows_sys::Win32::System::IO::{PostQueuedCompletionStatus, OVERLAPPED};

pub(crate) struct OwnedHandle(HANDLE);

// Windows HANDLE values may cross threads. This wrapper has one owner and
// closes the value exactly once.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub(crate) fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    pub(crate) fn from_known(handle: HANDLE) -> Self {
        debug_assert!(!handle.is_null() && handle != INVALID_HANDLE_VALUE);
        Self(handle)
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct ProcessWaitContext {
    completion_port: Arc<OwnedHandle>,
    completion_key: usize,
}

pub(crate) struct OwnedProcessWait {
    wait: HANDLE,
    context: *mut ProcessWaitContext,
}

// The registered wait and its callback context are transferred to the reactor
// thread. Drop synchronously unregisters the callback before freeing context.
unsafe impl Send for OwnedProcessWait {}

impl OwnedProcessWait {
    pub(crate) fn register(
        process: HANDLE,
        completion_port: Arc<OwnedHandle>,
        completion_key: usize,
    ) -> io::Result<Self> {
        let context = Box::into_raw(Box::new(ProcessWaitContext {
            completion_port,
            completion_key,
        }));
        let mut wait = null_mut();
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                process,
                Some(post_process_exit),
                context.cast(),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if registered == 0 {
            unsafe {
                drop(Box::from_raw(context));
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self { wait, context })
    }
}

impl Drop for OwnedProcessWait {
    fn drop(&mut self) {
        let completion = OwnedHandle::new(unsafe { CreateEventW(null(), 1, 0, null()) });
        let unregistered = completion.as_ref().is_ok_and(|completion| {
            let result = unsafe { UnregisterWaitEx(self.wait, completion.raw()) };
            let pending = result == 0 && unsafe { GetLastError() } == ERROR_IO_PENDING;
            (result != 0 || pending)
                && unsafe { WaitForSingleObject(completion.raw(), INFINITE) } == WAIT_OBJECT_0
        });
        // A signaled completion event proves no callback can still access the
        // context. If the event could not be created, retain the synchronous
        // fallback used by the API for the same guarantee.
        if unregistered || unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) } != 0 {
            unsafe {
                drop(Box::from_raw(self.context));
            }
        }
    }
}

unsafe extern "system" fn post_process_exit(context: *mut c_void, _timed_out: bool) {
    let context = unsafe { &*context.cast::<ProcessWaitContext>() };
    unsafe {
        PostQueuedCompletionStatus(
            context.completion_port.raw(),
            0,
            context.completion_key,
            null_mut(),
        );
    }
}

pub(crate) struct OwnedPseudoConsole(HPCON);

// HPCON ownership is transferred to one closer and is never aliased.
unsafe impl Send for OwnedPseudoConsole {}

impl OwnedPseudoConsole {
    pub(crate) fn new(handle: HPCON) -> Self {
        Self(handle)
    }

    pub(crate) fn raw(&self) -> HPCON {
        self.0
    }

    pub(crate) fn close(mut self) {
        let raw = std::mem::replace(&mut self.0, 0);
        unsafe {
            ClosePseudoConsole(raw);
        }
    }
}

impl Drop for OwnedPseudoConsole {
    fn drop(&mut self) {
        if self.0 != 0 {
            let raw = std::mem::replace(&mut self.0, 0);
            unsafe {
                ClosePseudoConsole(raw);
            }
        }
    }
}

pub(crate) struct AttributeList {
    storage: Box<[usize]>,
    raw: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    pub(crate) fn new(attribute_count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), attribute_count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words].into_boxed_slice();
        let raw = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(raw, attribute_count, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage, raw })
    }

    pub(crate) fn set_pseudoconsole(&mut self, value: HPCON) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                value as *mut c_void,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn set_job(&mut self, jobs: &mut [HANDLE; 1]) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_mut_ptr().cast(),
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn raw(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.raw
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.raw);
        }
        let _ = self.storage.len();
    }
}

pub(crate) struct PipeSecurity {
    descriptor: *mut c_void,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    pub(crate) fn current_owner_and_system() -> io::Result<Self> {
        let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;OW)\0".encode_utf16().collect();
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    pub(crate) fn attributes(&mut self) -> *const SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoKind {
    Read,
    Write,
}

#[repr(C)]
pub(crate) struct IoOperation {
    overlapped: OVERLAPPED,
    pub(crate) buffer: Vec<u8>,
    pub(crate) offset: usize,
    pub(crate) sequence: u64,
    pub(crate) kind: IoKind,
}

// The operation is pinned before its addresses reach Windows and remains owned
// until IOCP reports the terminal completion.
unsafe impl Send for IoOperation {}

impl IoOperation {
    pub(crate) fn read(capacity: usize) -> Pin<Box<Self>> {
        Box::pin(Self {
            overlapped: unsafe { zeroed() },
            buffer: vec![0; capacity],
            offset: 0,
            sequence: 0,
            kind: IoKind::Read,
        })
    }

    pub(crate) fn write(buffer: Vec<u8>, sequence: u64) -> Pin<Box<Self>> {
        Box::pin(Self {
            overlapped: unsafe { zeroed() },
            buffer,
            offset: 0,
            sequence,
            kind: IoKind::Write,
        })
    }

    pub(crate) fn overlapped_mut(self: &mut Pin<Box<Self>>) -> *mut OVERLAPPED {
        unsafe { &mut self.as_mut().get_unchecked_mut().overlapped }
    }

    pub(crate) fn overlapped_ptr(self: &Pin<Box<Self>>) -> *mut OVERLAPPED {
        (&self.as_ref().get_ref().overlapped as *const OVERLAPPED).cast_mut()
    }

    pub(crate) fn remaining_ptr(&self) -> *const u8 {
        unsafe { self.buffer.as_ptr().add(self.offset) }
    }

    pub(crate) fn remaining_len(&self) -> usize {
        self.buffer.len() - self.offset
    }

    pub(crate) fn reset_overlapped(self: &mut Pin<Box<Self>>) {
        unsafe {
            self.as_mut().get_unchecked_mut().overlapped = zeroed();
        }
    }
}
