//! A runtime method registry for the AOT backend's dynamic method dispatch.
//!
//! Most extend-method calls devirtualize statically (the codegen resolves the
//! one method a call can target). But when two types define a method with the
//! same name *and* arity (e.g. an interface both `Dog` and `Cat` implement),
//! the target depends on the receiver's runtime type. For those, codegen emits a
//! `(type-name, method-name)` lookup here, then an indirect call.
//!
//! The table is populated once at startup (before `main`'s body, single-threaded)
//! by `jrt_method_register` calls the codegen emits, then only read — so a plain
//! `Mutex<Vec<…>>` is sufficient (lookups take the lock but never contend during
//! registration).

use core::ffi::{c_char, c_void};
use std::sync::Mutex;

use crate::sys::strlen;
use crate::value::JadeValue;

struct Entry {
    type_name: String,
    method: String,
    fnptr: usize,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

#[inline]
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let n = strlen(p as *const u8);
        String::from_utf8_lossy(core::slice::from_raw_parts(p as *const u8, n)).into_owned()
    }
}

/// Register that struct type `type_name`'s method `method` is implemented by
/// `fnptr` (a `jf_<uid>` address). Called once per extend method at startup.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_method_register(type_name: *const c_char, method: *const c_char, fnptr: *const c_void) {
    let (t, m) = unsafe { (cstr(type_name), cstr(method)) };
    if let Ok(mut reg) = REGISTRY.lock() {
        reg.push(Entry { type_name: t, method: m, fnptr: fnptr as usize });
    }
}

/// Look up the implementation of `method` for the struct whose type name is in
/// `type_word` (a tagged string, from `jrt_get_type_name`). Returns the `jf_<uid>`
/// pointer, or null if none is registered (the caller raises).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_method_lookup(type_word: i64, method: *const c_char) -> *const c_void {
    let tn = {
        let v = JadeValue::from_bits(type_word as u64);
        if v.is_str() {
            unsafe { cstr(v.as_ptr() as *const c_char) }
        } else {
            String::new()
        }
    };
    let m = unsafe { cstr(method) };
    if let Ok(reg) = REGISTRY.lock() {
        for e in reg.iter() {
            if e.type_name == tn && e.method == m {
                return e.fnptr as *const c_void;
            }
        }
    }
    core::ptr::null()
}
