//! Unicode-correct `string.*` case operations, shared by both engines.
//!
//! These were implemented twice: the VM used Rust's `to_uppercase`/
//! `to_lowercase` while the AOT backend used C's byte-wise `toupper`/`tolower`
//! (`common.c`). The C versions are ASCII-only and, worse, assume the result is
//! the same byte length as the input, which some case mappings are not. Every
//! non-ASCII string differed between the engines:
//!
//! | expression        | `jade run` | `jade build` |
//! |-------------------|------------|--------------|
//! | `"café".upper()`  | `CAFÉ`     | `CAFé`       |
//! | `"größe".upper()` | `GRÖSSE`   | `GRößE`      |
//! | `"HÉLLO".lower()` | `héllo`    | `hÉllo`      |
//!
//! `ß` → `SS` is the case that breaks the C version structurally: one byte in,
//! two out. The Rust core allocates from the *result*, so it is not a special
//! case here — it just works.

use core::ffi::c_char;

use crate::string;
use crate::sys::strlen;

// ── Neutral cores ────────────────────────────────────────────────────────────

/// `s.upper()` — full Unicode uppercase, matching the VM.
pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `s.lower()` — full Unicode lowercase, matching the VM.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

// ── AOT C-ABI wrappers ───────────────────────────────────────────────────────

/// Borrow a tagged string as `&str`. Invalid UTF-8 is lossy-free here: Jade
/// strings are UTF-8 by construction, and a non-UTF-8 byte would already have
/// been a bug upstream.
///
/// # Safety
/// `p` must be NUL-terminated or null.
unsafe fn borrow<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    unsafe {
        let n = strlen(p as *const u8);
        core::str::from_utf8(core::slice::from_raw_parts(p as *const u8, n)).unwrap_or("")
    }
}

/// Allocate a tagged string holding `s`, preserving the input's trust byte.
///
/// # Safety
/// `src` must be a valid tagged string pointer or null.
unsafe fn emit(s: &str, src: *const c_char) -> *mut c_char {
    let trust = string::trust_of(src as *const u8);
    let out = string::new(s.len(), trust);
    if !s.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out, s.len()) };
    }
    out as *mut c_char
}

/// `s.upper()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_upper(s: *const c_char) -> *mut c_char {
    unsafe { emit(&upper(borrow(s)), s) }
}

/// `s.lower()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_lower(s: *const c_char) -> *mut c_char {
    unsafe { emit(&lower(borrow(s)), s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_matches_the_old_behavior() {
        assert_eq!(upper("hello"), "HELLO");
        assert_eq!(lower("HELLO"), "hello");
    }

    /// The divergence this module exists to remove. Byte-wise `toupper` left
    /// these untouched.
    #[test]
    fn non_ascii_case_maps_like_the_vm() {
        assert_eq!(upper("café"), "CAFÉ");
        assert_eq!(lower("HÉLLO"), "héllo");
    }

    /// One character in, two out — the case the C implementation could not
    /// express, since it wrote one output byte per input byte.
    #[test]
    fn case_mapping_may_change_character_count() {
        assert_eq!(upper("größe"), "GRÖSSE");
        assert_eq!(upper("ß"), "SS");
        assert_eq!(upper("ß").chars().count(), 2, "one char maps to two");
    }
}
