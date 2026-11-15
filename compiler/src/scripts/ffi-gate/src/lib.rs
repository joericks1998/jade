//! What the FFI gate calls: `regex`, and the system's zlib through `libz-sys`.
//!
//! Each function is a thin, safe wrapper with `#[jade::export]` on it, the way a
//! user would write one. `echo` and `words` exist for the long run, which calls
//! them hundreds of thousands of times to show that what each call hands over is
//! freed by its `__free` companion.

use libz_sys as z;
use regex::Regex;

/// Every match of a pattern, in order.
#[jade::export]
pub fn find_all(pattern: &str, text: &str) -> Result<Vec<String>, String> {
    let re = Regex::new(pattern).map_err(|e| e.to_string())?;
    Ok(re.find_iter(text).map(|m| m.as_str().to_string()).collect())
}

/// Whether a pattern matches anywhere in the text.
#[jade::export]
pub fn is_match(pattern: &str, text: &str) -> Result<bool, String> {
    Ok(Regex::new(pattern).map_err(|e| e.to_string())?.is_match(text))
}

/// Every match of a pattern replaced; `$1` names a group.
#[jade::export]
pub fn replace_all(pattern: &str, text: &str, with: &str) -> Result<String, String> {
    Ok(Regex::new(pattern).map_err(|e| e.to_string())?.replace_all(text, with).into_owned())
}

/// Compresses bytes with the system's zlib.
#[jade::export]
pub fn deflate(data: &[u8]) -> Result<Vec<u8>, String> {
    // SAFETY: `compressBound` only computes a size.
    let mut len = unsafe { z::compressBound(data.len() as z::uLong) };
    let mut out = vec![0u8; len as usize];
    // SAFETY: `out` has `len` writable bytes, which zlib overwrites with the
    // length it wrote.
    let code =
        unsafe { z::compress(out.as_mut_ptr(), &mut len, data.as_ptr(), data.len() as z::uLong) };
    if code != z::Z_OK {
        return Err(format!("zlib could not compress: status {code}"));
    }
    out.truncate(len as usize);
    Ok(out)
}

/// Decompresses what `deflate` made, which is at most 1 MiB here.
#[jade::export]
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut len: z::uLong = 1 << 20;
    let mut out = vec![0u8; len as usize];
    // SAFETY: as in `deflate`.
    let code =
        unsafe { z::uncompress(out.as_mut_ptr(), &mut len, data.as_ptr(), data.len() as z::uLong) };
    if code != z::Z_OK {
        return Err(format!("zlib could not decompress: status {code}"));
    }
    out.truncate(len as usize);
    Ok(out)
}

/// The version of the zlib the program uses.
#[jade::export]
pub fn zlib_version() -> String {
    // SAFETY: zlibVersion returns a static NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(z::zlibVersion()).to_string_lossy().into_owned() }
}

/// A copy of the text, allocated by Rust and freed by the companion.
#[jade::export]
pub fn echo(s: &str) -> String {
    s.to_string()
}

/// The words of a text: an array of strings, each its own allocation.
#[jade::export]
pub fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}
