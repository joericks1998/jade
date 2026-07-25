//! The C surface over the active-provider slot, called from
//! `runtime_aot/infer/infer.c`. These only *resolve* the slot; the C runtime does
//! the loading and driving (via `jrt_native_load`/`jrt_native_call`), so nothing
//! about the provider ABI lives on the Rust side of the compiled path.

use core::ffi::c_char;

use crate::infer::ffi::to_c_buffer;

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
