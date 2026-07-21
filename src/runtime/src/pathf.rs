//! `std::path` — the single implementation of the `path` stdlib, shared by both
//! engines. The neutral `pub fn` cores use `std::path` (so the VM's behavior is
//! the canonical one — `join` variadic, `abs` normalizing `.`/`..`); the VM
//! (`src/path/mod.rs`) and the AOT `#[no_mangle]` wrappers below both call them.
//! Trust is propagated from the input path, applied only in the AOT wrappers.

use core::ffi::c_char;
use std::path::{Path, PathBuf};

use crate::string::trust_of;
use crate::cstr;

// ── Neutral cores (std::path; used by both engines) ───────────────────────────

/// `path.join(segments...)` — join two or more segments (an absolute later
/// segment replaces the accumulated path, per `PathBuf::push`).
pub fn join(segments: &[&str]) -> String {
    let mut p = PathBuf::from(segments.first().copied().unwrap_or(""));
    for s in &segments[1..] {
        p.push(s);
    }
    p.to_string_lossy().into_owned()
}

/// `path.basename(p)` — the final component (filename+ext), or "".
pub fn basename(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `path.dirname(p)` — the parent directory; "." for a bare filename.
pub fn dirname(p: &str) -> String {
    Path::new(p)
        .parent()
        .map(|d| {
            let s = d.to_string_lossy();
            if s.is_empty() { ".".to_string() } else { s.into_owned() }
        })
        .unwrap_or_else(|| ".".to_string())
}

/// `path.ext(p)` — the extension including the dot (e.g. ".rs"), or `None`.
pub fn ext(p: &str) -> Option<String> {
    Path::new(p).extension().map(|e| format!(".{}", e.to_string_lossy()))
}

/// `path.stem(p)` — the filename without its final extension, or "".
pub fn stem(p: &str) -> String {
    Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `path.abs(p)` — the absolute path (lexical; `.`/`..` normalized by
/// `std::path::absolute`; symlinks not resolved). `Err` if the cwd is unavailable.
pub fn abs(p: &str) -> std::io::Result<String> {
    std::path::absolute(p).map(|a| a.to_string_lossy().into_owned())
}

/// `path.is_abs(p)` — whether the path begins at the filesystem root.
pub fn is_abs(p: &str) -> bool {
    Path::new(p).is_absolute()
}

// ── AOT C-ABI wrappers (apply trust; the cores are trust-agnostic) ────────────

fn trust(p: *const c_char) -> u8 {
    trust_of(p as *const u8)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_basename(p: *const c_char) -> *mut c_char {
    unsafe { cstr::emit(basename(cstr::borrow(p)).as_bytes(), trust(p)) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_ext(p: *const c_char) -> *mut c_char {
    match ext(unsafe { cstr::borrow(p) }) {
        Some(e) => cstr::emit(e.as_bytes(), trust(p)),
        None => core::ptr::null_mut(),
    }
}

/// The AOT-facing join is binary (codegen emits `path.join(a, b)`); the neutral
/// core is variadic and serves the VM's variadic `path.join`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_join(a: *const c_char, b: *const c_char) -> *mut c_char {
    let t = trust(a) | trust(b);
    let s = join(&[unsafe { cstr::borrow(a) }, unsafe { cstr::borrow(b) }]);
    cstr::emit(s.as_bytes(), t)
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_dirname(p: *const c_char) -> *mut c_char {
    unsafe { cstr::emit(dirname(cstr::borrow(p)).as_bytes(), trust(p)) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_stem(p: *const c_char) -> *mut c_char {
    unsafe { cstr::emit(stem(cstr::borrow(p)).as_bytes(), trust(p)) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_abs(p: *const c_char) -> *mut c_char {
    let s = abs(unsafe { cstr::borrow(p) }).unwrap_or_else(|_| unsafe { cstr::borrow(p) }.to_owned());
    cstr::emit(s.as_bytes(), trust(p))
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_is_abs(p: *const c_char) -> i32 {
    i32::from(is_abs(unsafe { cstr::borrow(p) }))
}
