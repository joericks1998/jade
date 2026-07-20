use std::{
    collections::HashMap,
    ffi::{CStr, CString},
    path::Path,
    sync::Arc,
};

use crate::{
    vm::VmValue,
    frontend::error::{JadeError, Result, Span},
};

// ── C-ABI tag constants ───────────────────────────────────────────────────────

pub const JADE_TAG_NIL:   u8 = 0;
pub const JADE_TAG_INT:   u8 = 1;
pub const JADE_TAG_FLOAT: u8 = 2;
pub const JADE_TAG_BOOL:  u8 = 3;
/// Null-terminated UTF-8.  For *input* args, jade owns the buffer.
/// For *output* vals, the native lib owns the buffer (must stay valid through
/// the return of the native function — jade copies immediately).
pub const JADE_TAG_STR:   u8 = 4;
/// Like JADE_TAG_STR but signals an error.  The str is the error message.
pub const JADE_TAG_ERROR: u8 = 5;

// ── Value type ────────────────────────────────────────────────────────────────

#[repr(C)]
pub union JadeValData {
    pub as_int:   i64,
    pub as_float: f64,
    pub as_bool:  u8,
    /// Non-owning pointer to null-terminated UTF-8.
    pub as_str:   *const u8,
    /// Padding — present for JADE_TAG_NIL.
    pub as_nil:   u64,
}

#[repr(C)]
pub struct JadeVal {
    pub tag:  u8,
    pub _pad: [u8; 7],
    pub data: JadeValData,
}

impl JadeVal {
    pub fn nil() -> Self {
        JadeVal { tag: JADE_TAG_NIL, _pad: [0; 7], data: JadeValData { as_nil: 0 } }
    }
}

// ── Package descriptor ────────────────────────────────────────────────────────

/// Function pointer type used by the native ABI.
pub type JadeNativeFnPtr =
    unsafe extern "C" fn(argc: usize, argv: *const JadeVal, out: *mut JadeVal) -> i32;

/// Single exported function binding returned by `jade_pkg_init`.
#[repr(C)]
pub struct JadeBinding {
    /// Null-terminated ASCII/UTF-8 name.
    pub name: *const std::ffi::c_char,
    pub func: JadeNativeFnPtr,
}

/// Top-level descriptor written into the `out` pointer by `jade_pkg_init`.
#[repr(C)]
pub struct JadeNativePkg {
    /// Null-terminated package name (informational).
    pub name:          *const std::ffi::c_char,
    pub bindings:      *const JadeBinding,
    pub binding_count: usize,
}

// ── NativeLibFn — a callable that wraps a native ABI function ─────────────────

pub struct NativeLibFn {
    pub name: String,
    fn_ptr:   JadeNativeFnPtr,
    /// Keep the library loaded for as long as any of its functions are alive.
    _lib:     Arc<libloading::Library>,
}

impl NativeLibFn {
    pub fn call(&self, args: &[VmValue], span: Span) -> Result<VmValue> {
        // Build FFI args.  CStrings must outlive the native call.
        let mut cstrings: Vec<CString> = Vec::new();
        let ffi_args: Vec<JadeVal> = args.iter().map(|v| vm_to_ffi(v, &mut cstrings)).collect();

        let mut out = JadeVal::nil();
        let status =
            unsafe { (self.fn_ptr)(ffi_args.len(), ffi_args.as_ptr(), &mut out) };
        // ffi_args and cstrings are still alive here ↑

        if status != 0 {
            let msg = if out.tag == JADE_TAG_STR || out.tag == JADE_TAG_ERROR {
                unsafe {
                    CStr::from_ptr(out.data.as_str as *const std::ffi::c_char)
                        .to_string_lossy()
                        .into_owned()
                }
            } else {
                format!("native fn '{}' returned error code {}", self.name, status)
            };
            return Err(JadeError::IoError { message: msg, span });
        }

        ffi_to_vm(&out, span)
    }
}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Load a native package from a shared library and return its exported functions
/// as a `HashMap<name, VmValue::NativeLibFn>`.
pub fn load_native_package(
    lib_path: &Path,
    span: Span,
) -> Result<HashMap<String, VmValue>> {
    let lib = unsafe { libloading::Library::new(lib_path) }.map_err(|e| {
        JadeError::IoError {
            message: format!(
                "could not load native library '{}': {}",
                lib_path.display(),
                e
            ),
            span,
        }
    })?;

    let lib = Arc::new(lib);

    let init_fn: libloading::Symbol<unsafe extern "C" fn(*mut JadeNativePkg) -> i32> =
        unsafe { lib.get(b"jade_pkg_init\0") }.map_err(|e| JadeError::IoError {
            message: format!(
                "native library '{}' missing `jade_pkg_init` symbol: {}",
                lib_path.display(),
                e
            ),
            span,
        })?;

    let mut pkg = JadeNativePkg {
        name:          std::ptr::null(),
        bindings:      std::ptr::null(),
        binding_count: 0,
    };
    let status = unsafe { init_fn(&mut pkg) };
    if status != 0 {
        return Err(JadeError::IoError {
            message: format!(
                "jade_pkg_init in '{}' returned error code {}",
                lib_path.display(),
                status
            ),
            span,
        });
    }

    if pkg.bindings.is_null() || pkg.binding_count == 0 {
        return Ok(HashMap::new());
    }

    let bindings = unsafe { std::slice::from_raw_parts(pkg.bindings, pkg.binding_count) };
    let mut map = HashMap::with_capacity(bindings.len());

    for binding in bindings {
        if binding.name.is_null() {
            continue;
        }
        let name = unsafe { CStr::from_ptr(binding.name) }
            .to_string_lossy()
            .into_owned();
        let nfn = Arc::new(NativeLibFn {
            name: name.clone(),
            fn_ptr: binding.func,
            _lib: Arc::clone(&lib),
        });
        map.insert(name, VmValue::NativeLibFn(nfn));
    }

    Ok(map)
}

// ── Conversions ───────────────────────────────────────────────────────────────

/// Convert a `VmValue` to a `JadeVal`.
/// String conversions allocate a `CString` pushed into `scratch` — the caller
/// must keep `scratch` alive for the duration of the native call.
pub fn vm_to_ffi(val: &VmValue, scratch: &mut Vec<CString>) -> JadeVal {
    match val {
        VmValue::Nil => JadeVal::nil(),
        VmValue::Int(i) => JadeVal {
            tag: JADE_TAG_INT,
            _pad: [0; 7],
            data: JadeValData { as_int: *i },
        },
        VmValue::Float(f) => JadeVal {
            tag: JADE_TAG_FLOAT,
            _pad: [0; 7],
            data: JadeValData { as_float: *f },
        },
        VmValue::Bool(b) => JadeVal {
            tag: JADE_TAG_BOOL,
            _pad: [0; 7],
            data: JadeValData { as_bool: if *b { 1 } else { 0 } },
        },
        VmValue::Str(s) => {
            let cs = CString::new(s.as_bytes()).unwrap_or_default();
            let ptr = cs.as_ptr() as *const u8;
            scratch.push(cs);
            JadeVal {
                tag: JADE_TAG_STR,
                _pad: [0; 7],
                data: JadeValData { as_str: ptr },
            }
        }
        // Non-primitive types become nil — native fns can't consume them.
        _ => JadeVal::nil(),
    }
}

/// Convert a `JadeVal` returned by a native function back to a `VmValue`.
pub fn ffi_to_vm(val: &JadeVal, span: Span) -> Result<VmValue> {
    match val.tag {
        JADE_TAG_NIL => Ok(VmValue::Nil),
        JADE_TAG_INT => Ok(VmValue::Int(unsafe { val.data.as_int })),
        JADE_TAG_FLOAT => Ok(VmValue::Float(unsafe { val.data.as_float })),
        JADE_TAG_BOOL => Ok(VmValue::Bool(unsafe { val.data.as_bool } != 0)),
        JADE_TAG_STR => {
            let s = unsafe {
                CStr::from_ptr(val.data.as_str as *const std::ffi::c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            Ok(VmValue::Str(s.into()))
        }
        JADE_TAG_ERROR => {
            let msg = unsafe {
                CStr::from_ptr(val.data.as_str as *const std::ffi::c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            Err(JadeError::IoError { message: msg, span })
        }
        other => Err(JadeError::IoError {
            message: format!("native function returned unknown tag {other:#04x}"),
            span,
        }),
    }
}

#[cfg(test)]
mod tests;
