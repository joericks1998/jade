//! `std::fs` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/fs/fs.c`. read/write/append/exists/delete/mkdir. (`list_dir`
//! already lives in `ffi_coll.rs`.)
//!
//! ## Raising across the Rust boundary
//!
//! A Jade exception is a `longjmp`, which must not cross a Rust frame. So the
//! fallible ops here never throw: on error they record a message in a
//! thread-local and return a sentinel (null / no-op). A thin C forwarder in
//! `common.c` (which also performs `fs.read`'s tainted-path refusal) calls the
//! impl, then [`jrt_fs_take_error`] and throws if one is pending. `exists` never
//! fails and exports directly.

use core::ffi::c_char;
use std::cell::Cell;

use crate::string::{self, TAINTED, TRUSTED};
use crate::sys::strlen;

thread_local! {
    /// A pending fs error message as an owned, leaked tagged string (TRUSTED), or
    /// null. Set by a failing op, drained by the C forwarder via
    /// [`jrt_fs_take_error`].
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

unsafe fn mk_str(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

unsafe fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, n)).unwrap_or("")
    }
}

/// Record `<op> '<path>': <err>` as the pending error (matching the C `fs_raise`
/// shape — `std::io::Error`'s Display already ends with "(os error N)").
fn set_err(op: &str, path: &str, err: &std::io::Error) {
    let msg = format!("{op} '{path}': {err}");
    let s = unsafe { mk_str(msg.as_bytes(), TRUSTED) };
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            unsafe { string::free_str(old as *mut u8) };
        }
    });
}

/// Drain the pending fs error (a tagged string the caller owns), or null if none.
/// The C forwarder throws it when non-null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// `fs.exists(path)` — never raises.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_exists(path: *const c_char) -> i32 {
    if path.is_null() {
        return 0;
    }
    i32::from(std::path::Path::new(unsafe { cstr(path) }).exists())
}

/// `fs.read(path, trust)` core (the forwarder handles the tainted-path refusal).
/// `trust != 0` → the operator asserts the file is trusted, so content is TRUSTED;
/// else TAINTED. On error records the pending error and returns null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_read_impl(path: *const c_char, trust: i32) -> *mut c_char {
    let tag = if trust != 0 { TRUSTED } else { TAINTED };
    let p = unsafe { cstr(path) };
    match std::fs::read(p) {
        Ok(bytes) => unsafe { mk_str(&bytes, tag) },
        Err(e) => {
            set_err("read", p, &e);
            core::ptr::null_mut()
        }
    }
}

/// `fs.write(path, content)` — truncating. Records a pending error on failure.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_write_impl(path: *const c_char, content: *const c_char) {
    let p = unsafe { cstr(path) };
    if let Err(e) = std::fs::write(p, unsafe { cstr(content) }.as_bytes()) {
        set_err("write", p, &e);
    }
}

/// `fs.append(path, content)` — create if absent.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_append_impl(path: *const c_char, content: *const c_char) {
    let p = unsafe { cstr(path) };
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .and_then(|mut f| std::io::Write::write_all(&mut f, unsafe { cstr(content) }.as_bytes()));
    if let Err(e) = r {
        set_err("append", p, &e);
    }
}

/// `fs.delete(path)` — remove a file.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_delete_impl(path: *const c_char) {
    let p = unsafe { cstr(path) };
    if let Err(e) = std::fs::remove_file(p) {
        set_err("delete", p, &e);
    }
}

/// `fs.mkdir(path)` — recursively create `path` and parents (idempotent).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_mkdir_impl(path: *const c_char) {
    let p = unsafe { cstr(path) };
    if let Err(e) = std::fs::create_dir_all(p) {
        set_err("mkdir", p, &e);
    }
}
