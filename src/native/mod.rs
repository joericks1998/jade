use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    sync::Arc,
};

use jade_runtime::coll::{DictObj, StructObj};

use crate::{
    builtins::make_array,
    vm::{Mutex, VmValue},
    frontend::error::{JadeError, Result, Span},
};

// The transport trees for arrays/dicts are libc-heap so either `jade-runtime`
// instance in the process (this VM, and each dlopen'd package — each with its own
// mimalloc) can free them; see the JadeArr/JadeMap note below and runtime_aot's
// runtime.h for the full rationale.
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

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
/// `data.as_arr` → a deep-copied [`JadeArr`] tree (libc-owned).
pub const JADE_TAG_ARRAY: u8 = 6;
/// `data.as_dict` → a deep-copied [`JadeMap`] tree (libc-owned).
pub const JADE_TAG_DICT:  u8 = 7;
/// `data.as_struct` → a deep-copied [`JadeStruct`] tree (libc-owned).
///
/// A struct is a dict that also carries its type name. The name is what makes a
/// typed contract enforceable across the boundary: a receiver can refuse a
/// struct that is not the type it expects, where a bare dict with the wrong keys
/// reads as a set of nils and fails silently.
pub const JADE_TAG_STRUCT: u8 = 8;

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
    pub as_arr:   *mut JadeArr,
    pub as_dict:  *mut JadeMap,
    pub as_struct: *mut JadeStruct,
}

#[repr(C)]
pub struct JadeVal {
    pub tag:  u8,
    pub _pad: [u8; 7],
    pub data: JadeValData,
}

/// A collection cannot cross the FFI by pointer — the process holds two
/// `jade-runtime` instances (this VM and each dlopen'd package), each with its
/// own mimalloc, so a word owned by one must never be freed by the other. Arrays
/// and dicts are therefore *deep-copied* into these trees, every node allocated
/// with libc `malloc`/strdup — a process-shared allocator (mimalloc is a Rust
/// `#[global_allocator]`; it does not override the C `malloc`) — so either side
/// releases the whole tree with [`ffi_free`]. Layout mirrors `JadeArr`/`JadeMap`
/// in runtime_aot's runtime.h. Cyclic collections are unsupported.
#[repr(C)]
pub struct JadeArr {
    pub items: *mut JadeVal,
    pub len:   usize,
}

#[repr(C)]
pub struct JadeMap {
    pub keys: *mut *const u8,
    pub vals: *mut JadeVal,
    pub len:  usize,
}

/// A [`JadeMap`] plus the struct's type name, in declaration order. Same
/// ownership rules: every node is libc heap, released by [`ffi_free`].
#[repr(C)]
pub struct JadeStruct {
    /// Null-terminated UTF-8, libc-owned.
    pub type_name: *const u8,
    pub keys: *mut *const u8,
    pub vals: *mut JadeVal,
    pub len:  usize,
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
        // Build FFI args.  CStrings back top-level string args and must outlive
        // the native call; array/dict args are libc-heap trees freed below.
        let mut cstrings: Vec<CString> = Vec::new();
        let ffi_args: Vec<JadeVal> = args.iter().map(|v| vm_to_ffi(v, &mut cstrings)).collect();

        let mut out = JadeVal::nil();
        let status =
            unsafe { (self.fn_ptr)(ffi_args.len(), ffi_args.as_ptr(), &mut out) };
        // ffi_args and cstrings are still alive here ↑

        // Read the result before freeing anything (ffi_to_vm deep-copies out of
        // the transport tree), then release every marshalled tree.
        let result = if status != 0 {
            let msg = if out.tag == JADE_TAG_STR || out.tag == JADE_TAG_ERROR {
                unsafe {
                    CStr::from_ptr(out.data.as_str as *const c_char)
                        .to_string_lossy()
                        .into_owned()
                }
            } else {
                format!("native fn '{}' returned error code {}", self.name, status)
            };
            Err(JadeError::IoError { message: msg, span })
        } else {
            ffi_to_vm(&out, span)
        };

        unsafe {
            for a in &ffi_args {
                ffi_free(a);
            }
            ffi_free(&out);
        }

        result
    }
}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Load a native package from a shared library and return its exported functions
/// as a `HashMap<name, VmValue::NativeLibFn>`.
/// Refuse a package built against a value ABI this runtime cannot talk to.
///
/// Two symbols are consulted, in order:
///
///  * `jade_pkg_abi_version` — emitted into every package `jade build --lib`
///    produces (see [`crate::aot`]). Authoritative.
///  * `jrt_abi_version` — the runtime's own accessor. A package that links
///    `jade-runtime` may re-export it, which is how packages published before
///    the first symbol existed can still be checked.
///
/// Neither present means the package does not link the Jade runtime at all — a
/// plain C library wrapped by `jade pkg add --c-abi`, which has no value ABI to
/// disagree about. Those load, as they always have.
///
/// This exists because nothing checked [`jade_runtime::RUNTIME_ABI_VERSION`]
/// despite its whole purpose being detection: when v1.1.31 began sending the
/// inference request as a struct, every published provider — built against
/// ABI 1 — failed with `native function returned an unknown value tag` from
/// inside the call, naming neither the version nor the fix.
fn check_package_abi(
    lib: &libloading::Library,
    lib_path: &Path,
    span: Span,
) -> Result<()> {
    let read = |sym: &[u8]| -> Option<u32> {
        let f: libloading::Symbol<unsafe extern "C" fn() -> u32> =
            unsafe { lib.get(sym) }.ok()?;
        Some(unsafe { f() })
    };

    let Some(theirs) = read(b"jade_pkg_abi_version\0").or_else(|| read(b"jrt_abi_version\0"))
    else {
        return Ok(());
    };

    let ours = jade_runtime::RUNTIME_ABI_VERSION;
    if theirs == ours {
        return Ok(());
    }

    let advice = if theirs < ours {
        "It was built against an older Jade. Rebuild it with this toolchain, or \
         reinstall the providers that ship with your Jade release."
    } else {
        "It was built against a newer Jade than this one. Upgrade with `jade upgrade`."
    };
    Err(JadeError::IoError {
        message: format!(
            "native package '{}' speaks value ABI {theirs}, but this Jade speaks {ours}. {advice}",
            lib_path.display()
        ),
        span,
    })
}

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

    check_package_abi(&lib, lib_path, span)?;

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

// ── libc-heap helpers for the array/dict transport trees ──────────────────────

/// `malloc(n * size_of::<T>())`, aborting on failure. Returns null for `n == 0`.
unsafe fn ffi_alloc<T>(n: usize) -> *mut T {
    if n == 0 {
        return std::ptr::null_mut();
    }
    let p = unsafe { malloc(n * std::mem::size_of::<T>()) } as *mut T;
    if p.is_null() {
        std::alloc::handle_alloc_error(
            std::alloc::Layout::array::<T>(n).expect("ffi layout"),
        );
    }
    p
}

/// Copy `s` into a libc-owned NUL-terminated buffer (a container-owned string).
unsafe fn ffi_strdup(s: &str) -> *const u8 {
    let bytes = s.as_bytes();
    let p = unsafe { malloc(bytes.len() + 1) } as *mut u8;
    if p.is_null() {
        std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(bytes.len() + 1, 1).expect("ffi layout"),
        );
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        *p.add(bytes.len()) = 0;
    }
    p
}

/// The name a struct crosses the ABI under: its source name, with any
/// import-mangling suffix removed.
///
/// `aot/imports.rs` renames a module-global `Foo` to `Foo$2` when flattening
/// imports, so two imported modules can each declare one. That number is a
/// property of the importing program's module graph, not of the type, so it means
/// nothing on the other side of an FFI call. Without stripping it, a provider
/// package built with `use ovata::infer` hands back a frame named `Token$0` and
/// the caller does not recognise its own protocol.
///
/// `$` cannot appear in a Jade identifier, so a trailing `$<digits>` is always the
/// mangling and never part of a name someone wrote. `runtime_aot/native.c` strips
/// the same thing on its side of the boundary.
pub fn abi_type_name(name: &str) -> &str {
    match name.rsplit_once('$') {
        // A non-empty base is required: stripping would otherwise turn a name that
        // is *only* a suffix into an empty type name, which is worse than leaving
        // the odd input alone.
        Some((base, id))
            if !base.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => name,
    }
}

/// Marshal a `VmValue` into a `JadeVal` whose every owned part is libc heap:
/// strings are copied (via [`ffi_strdup`]) and arrays/dicts build nested trees,
/// so the whole thing is releasable with [`ffi_free`]. Used for container
/// elements at any depth, and for a top-level array/dict argument. A top-level
/// *string* argument instead borrows through [`vm_to_ffi`]'s `CString` scratch.
fn vm_to_ffi_owned(val: &VmValue) -> JadeVal {
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
        VmValue::Str(s) => JadeVal {
            tag: JADE_TAG_STR,
            _pad: [0; 7],
            data: JadeValData { as_str: unsafe { ffi_strdup(s.as_str()) } },
        },
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let n = guard.len();
            let items = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, el) in guard.iter().enumerate() {
                unsafe { items.add(i).write(vm_to_ffi_owned(el)) };
            }
            let a = unsafe { ffi_alloc::<JadeArr>(1) };
            unsafe { a.write(JadeArr { items, len: n }) };
            JadeVal { tag: JADE_TAG_ARRAY, _pad: [0; 7], data: JadeValData { as_arr: a } }
        }
        VmValue::Dict(map) => {
            let n = map.len();
            let keys = unsafe { ffi_alloc::<*const u8>(n) };
            let vals = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, (k, v)) in map.iter().enumerate() {
                unsafe {
                    keys.add(i).write(ffi_strdup(k));
                    vals.add(i).write(vm_to_ffi_owned(v));
                }
            }
            let m = unsafe { ffi_alloc::<JadeMap>(1) };
            unsafe { m.write(JadeMap { keys, vals, len: n }) };
            JadeVal { tag: JADE_TAG_DICT, _pad: [0; 7], data: JadeValData { as_dict: m } }
        }
        VmValue::Struct(arc) => {
            let guard = arc.lock();
            let n = guard.len();
            let keys = unsafe { ffi_alloc::<*const u8>(n) };
            let vals = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, (k, v)) in guard.fields().iter().enumerate() {
                unsafe {
                    keys.add(i).write(ffi_strdup(k));
                    vals.add(i).write(vm_to_ffi_owned(v));
                }
            }
            let st = unsafe { ffi_alloc::<JadeStruct>(1) };
            unsafe {
                st.write(JadeStruct {
                    type_name: ffi_strdup(abi_type_name(guard.type_name())),
                    keys,
                    vals,
                    len: n,
                })
            };
            JadeVal { tag: JADE_TAG_STRUCT, _pad: [0; 7], data: JadeValData { as_struct: st } }
        }
        // Remaining kinds (functions, futures, prompts) have no ABI
        // representation — native fns can't consume them.
        _ => JadeVal::nil(),
    }
}

/// Convert a `VmValue` to a `JadeVal` for a top-level argument. A string is
/// handed over as a borrowed pointer into a `CString` pushed onto `scratch`,
/// which the caller must keep alive for the native call; arrays and dicts become
/// libc-heap trees the caller releases with [`ffi_free`] afterward.
pub fn vm_to_ffi(val: &VmValue, scratch: &mut Vec<CString>) -> JadeVal {
    if let VmValue::Str(s) = val {
        let cs = CString::new(s.as_bytes()).unwrap_or_default();
        let ptr = cs.as_ptr() as *const u8;
        scratch.push(cs);
        return JadeVal { tag: JADE_TAG_STR, _pad: [0; 7], data: JadeValData { as_str: ptr } };
    }
    vm_to_ffi_owned(val)
}

/// Convert a `JadeVal` returned by a native function back to a `VmValue`,
/// deep-copying array/dict trees into owned collections.
pub fn ffi_to_vm(val: &JadeVal, span: Span) -> Result<VmValue> {
    match val.tag {
        JADE_TAG_NIL => Ok(VmValue::Nil),
        JADE_TAG_INT => Ok(VmValue::Int(unsafe { val.data.as_int })),
        JADE_TAG_FLOAT => Ok(VmValue::Float(unsafe { val.data.as_float })),
        JADE_TAG_BOOL => Ok(VmValue::Bool(unsafe { val.data.as_bool } != 0)),
        JADE_TAG_STR => {
            let s = unsafe {
                CStr::from_ptr(val.data.as_str as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            };
            Ok(VmValue::Str(s.into()))
        }
        JADE_TAG_ARRAY => {
            let a = unsafe { val.data.as_arr };
            let mut items = Vec::new();
            if !a.is_null() {
                let len = unsafe { (*a).len };
                let base = unsafe { (*a).items };
                for i in 0..len {
                    items.push(ffi_to_vm(unsafe { &*base.add(i) }, span)?);
                }
            }
            Ok(make_array(items))
        }
        JADE_TAG_DICT => {
            let m = unsafe { val.data.as_dict };
            let mut d = DictObj::new();
            if !m.is_null() {
                let len = unsafe { (*m).len };
                let (keys, vals) = unsafe { ((*m).keys, (*m).vals) };
                for i in 0..len {
                    let key = unsafe {
                        CStr::from_ptr(*keys.add(i) as *const c_char)
                            .to_string_lossy()
                            .into_owned()
                    };
                    let value = ffi_to_vm(unsafe { &*vals.add(i) }, span)?;
                    d.insert(key, value);
                }
            }
            Ok(VmValue::Dict(d))
        }
        JADE_TAG_STRUCT => {
            let st = unsafe { val.data.as_struct };
            if st.is_null() {
                return Ok(VmValue::Nil);
            }
            let (type_name, keys, vals, len) =
                unsafe { ((*st).type_name, (*st).keys, (*st).vals, (*st).len) };
            let name = unsafe {
                CStr::from_ptr(type_name as *const c_char).to_string_lossy().into_owned()
            };
            let mut obj = StructObj::new(&name);
            for i in 0..len {
                let key = unsafe {
                    CStr::from_ptr(*keys.add(i) as *const c_char).to_string_lossy().into_owned()
                };
                let value = ffi_to_vm(unsafe { &*vals.add(i) }, span)?;
                obj.set_field(&key, value);
            }
            Ok(VmValue::Struct(Arc::new(Mutex::new(obj))))
        }
        JADE_TAG_ERROR => {
            let msg = unsafe {
                CStr::from_ptr(val.data.as_str as *const c_char)
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

/// Recursively free a container-owned node: its owned string, or its nested
/// arrays/dicts. Reached only through a container root, where string elements
/// are libc-owned (`ffi_strdup`).
unsafe fn ffi_free_node(v: &JadeVal) {
    match v.tag {
        JADE_TAG_STR | JADE_TAG_ERROR => unsafe { free(v.data.as_str as *mut c_void) },
        JADE_TAG_ARRAY => {
            let a = unsafe { v.data.as_arr };
            if !a.is_null() {
                let (base, len) = unsafe { ((*a).items, (*a).len) };
                for i in 0..len {
                    unsafe { ffi_free_node(&*base.add(i)) };
                }
                unsafe {
                    free(base as *mut c_void);
                    free(a as *mut c_void);
                }
            }
        }
        JADE_TAG_DICT => {
            let m = unsafe { v.data.as_dict };
            if !m.is_null() {
                let (keys, vals, len) = unsafe { ((*m).keys, (*m).vals, (*m).len) };
                for i in 0..len {
                    unsafe {
                        free(*keys.add(i) as *mut c_void);
                        ffi_free_node(&*vals.add(i));
                    }
                }
                unsafe {
                    free(keys as *mut c_void);
                    free(vals as *mut c_void);
                    free(m as *mut c_void);
                }
            }
        }
        JADE_TAG_STRUCT => {
            let st = unsafe { v.data.as_struct };
            if !st.is_null() {
                let (name, keys, vals, len) =
                    unsafe { ((*st).type_name, (*st).keys, (*st).vals, (*st).len) };
                for i in 0..len {
                    unsafe {
                        free(*keys.add(i) as *mut c_void);
                        ffi_free_node(&*vals.add(i));
                    }
                }
                unsafe {
                    free(name as *mut c_void);
                    free(keys as *mut c_void);
                    free(vals as *mut c_void);
                    free(st as *mut c_void);
                }
            }
        }
        _ => {} // scalar: nothing owned
    }
}

/// Release a marshalled `JadeVal` tree. Only container roots own libc heap;
/// scalars (including top-level non-owning strings) are left untouched, so this
/// is safe to call on any `JadeVal`.
pub unsafe fn ffi_free(v: &JadeVal) {
    if matches!(v.tag, JADE_TAG_ARRAY | JADE_TAG_DICT | JADE_TAG_STRUCT) {
        unsafe { ffi_free_node(v) };
    }
}

#[cfg(test)]
mod tests;
