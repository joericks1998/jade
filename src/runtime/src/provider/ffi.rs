//! The C surface over the active-provider slot, called from
//! `runtime_aot/infer/infer.c`. These only *resolve* the slot; the C runtime does
//! the loading and driving (via `jrt_native_load`/`jrt_native_call`), so nothing
//! about the provider ABI lives on the Rust side of the compiled path.

use core::ffi::c_char;

use crate::sys;

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

/// Whether an active provider is installed — the compiled binary's cue to drive a
/// provider in-process rather than connect to the daemon. Cheap: a directory check.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_available() -> i32 {
    if super::is_active() {
        1
    } else {
        0
    }
}

/// The active provider `.so`'s absolute path as a `malloc`'d NUL-terminated string
/// the caller frees, or null when no provider is active.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_active_lib_path() -> *mut c_char {
    match super::active_lib_path() {
        Some(p) => to_c_buffer(p.to_string_lossy().as_bytes()),
        None => core::ptr::null_mut(),
    }
}

/// The active provider's config blob (`config.json`) as a `malloc`'d
/// NUL-terminated string the caller frees, or null when there is none.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_provider_active_config() -> *mut c_char {
    let cfg = super::active_config();
    if cfg.is_empty() {
        core::ptr::null_mut()
    } else {
        to_c_buffer(&cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

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

    /// A config blob with an embedded NUL keeps its full length in the buffer —
    /// the terminator goes past the body, it does not replace it.
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
}
