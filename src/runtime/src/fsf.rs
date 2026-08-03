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

/// Read all of stdin as raw octets.
///
/// The byte twin of `input()`, which reads one line and decodes it as UTF-8.
/// A program sitting in a binary pipeline (`cat img.png | jade run f.jde`)
/// needs this: `input()` would stop at the first newline and reject the rest.
pub fn read_stdin_bytes() -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Write raw octets to stdout, unbuffered by the caller's next flush.
pub fn write_stdout_bytes(data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let out = std::io::stdout();
    let mut h = out.lock();
    h.write_all(data)?;
    h.flush()
}

/// Read `path` as raw octets.
///
/// The byte-oriented twin of [`read`]. `read` goes through
/// `read_to_string`, which rejects anything that is not valid UTF-8 — so
/// reading a PNG or a gzip stream needs this, not that.
pub fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// Write raw octets to `path`, truncating.
pub fn write_bytes(path: &str, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// Append raw octets to `path`, creating it if absent.
pub fn append_bytes(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(data)
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

/// `fs.read_stdin_bytes()` core. Null + pending error on failure.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_read_stdin_bytes_impl() -> *mut core::ffi::c_void {
    match read_stdin_bytes() {
        Ok(d) => crate::gc::leak_obj(crate::bytesf::BytesObj::new(d, TAINTED)),
        Err(e) => {
            set_err("read_stdin_bytes", "<stdin>", &e);
            core::ptr::null_mut()
        }
    }
}

/// # Safety
/// `data` must point at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_fs_write_stdout_bytes_impl(data: *const u8, len: usize) {
    let slice = if data.is_null() || len == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(data, len) }
    };
    if let Err(e) = write_stdout_bytes(slice) {
        set_err("write_stdout_bytes", "<stdout>", &e);
    }
}

/// `fs.read_bytes(path)` core. Null + pending error on failure.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_fs_read_bytes_impl(path: *const c_char, trust: i32) -> *mut core::ffi::c_void {
    let tag = if trust != 0 { TRUSTED } else { TAINTED };
    let p = unsafe { cstr::borrow(path) };
    match read_bytes(p) {
        Ok(d) => crate::gc::leak_obj(crate::bytesf::BytesObj::new(d, tag)),
        Err(e) => {
            set_err("read_bytes", p, &e);
            core::ptr::null_mut()
        }
    }
}

/// # Safety
/// `data` must point at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_fs_write_bytes_impl(path: *const c_char, data: *const u8, len: usize) {
    let p = unsafe { cstr::borrow(path) };
    let slice = if data.is_null() || len == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(data, len) }
    };
    if let Err(e) = write_bytes(p, slice) {
        set_err("write_bytes", p, &e);
    }
}

/// # Safety
/// `data` must point at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_fs_append_bytes_impl(path: *const c_char, data: *const u8, len: usize) {
    let p = unsafe { cstr::borrow(path) };
    let slice = if data.is_null() || len == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(data, len) }
    };
    if let Err(e) = append_bytes(p, slice) {
        set_err("append_bytes", p, &e);
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
