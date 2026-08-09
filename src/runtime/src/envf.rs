//! `std::env` — the single implementation of the `env` stdlib, shared by both
//! engines. The neutral `pub fn` cores (`cwd`/`get`/`set`/`args`) operate on plain
//! Rust types and are called by the VM (`src/env/mod.rs`, wrapping into `VmValue`)
//! and by the AOT `#[no_mangle]` wrappers below (tagging into the C-string ABI).
//!
//! Trust model (AOT-only; the VM has no taint): `env.get` is external,
//! attacker-influenceable input → TAINTED; `cwd`/`args` are the program's own
//! invocation → TRUSTED. Trust is applied only in the AOT wrappers.

use core::ffi::{c_char, c_void};

use crate::coll::ArrayObj;
use crate::cstr;
use crate::string::{TAINTED, TRUSTED};
use crate::value::JadeValue;

// ── Neutral cores (used by both the VM and the AOT wrappers) ──────────────────

/// The current working directory. `Err` on failure (the VM raises; the AOT
/// wrapper falls back to "").
pub fn cwd() -> std::io::Result<String> {
    Ok(std::env::current_dir()?.to_string_lossy().into_owned())
}

/// The environment variable `name`, or `None` when unset.
pub fn get(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Set environment variable `name` to `value`.
pub fn set(name: &str, value: &str) {
    // Jade programs are single-threaded at the OS/process level.
    #[allow(deprecated)]
    unsafe {
        std::env::set_var(name, value)
    };
}

/// The process arguments (argv[0] first).
pub fn args() -> Vec<String> {
    std::env::args().collect()
}

/// `env.cwd()` — the current working directory as a TRUSTED string (empty on error).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_cwd() -> *mut c_char {
    cstr::emit(cwd().unwrap_or_default().as_bytes(), TRUSTED)
}

/// `env.get(name)` — the environment variable as a TAINTED string, or NULL when
/// unset (codegen maps NULL to nil, matching the C leaf's contract).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_get(name: *const c_char) -> *mut c_char {
    if name.is_null() {
        return core::ptr::null_mut();
    }
    match get(unsafe { cstr::borrow(name) }) {
        Some(v) => cstr::emit(v.as_bytes(), TAINTED),
        None => core::ptr::null_mut(),
    }
}

/// `env.set(name, value)` — set an environment variable (NULL value → empty).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_env_set(name: *const c_char, value: *const c_char) {
    if name.is_null() {
        return;
    }
    set(unsafe { cstr::borrow(name) }, unsafe { cstr::borrow(value) });
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
    for a in args() {
        let s = cstr::emit(a.as_bytes(), TRUSTED);
        arr.push(JadeValue::from_str_ptr(s as *const ()).bits() as i64);
    }
    JadeValue::from_ptr(crate::gc::leak_obj(arr) as *const c_void as *const ()).bits() as i64
}
