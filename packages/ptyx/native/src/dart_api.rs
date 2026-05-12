//! Minimal Dart native API bindings.
//!
//! The build script enables this module only when Dart SDK headers are
//! available. The declarations here mirror the small subset needed for native
//! ports and external typed data.

use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::abi::{
    PTYX_STATUS_INVALID_ARGUMENT, PTYX_STATUS_NATIVE_ERROR, PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED,
};
use crate::error::{ffi_status, PtyxError};

static DART_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub(crate) fn init(dart_initialize_api_dl_data: *mut c_void) -> u32 {
    ffi_status(|| {
        if dart_initialize_api_dl_data.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "Dart initialize API data must not be null",
            ));
        }
        if !dart_dl_available() {
            return Err(PtyxError::new(
                PTYX_STATUS_UNSUPPORTED,
                "Dart DL API was built without Dart SDK headers",
            ));
        }
        let rc = dart_initialize_api_dl(dart_initialize_api_dl_data);
        if rc != 0 {
            return Err(PtyxError::new(
                PTYX_STATUS_NATIVE_ERROR,
                format!("Dart_InitializeApiDL failed with {rc}"),
            ));
        }
        DART_INITIALIZED.store(true, Ordering::SeqCst);
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn is_initialized_bool() -> bool {
    DART_INITIALIZED.load(Ordering::SeqCst)
}

#[cfg(ptyx_dart_dl)]
pub(crate) type DartPortDl = i64;

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) enum DartCObjectType {
    Int64 = 3,
    String = 5,
    Array = 6,
    TypedData = 7,
    UnmodifiableExternalTypedData = 13,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) enum DartTypedDataType {
    Uint8 = 2,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DartCObjectArray {
    length: isize,
    values: *mut *mut DartCObject,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DartCObjectTypedData {
    typed_data_type: DartTypedDataType,
    length: isize,
    values: *const u8,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DartCObjectExternalTypedData {
    typed_data_type: DartTypedDataType,
    length: isize,
    data: *mut u8,
    peer: *mut c_void,
    callback: DartHandleFinalizer,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union DartCObjectValue {
    as_int64: i64,
    as_string: *const c_char,
    as_array: DartCObjectArray,
    as_typed_data: DartCObjectTypedData,
    as_external_typed_data: DartCObjectExternalTypedData,
}

#[cfg(ptyx_dart_dl)]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DartCObject {
    object_type: DartCObjectType,
    value: DartCObjectValue,
}

#[cfg(ptyx_dart_dl)]
pub(crate) type DartHandleFinalizer = extern "C" fn(*mut c_void, *mut c_void);

#[cfg(ptyx_dart_dl)]
impl DartCObject {
    pub(crate) fn int64(value: i64) -> Self {
        Self {
            object_type: DartCObjectType::Int64,
            value: DartCObjectValue { as_int64: value },
        }
    }

    pub(crate) fn string(value: *const c_char) -> Self {
        Self {
            object_type: DartCObjectType::String,
            value: DartCObjectValue { as_string: value },
        }
    }

    pub(crate) fn typed_data(data: *const u8, len: isize) -> Self {
        Self {
            object_type: DartCObjectType::TypedData,
            value: DartCObjectValue {
                as_typed_data: DartCObjectTypedData {
                    typed_data_type: DartTypedDataType::Uint8,
                    length: len,
                    values: data,
                },
            },
        }
    }

    pub(crate) fn external_typed_data(
        data: *mut u8,
        len: isize,
        peer: *mut c_void,
        callback: DartHandleFinalizer,
    ) -> Self {
        Self {
            object_type: DartCObjectType::UnmodifiableExternalTypedData,
            value: DartCObjectValue {
                as_external_typed_data: DartCObjectExternalTypedData {
                    typed_data_type: DartTypedDataType::Uint8,
                    length: len,
                    data,
                    peer,
                    callback,
                },
            },
        }
    }

    pub(crate) fn array(values: *mut *mut DartCObject, len: isize) -> Self {
        Self {
            object_type: DartCObjectType::Array,
            value: DartCObjectValue {
                as_array: DartCObjectArray {
                    length: len,
                    values,
                },
            },
        }
    }
}

#[cfg(ptyx_dart_dl)]
type DartPostCObjectFn = unsafe extern "C" fn(DartPortDl, *mut DartCObject) -> bool;

#[cfg(ptyx_dart_dl)]
unsafe extern "C" {
    fn Dart_InitializeApiDL(data: *mut c_void) -> isize;
    static Dart_PostCObject_DL: Option<DartPostCObjectFn>;
}

#[cfg(ptyx_dart_dl)]
fn dart_dl_available() -> bool {
    true
}

#[cfg(not(ptyx_dart_dl))]
fn dart_dl_available() -> bool {
    false
}

#[cfg(ptyx_dart_dl)]
fn dart_initialize_api_dl(data: *mut c_void) -> isize {
    // `data` is the initialization token supplied by Dart. The C API owns its
    // lifetime for this call.
    unsafe { Dart_InitializeApiDL(data) }
}

#[cfg(not(ptyx_dart_dl))]
fn dart_initialize_api_dl(_data: *mut c_void) -> isize {
    -1
}

#[cfg(ptyx_dart_dl)]
fn post_cobject(port: i64, object: &mut DartCObject) -> bool {
    // Dart initializes this function pointer through `Dart_InitializeApiDL`.
    let Some(post) = (unsafe { Dart_PostCObject_DL }) else {
        return false;
    };
    // `object` and nested values stay alive until `Dart_PostCObject_DL`
    // returns. External typed data uses an explicit finalizer for later access.
    unsafe { post(port, object) }
}

#[cfg(not(ptyx_dart_dl))]
fn post_cobject(_port: i64, _object: &mut ()) -> bool {
    false
}

#[cfg(ptyx_dart_dl)]
pub(crate) fn post_array(port: i64, values: &mut [DartCObject]) -> bool {
    let mut ptrs = [std::ptr::null_mut(); 10];
    debug_assert!(values.len() <= ptrs.len());
    for (index, value) in values.iter_mut().enumerate() {
        ptrs[index] = value;
    }
    let mut object = DartCObject::array(ptrs.as_mut_ptr(), values.len() as isize);
    post_cobject(port, &mut object)
}

#[cfg(not(ptyx_dart_dl))]
pub(crate) fn post_array(_port: i64) -> bool {
    false
}
