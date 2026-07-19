//! `std::env` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/env/env.c` so the stdlib lives once, in Rust. Codegen's emitted
//! `jrt_env_*` calls resolve here. None of these raise, so no C forwarder is
//! needed (unlike the fs/http ops).
//!
//! Trust model (AOT-only; the VM has no taint): `env.get` is external,
//! attacker-influenceable input → TAINTED; `cwd`/`args` are the program's own
//! invocation → TRUSTED.

use core::ffi::{c_char, c_void};

use crate::coll::ArrayObj;
use crate::string::{self, TAINTED, TRUSTED};
use crate::sys::strlen;
use crate::value::JadeValue;

/// Allocate a fresh tagged string holding `bytes` with `trust`; returns the data
/// pointer (the C runtime's `char*`).
unsafe fn mk_str(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

/// Borrow a NUL-terminated C string as `&str` (empty on NULL / invalid UTF-8).
unsafe fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, n)).unwrap_or("")
    }
}

/// `env.cwd()` — the current working directory as a TRUSTED string (empty on error).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_cwd() -> *mut c_char {
    let s = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    unsafe { mk_str(s.as_bytes(), TRUSTED) }
}

/// `env.get(name)` — the environment variable as a TAINTED string, or NULL when
/// unset (codegen maps NULL to nil, matching the C leaf's contract).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_get(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return core::ptr::null_mut();
    }
    match std::env::var(unsafe { cstr(name) }) {
        Ok(v) => unsafe { mk_str(v.as_bytes(), TAINTED) },
        Err(_) => core::ptr::null_mut(),
    }
}

/// `env.set(name, value)` — set an environment variable (NULL value → empty).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_set(name: *const c_char, value: *const c_char) {
    if name.is_null() {
        return;
    }
    unsafe { std::env::set_var(cstr(name), cstr(value)) };
}

/// Receives `main`'s `(argc, argv)`. Retained for ABI compatibility (codegen
/// emits a call in the program prologue) but a no-op: [`jrt_env_args`] reads the
/// process arguments live via `std::env::args`, matching the VM.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_set_args(_argc: i32, _argv: *mut *mut c_char) {}

/// `env.args()` — the process arguments as a tagged ObjHeader array of TRUSTED
/// string words (argv[0] first). The returned word is already boxed
/// (`JadeValue::from_ptr`), so codegen consumes it directly.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_args() -> i64 {
    let mut arr = ArrayObj::<i64>::new();
    for a in std::env::args() {
        let s = unsafe { mk_str(a.as_bytes(), TRUSTED) };
        arr.push(JadeValue::from_str_ptr(s as *const ()).bits() as i64);
    }
    JadeValue::from_ptr(crate::gc::leak_obj(arr) as *const c_void as *const ()).bits() as i64
}
