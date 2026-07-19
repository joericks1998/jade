//! `std::path` runtime module — the AOT C-ABI surface, ported from the C leaf
//! `runtime_lib/path/path.c`. Lexical path manipulation mirroring Rust's
//! `std::path` (the VM's `path_pkg.rs`); trust is propagated from the input. The
//! byte logic is translated verbatim from the C so difftest output is unchanged
//! (e.g. `path.abs` still does not lexically normalize `.`/`..` — a documented
//! divergence from `std::path::absolute`). None raise, so no C forwarder.

use core::ffi::c_char;

use crate::string::{self, trust_of};
use crate::sys::strlen;

/// Allocate a fresh tagged string holding `bytes` with `trust`; returns `char*`.
unsafe fn mk_str(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

/// Borrow a NUL-terminated C string's bytes (no trailing NUL); NULL → `&[]`.
unsafe fn cbytes<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(p as *const u8, strlen(p as *const u8)) }
}

/// Trust byte of a tagged string (NULL → TRUSTED).
fn trust(p: *const c_char) -> u8 {
    trust_of(p as *const u8)
}

/// Final path component (filename+ext), ignoring trailing slashes. Returns an
/// empty slice for "no file name" (`""`/`"."`/`".."`/trailing-slash-only),
/// matching Rust `Path::file_name -> None`.
fn path_filename(p: &[u8]) -> &[u8] {
    let mut len = p.len();
    while len > 0 && p[len - 1] == b'/' {
        len -= 1; // strip trailing '/'
    }
    let end = len;
    while len > 0 && p[len - 1] != b'/' {
        len -= 1; // back to last '/'
    }
    let s = &p[len..end];
    if s.is_empty() || s == b"." || s == b".." {
        return &[];
    }
    s
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_basename(p: *const c_char) -> *mut c_char {
    let t = trust(p);
    unsafe { mk_str(path_filename(cbytes(p)), t) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_ext(p: *const c_char) -> *mut c_char {
    if p.is_null() {
        return core::ptr::null_mut();
    }
    let name = unsafe { path_filename(cbytes(p)) };
    if name.is_empty() {
        return core::ptr::null_mut();
    }
    // Last '.' within the file name, not at index 0 (a leading dot is a
    // hidden-file marker, not an extension — matches Rust Path::extension).
    let mut dot = name.len();
    for i in (0..name.len()).rev() {
        if name[i] == b'.' {
            dot = i;
            break;
        }
    }
    if dot == name.len() || dot == 0 {
        return core::ptr::null_mut();
    }
    unsafe { mk_str(&name[dot..], trust(p)) } // includes the dot
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_join(a: *const c_char, b: *const c_char) -> *mut c_char {
    let t = trust(a) | trust(b);
    let (av, bv) = unsafe { (cbytes(a), cbytes(b)) };
    if !bv.is_empty() && bv[0] == b'/' {
        return unsafe { mk_str(bv, t) }; // absolute b replaces a
    }
    if av.is_empty() {
        return unsafe { mk_str(bv, t) };
    }
    let sep = *av.last().unwrap() != b'/'; // avoid doubling '/'
    let mut out = Vec::with_capacity(av.len() + sep as usize + bv.len());
    out.extend_from_slice(av);
    if sep {
        out.push(b'/');
    }
    out.extend_from_slice(bv);
    unsafe { mk_str(&out, t) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_dirname(p: *const c_char) -> *mut c_char {
    let t = trust(p);
    let bytes = unsafe { cbytes(p) };
    if bytes.is_empty() {
        return unsafe { mk_str(b".", t) };
    }
    let mut n = bytes.len();
    while n > 1 && bytes[n - 1] == b'/' {
        n -= 1; // strip trailing slashes
    }
    let mut slash = n;
    for i in (0..n).rev() {
        if bytes[i] == b'/' {
            slash = i;
            break;
        }
    }
    if slash == n {
        return unsafe { mk_str(b".", t) }; // no separator → "."
    }
    if slash == 0 {
        return unsafe { mk_str(b"/", t) }; // parent is the root
    }
    let mut end = slash;
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1; // collapse repeated seps
    }
    unsafe { mk_str(&bytes[..end], t) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_stem(p: *const c_char) -> *mut c_char {
    let t = trust(p);
    let name = unsafe { path_filename(cbytes(p)) };
    let n = name.len();
    let mut dot = n;
    for i in (0..n).rev() {
        if name[i] == b'.' {
            dot = i;
            break;
        }
    }
    let stemlen = if dot == n || dot == 0 { n } else { dot }; // no ext, or leading dot
    unsafe { mk_str(&name[..stemlen], t) }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_abs(p: *const c_char) -> *mut c_char {
    let t = trust(p);
    let bytes = unsafe { cbytes(p) };
    if !bytes.is_empty() && bytes[0] == b'/' {
        return unsafe { mk_str(bytes, t) };
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let cwd = cwd.to_string_lossy();
            let mut out = Vec::with_capacity(cwd.len() + 1 + bytes.len());
            out.extend_from_slice(cwd.as_bytes());
            out.push(b'/');
            out.extend_from_slice(bytes);
            unsafe { mk_str(&out, t) }
        }
        Err(_) => unsafe { mk_str(bytes, t) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn jrt_path_is_abs(p: *const c_char) -> i32 {
    let bytes = unsafe { cbytes(p) };
    i32::from(!bytes.is_empty() && bytes[0] == b'/')
}
