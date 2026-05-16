//! Minimal Dart native API bindings.
//!
//! This module owns the Dart DL C ABI details. Only Dart ports, Dart values,
//! and initialization state are modeled here.

use std::fmt;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

static DART_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub(crate) type DartPort = i64;
pub(crate) type DartHandleFinalizer = extern "C" fn(*mut c_void, *mut c_void);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DartApiError {
    NullInitializeData,
    InitializeFailed(isize),
}

impl fmt::Display for DartApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DartApiError::NullInitializeData => {
                formatter.write_str("Dart initialize API data must not be null")
            }
            DartApiError::InitializeFailed(code) => {
                write!(formatter, "Dart_InitializeApiDL failed with {code}")
            }
        }
    }
}

impl std::error::Error for DartApiError {}

pub(crate) fn init(dart_initialize_api_dl_data: *mut c_void) -> Result<(), DartApiError> {
    if dart_initialize_api_dl_data.is_null() {
        return Err(DartApiError::NullInitializeData);
    }

    let rc = dart_initialize_api_dl(dart_initialize_api_dl_data);
    if rc != 0 {
        return Err(DartApiError::InitializeFailed(rc));
    }
    DART_INITIALIZED.store(true, Ordering::SeqCst);
    Ok(())
}

pub(crate) fn is_initialized() -> bool {
    DART_INITIALIZED.load(Ordering::SeqCst)
}

#[derive(Clone, Copy)]
pub(crate) enum DartValue {
    Int64(i64),
    String(*const c_char),
    TypedData {
        data: *const u8,
        length: usize,
    },
    ExternalTypedData {
        data: *mut u8,
        length: usize,
        peer: *mut c_void,
        finalizer: DartHandleFinalizer,
    },
}

impl DartValue {
    pub(crate) fn int64(value: i64) -> Self {
        Self::Int64(value)
    }

    pub(crate) fn string(value: *const c_char) -> Self {
        Self::String(value)
    }

    pub(crate) fn typed_data(data: *const u8, length: usize) -> Self {
        Self::TypedData { data, length }
    }

    pub(crate) fn external_typed_data(
        data: *mut u8,
        length: usize,
        peer: *mut c_void,
        finalizer: DartHandleFinalizer,
    ) -> Self {
        Self::ExternalTypedData {
            data,
            length,
            peer,
            finalizer,
        }
    }

    fn length_fits_dart(self) -> bool {
        match self {
            DartValue::TypedData { length, .. } | DartValue::ExternalTypedData { length, .. } => {
                isize::try_from(length).is_ok()
            }
            DartValue::Int64(_) | DartValue::String(_) => true,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
enum DartCObjectType {
    Int64 = 3,
    String = 5,
    Array = 6,
    TypedData = 7,
    UnmodifiableExternalTypedData = 13,
}

#[repr(C)]
#[derive(Clone, Copy)]
enum DartTypedDataType {
    Uint8 = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DartCObjectArray {
    length: isize,
    values: *mut *mut DartCObject,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DartCObjectTypedData {
    typed_data_type: DartTypedDataType,
    length: isize,
    values: *const u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DartCObjectExternalTypedData {
    typed_data_type: DartTypedDataType,
    length: isize,
    data: *mut u8,
    peer: *mut c_void,
    callback: DartHandleFinalizer,
}

#[repr(C)]
#[derive(Clone, Copy)]
union DartCObjectValue {
    as_int64: i64,
    as_string: *const c_char,
    as_array: DartCObjectArray,
    as_typed_data: DartCObjectTypedData,
    as_external_typed_data: DartCObjectExternalTypedData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DartCObject {
    object_type: DartCObjectType,
    value: DartCObjectValue,
}

impl DartCObject {
    fn from_value(value: DartValue) -> Self {
        match value {
            DartValue::Int64(value) => Self {
                object_type: DartCObjectType::Int64,
                value: DartCObjectValue { as_int64: value },
            },
            DartValue::String(value) => Self {
                object_type: DartCObjectType::String,
                value: DartCObjectValue { as_string: value },
            },
            DartValue::TypedData { data, length } => Self {
                object_type: DartCObjectType::TypedData,
                value: DartCObjectValue {
                    as_typed_data: DartCObjectTypedData {
                        typed_data_type: DartTypedDataType::Uint8,
                        length: length as isize,
                        values: data,
                    },
                },
            },
            DartValue::ExternalTypedData {
                data,
                length,
                peer,
                finalizer,
            } => Self {
                object_type: DartCObjectType::UnmodifiableExternalTypedData,
                value: DartCObjectValue {
                    as_external_typed_data: DartCObjectExternalTypedData {
                        typed_data_type: DartTypedDataType::Uint8,
                        length: length as isize,
                        data,
                        peer,
                        callback: finalizer,
                    },
                },
            },
        }
    }

    fn array(values: *mut *mut DartCObject, len: isize) -> Self {
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

type DartPostCObjectFn = unsafe extern "C" fn(DartPort, *mut DartCObject) -> bool;

unsafe extern "C" {
    fn Dart_InitializeApiDL(data: *mut c_void) -> isize;
    static Dart_PostCObject_DL: Option<DartPostCObjectFn>;
}

fn dart_initialize_api_dl(data: *mut c_void) -> isize {
    unsafe { Dart_InitializeApiDL(data) }
}

fn post_cobject(port: DartPort, object: &mut DartCObject) -> bool {
    let Some(post) = (unsafe { Dart_PostCObject_DL }) else {
        return false;
    };
    unsafe { post(port, object) }
}

pub(crate) fn post_array<const N: usize>(port: DartPort, values: [DartValue; N]) -> bool {
    if values.iter().any(|value| !value.length_fits_dart()) {
        return false;
    }

    let mut objects: [DartCObject; N] =
        std::array::from_fn(|index| DartCObject::from_value(values[index]));
    let mut ptrs: [*mut DartCObject; N] =
        std::array::from_fn(|index| &mut objects[index] as *mut DartCObject);
    let mut object = DartCObject::array(ptrs.as_mut_ptr(), N as isize);
    post_cobject(port, &mut object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_rejects_null_initialize_data() {
        let result = init(std::ptr::null_mut());

        assert_eq!(result, Err(DartApiError::NullInitializeData));
    }
}
