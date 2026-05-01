//! jade_infer — C-callable runtime shim for `?prompt` in LLVM-compiled binaries.
//!
//! This module is compiled into `libJadeRuntime.a` (alongside the async and dict
//! runtime helpers) and linked into every LLVM-compiled Jade binary that uses
//! `?prompt`.
//!
//! When a compiled Jade program evaluates `?prompt` at runtime it calls:
//!
//! ```c
//! char* jade_infer(
//!     const char* prompt,  size_t prompt_len,
//!     const char* model,   size_t model_len,
//!     uint32_t    max_tokens,
//! );
//! ```
//!
//! The function opens `/dev/jade`, writes an `InferenceRequest`, reads streaming
//! `Frame` responses until `DONE` or `ERROR`, and returns a heap-allocated
//! null-terminated string containing the concatenated token text.  The caller is
//! responsible for freeing the returned pointer.
//!
//! On dev machines without `/dev/jade` the function returns an empty string rather
//! than aborting — the interpreter path is preferred for development anyway.

use std::alloc::{alloc, dealloc, Layout};
use std::io::{Read, Write};
use std::fs::OpenOptions;

use jade_protocol::{Frame, FrameError, InferenceRequest, Message};

/// Open `/dev/jade`, issue the request, and return the assembled response as a
/// heap-allocated, null-terminated C string.  Returns an empty string on error.
///
/// # Safety
/// `prompt` and `model` must be valid non-null pointers to at least
/// `prompt_len` / `model_len` bytes of UTF-8 data respectively.  The returned
/// pointer must be freed with `jade_infer_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jade_infer(
    prompt: *const u8,
    prompt_len: usize,
    model: *const u8,
    model_len: usize,
    max_tokens: u32,
) -> *mut u8 {
    // SAFETY: caller guarantees prompt/model are valid UTF-8 slices.
    let result = unsafe {
        jade_infer_inner(prompt, prompt_len, model, model_len, max_tokens)
    }.unwrap_or_default();

    // Allocate a null-terminated copy.
    let len = result.len();
    let layout = Layout::array::<u8>(len + 1).expect("layout overflow");
    // SAFETY: layout is valid and non-zero.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        return ptr;
    }
    // SAFETY: ptr is valid and result.len() bytes were allocated.
    unsafe {
        std::ptr::copy_nonoverlapping(result.as_ptr(), ptr, len);
        *ptr.add(len) = 0;
    }
    ptr
}

/// Free a string returned by `jade_infer`.
///
/// # Safety
/// `ptr` must have been returned by `jade_infer` and not previously freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jade_infer_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() { return; }
    let layout = Layout::array::<u8>(len + 1).expect("layout overflow");
    // SAFETY: caller guarantees ptr/len match a prior jade_infer call.
    unsafe { dealloc(ptr, layout) };
}

// ── Internal implementation ───────────────────────────────────────────────────

unsafe fn jade_infer_inner(
    prompt: *const u8,
    prompt_len: usize,
    model: *const u8,
    model_len: usize,
    max_tokens: u32,
) -> Option<String> {
    // SAFETY: caller guarantees valid UTF-8 slices.
    let prompt_str = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(prompt, prompt_len)).ok()?
    };
    let model_str = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(model, model_len)).ok()?
    };

    let req = InferenceRequest {
        prompt:     prompt_str.to_owned(),
        model:      model_str.to_owned(),
        history:    Vec::<Message>::new(),
        max_tokens,
    };

    let payload = req.encode().ok()?;

    let mut dev = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/jade")
        .ok()?;

    dev.write_all(&payload).ok()?;

    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut text = String::new();

    loop {
        match Frame::decode(&buf) {
            Ok((Frame::Token(token), consumed)) => {
                text.push_str(&token);
                buf.drain(..consumed);
            }
            Ok((Frame::Done { .. }, consumed)) => {
                buf.drain(..consumed);
                return Some(text);
            }
            Ok((Frame::Error(msg), consumed)) => {
                buf.drain(..consumed);
                // Surface the error message as the return value so the program
                // can at least see something went wrong.
                return Some(format!("<jade_infer error: {msg}>"));
            }
            Err(FrameError::Incomplete) => {
                let n = dev.read(&mut tmp).ok()?;
                if n == 0 {
                    // Device closed early; return whatever we have.
                    return Some(text);
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(e) => {
                return Some(format!("<jade_infer frame error: {e}>"));
            }
        }
    }
}
