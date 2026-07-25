//! The `jrt_provider_*` C entry points, called from `runtime_aot/infer/infer.c`.
//!
//! These mirror the `jrt_ipc_*` daemon entry points ([`crate::infer::ffi`]) byte
//! for byte in signature, so `infer.c` routes to them with a one-line branch:
//! when an active provider is installed, a compiled binary drives it in-process
//! instead of talking to the daemon socket. Same contract — hand over a JSON
//! request body, get back a `malloc`'d NUL-terminated buffer the caller frees —
//! and the same "no interpreter to unwind into, so print and exit(1) on failure"
//! behaviour.

use core::ffi::{c_char, c_void};

use crate::infer::ffi::{to_c_buffer, TokenCb};

/// Print and exit, the way the C runtime and the daemon path do. Never returns.
fn fatal(msg: &str) -> ! {
    eprintln!("jade: {msg}");
    std::process::exit(1)
}

/// Borrow the request bytes the caller passed.
fn req_slice<'a>(req_json: *const c_void, req_len: usize) -> &'a [u8] {
    if req_json.is_null() || req_len == 0 {
        &[]
    } else {
        // SAFETY: the caller passes a buffer of `req_len` bytes, valid for this call.
        unsafe { core::slice::from_raw_parts(req_json as *const u8, req_len) }
    }
}

/// Write a driven response through the caller's out-pointers (mirrors the daemon
/// dispatch in `crate::infer::ffi`).
fn write_out(
    resp: crate::infer::Response,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
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

/// Whether an active provider is installed — the compiled binary's cue to drive
/// a provider in-process rather than connect to the daemon. Cheap (no `dlopen`).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_available() -> i32 {
    if super::is_active() {
        1
    } else {
        0
    }
}

/// Drive the active provider for one request, accumulating TOKEN frames until
/// DONE. Mirrors [`crate::infer::ffi::jrt_ipc_request`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_request(
    req_json: *const c_void,
    req_len: usize,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
    match super::run(req_slice(req_json, req_len), None) {
        Ok(resp) => write_out(resp, resp_out, resp_len_out, tokens_used_out),
        Err(e) => fatal(&e),
    }
}

/// [`jrt_provider_request`], plus `on_token` per TOKEN frame as it arrives.
/// Mirrors [`crate::infer::ffi::jrt_ipc_request_streaming`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_request_streaming(
    req_json: *const c_void,
    req_len: usize,
    on_token: TokenCb,
    user: *mut c_void,
    resp_out: *mut *mut c_char,
    resp_len_out: *mut usize,
    tokens_used_out: *mut u64,
) {
    // Wrap the C callback so the driver never sees function pointers or `void*`.
    let mut wrapper = on_token.map(|cb| {
        move |bytes: &[u8]| unsafe { cb(bytes.as_ptr() as *const c_char, bytes.len(), user) }
    });
    let dynamic: Option<&mut dyn FnMut(&[u8])> = match wrapper.as_mut() {
        Some(f) => Some(f),
        None => None,
    };
    match super::run(req_slice(req_json, req_len), dynamic) {
        Ok(resp) => write_out(resp, resp_out, resp_len_out, tokens_used_out),
        Err(e) => fatal(&e),
    }
}
