//! The `jrt_ipc_*` C entry points, called from `runtime_aot/infer/infer.c`.
//!
//! These keep the exact signatures `runtime_aot/ipc/ipc.h` declares, so
//! `infer.c` is unchanged by the move from C to Rust — it still hands over a
//! JSON body and gets back a `malloc`'d NUL-terminated buffer it frees itself.
//!
//! ## Where the errors go
//!
//! A compiled Jade binary has no interpreter to unwind into, so — as the C
//! implementation did — any failure prints to stderr and exits 1. No partial
//! response is ever handed back: a caller that gets a buffer got a complete
//! one. The VM takes the same [`super::conn`] code and maps failures into
//! catchable `JadeError`s instead, which is why the exiting lives here at the
//! C edge rather than in the shared layer.

use core::ffi::{c_char, c_void};
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

use super::conn::{self, Mode};
use super::InferError;
use crate::sys;

/// Per-token callback: `(bytes, len, user)`. Bytes are **not** NUL-terminated.
pub type TokenCb = Option<unsafe extern "C" fn(*const c_char, usize, *mut c_void)>;

/// Print and exit, the way the C runtime did. Never returns.
fn fatal(e: InferError) -> ! {
    eprintln!("jade: {e}");
    std::process::exit(1)
}

/// Copy `bytes` into a `malloc`'d, NUL-terminated buffer.
///
/// `malloc` and not a Rust allocation because `infer.c` releases these with
/// `free()`. Mixing allocators here would be a heap corruption that only shows
/// up under load.
fn to_c_buffer(bytes: &[u8]) -> *mut c_char {
    let p = unsafe { sys::malloc(bytes.len() + 1) };
    if p.is_null() {
        sys::oom();
    }
    unsafe {
        if !bytes.is_empty() {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        }
        *p.add(bytes.len()) = 0;
    }
    p as *mut c_char
}

/// Run one exchange and write the results through the caller's out-pointers.
fn dispatch(
    req_json: *const c_void,
    req_len: usize,
    mode: Mode,
    on_token: Option<&mut dyn FnMut(&[u8])>,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
    // Safety: the caller passes a buffer of `req_len` bytes, valid for this call.
    let req = if req_json.is_null() || req_len == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(req_json as *const u8, req_len) }
    };

    let resp = match conn::shared().request(req, mode, on_token) {
        Ok(r) => r,
        Err(e) => fatal(e),
    };

    unsafe {
        if !resp_out.is_null() {
            *resp_out = to_c_buffer(&resp.body);
        }
        if !resp_len_out.is_null() {
            *resp_len_out = resp.body.len();
        }
        if !tokens_used_out.is_null() {
            *tokens_used_out = resp.tokens_used;
        }
    }
}

/// Send a request and accumulate TOKEN frames until DONE.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_ipc_request(
    req_json: *const c_void,
    req_len: usize,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
    dispatch(req_json, req_len, Mode::Tokens, None, resp_out, resp_len_out, tokens_used_out);
}

/// [`jrt_ipc_request`], plus `on_token` per TOKEN frame as it arrives.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_ipc_request_streaming(
    req_json: *const c_void,
    req_len: usize,
    on_token: TokenCb,
    user: *mut c_void,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
    // The C callback is wrapped rather than passed through so the shared layer
    // never has to know about function pointers or `void*` context.
    let mut wrapper = on_token.map(|cb| {
        move |bytes: &[u8]| unsafe { cb(bytes.as_ptr() as *const c_char, bytes.len(), user) }
    });
    let dynamic: Option<&mut dyn FnMut(&[u8])> = match wrapper.as_mut() {
        Some(f) => Some(f),
        None => None,
    };
    dispatch(req_json, req_len, Mode::Tokens, dynamic, resp_out, resp_len_out, tokens_used_out);
}

/// [`jrt_ipc_request`] but accumulating `0x05 JSON` frames instead of tokens.
/// Used by structured operations such as `llm.health()`.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_ipc_request_json(
    req_json: *const c_void,
    req_len: usize,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
) {
    dispatch(req_json, req_len, Mode::Json, None, resp_out, resp_len_out, core::ptr::null_mut());
}

/// Close the connection. The next request reconnects.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_ipc_shutdown() {
    conn::shared().shutdown();
}

/// The model named in the daemon's most recent META frame, or `""` before any
/// request. Backs `__model__`.
///
/// The returned pointer stays valid for the process lifetime. The C version
/// returned a fixed 128-byte static, silently truncating longer model names;
/// this allocates to fit. A superseded name is leaked rather than freed —
/// a caller may still be holding it, and the model changes at most once or
/// twice per run, so the total is a handful of bytes.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_reported_model() -> *const c_char {
    static CACHED: AtomicPtr<c_char> = AtomicPtr::new(core::ptr::null_mut());

    let want = conn::shared().reported_model();

    let current = CACHED.load(Ordering::Acquire);
    if !current.is_null() {
        // Safety: only ever set to a live `CString::into_raw`, never freed.
        let existing = unsafe { CStr::from_ptr(current) };
        if existing.to_bytes() == want.as_bytes() {
            return current;
        }
    }

    // A model name with an interior NUL is not a name we can hand to C; an
    // empty string is the same answer as "no model reported yet".
    let owned = CString::new(want).unwrap_or_default();
    let raw = owned.into_raw();
    CACHED.store(raw, Ordering::Release);
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_buffers_are_nul_terminated() {
        let p = to_c_buffer(b"hi");
        unsafe {
            assert_eq!(CStr::from_ptr(p).to_str().unwrap(), "hi");
            sys::free(p as *mut u8);
        }
    }

    #[test]
    fn an_empty_body_still_yields_a_valid_c_string() {
        let p = to_c_buffer(b"");
        unsafe {
            assert_eq!(CStr::from_ptr(p).to_bytes().len(), 0);
            sys::free(p as *mut u8);
        }
    }

    /// A body with an embedded NUL keeps its full length — `resp_len_out` is
    /// what `infer.c` copies with, not `strlen`.
    #[test]
    fn embedded_nuls_do_not_truncate_the_buffer() {
        let body = b"a\0b";
        let p = to_c_buffer(body);
        unsafe {
            assert_eq!(core::slice::from_raw_parts(p as *const u8, 3), body);
            assert_eq!(*p.add(3), 0, "terminator written past the body");
            sys::free(p as *mut u8);
        }
    }

    #[test]
    fn reported_model_is_empty_before_any_request() {
        let p = jrt_reported_model();
        assert!(!p.is_null());
        assert_eq!(unsafe { CStr::from_ptr(p) }.to_bytes(), b"");
    }
}
