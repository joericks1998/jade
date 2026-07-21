//! `std::fs` — the single implementation of the `fs` stdlib, shared by both
//! engines. Neutral `pub fn` cores return `std::io::Result`; the VM
//! (`src/fs/mod.rs`) maps `Err` to a `JadeError::IoError`, and the AOT wrappers
//! below record a thread-local pending error (a Jade exception is a `longjmp`
//! that must not cross a Rust frame) which a thin C forwarder in `common.c`
//! throws. The `"<op> '<path>': <e>"` message is assembled by each adapter, so it
//! is identical on both sides.

use core::ffi::c_char;
use std::cell::Cell;
use std::io::Write as _;

use crate::string::{self, TAINTED, TRUSTED};
use crate::cstr;

// ── Neutral cores (used by both engines) ──────────────────────────────────────

/// Read a file to a UTF-8 string (errors on non-UTF-8, matching the VM).
pub fn read(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// Write `content` to `path`, truncating.
pub fn write(path: &str, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)
}

/// Append `content` to `path` (create if absent).
pub fn append(path: &str, content: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(content.as_bytes())
}

/// Whether `path` exists.
pub fn exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// Remove a file.
pub fn delete(path: &str) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

/// Recursively create `path` and parents (idempotent).
pub fn mkdir(path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Directory entry names (OS enumeration order).
pub fn list_dir(path: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

// ── AOT C-ABI wrappers (pending-error channel; forwarders in common.c throw) ───

thread_local! {
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

/// Record `<op> '<path>': <err>` as the pending error (the VM formats the same
/// string in `io_err`).
fn set_err(op: &str, path: &str, err: &std::io::Error) {
    let s = cstr::emit(format!("{op} '{path}': {err}").as_bytes(), TRUSTED);
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            string::free_str(old as *mut u8);
        }
    });
}

/// Drain the pending fs error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// `fs.exists(path)` — never raises.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_exists(path: *const c_char) -> i32 {
    i32::from(exists(unsafe { cstr::borrow(path) }))
}

/// `fs.read(path, trust)` core (the forwarder handles the tainted-path refusal).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_read_impl(path: *const c_char, trust: i32) -> *mut c_char {
    let tag = if trust != 0 { TRUSTED } else { TAINTED };
    let p = unsafe { cstr::borrow(path) };
    match read(p) {
        Ok(s) => cstr::emit(s.as_bytes(), tag),
        Err(e) => {
            set_err("read", p, &e);
            core::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_write_impl(path: *const c_char, content: *const c_char) {
    let p = unsafe { cstr::borrow(path) };
    if let Err(e) = write(p, unsafe { cstr::borrow(content) }) {
        set_err("write", p, &e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_append_impl(path: *const c_char, content: *const c_char) {
    let p = unsafe { cstr::borrow(path) };
    if let Err(e) = append(p, unsafe { cstr::borrow(content) }) {
        set_err("append", p, &e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_delete_impl(path: *const c_char) {
    let p = unsafe { cstr::borrow(path) };
    if let Err(e) = delete(p) {
        set_err("delete", p, &e);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_mkdir_impl(path: *const c_char) {
    let p = unsafe { cstr::borrow(path) };
    if let Err(e) = mkdir(p) {
        set_err("mkdir", p, &e);
    }
}
