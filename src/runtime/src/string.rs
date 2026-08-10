//! The tagged-string allocator.
//!
//! Every Jade-visible string carries a **trust tag** in the byte at offset `-1`
//! relative to its data pointer. Because pointer tagging needs 8-aligned data
//! pointers, strings use an **8-byte header**: `jrt_str_new` returns `malloc+8`
//! and `jrt_str_free` frees `data-8`; the trust byte lives at `data[-1]`
//! (`header[7]`). This is byte-identical to `runtime.h`'s tagged-string ABI and
//! to codegen's pre-tagged string-literal globals.
//!
//! The header's first four bytes are a **reference count**. Strings used to be
//! left out of refcounting entirely — a leaf that cannot form a cycle, so it was
//! deferred — which meant a compiled program never reclaimed one: a loop
//! building strings grew without bound even with no FFI in sight. Codegen has
//! always emitted balanced retain/release for every heap word including these,
//! so honouring them here was all that was missing.
//!
//! A literal is [`IMMORTAL`]. Codegen emits it as a `constant` global in
//! read-only memory, so its count cannot be written at all, and both [`incref`]
//! and [`decref`] check for the marker before touching the word. That is what
//! lets a slot be released at scope exit without caring whether the string in it
//! was allocated or baked into the binary.
//!
//! Allocations go through the **system** allocator ([`crate::sys`]) so a string
//! made here is freeable by the C runtime and vice versa, and so codegen's
//! literal globals (never freed) coexist with heap strings under one ABI.
//!
//! Moving this here makes strings fully Rust-native: the dynamic `+` concat no
//! longer calls back into C, so `jade_runtime` depends on no C symbol at all
//! (only libc) — the archive dependency is cleanly one-directional.

use crate::sys::{free, malloc, oom, strlen};

/// The refcount of a string that must never be freed.
///
/// Codegen emits every literal as a `constant` global in read-only memory, so
/// its count cannot even be *written*, let alone dropped to zero. The marker is
/// baked into the initializer and both [`incref`] and [`decref`] check for it
/// before touching the word.
pub const IMMORTAL: u32 = u32::MAX;

/// Read the refcount word at `data[-8]`.
///
/// # Safety
/// `s` is a tagged-string data pointer with its 8-byte header intact.
#[inline]
unsafe fn rc_ptr(s: *const u8) -> *mut u32 {
    unsafe { s.sub(8) as *mut u32 }
}

/// Retain a string. A literal is immortal and is left alone.
///
/// # Safety
/// `s` is null or a tagged-string data pointer.
#[inline]
pub fn incref(s: *const u8) {
    if s.is_null() {
        return;
    }
    unsafe {
        let rc = rc_ptr(s);
        if *rc != IMMORTAL {
            *rc += 1;
        }
    }
}

/// Release a string, freeing it at zero. A literal is immortal and is left alone.
///
/// # Safety
/// `s` is null or a tagged-string data pointer this module allocated (or a
/// literal global, which is skipped).
#[inline]
pub fn decref(s: *mut u8) {
    if s.is_null() {
        return;
    }
    unsafe {
        let rc = rc_ptr(s);
        if *rc == IMMORTAL {
            return;
        }
        // Saturating rather than wrapping: an extra release is a bug, and
        // freeing at 0 twice is a use-after-free rather than a leak.
        if *rc <= 1 {
            *rc = 0;
            free_str(s);
        } else {
            *rc -= 1;
        }
    }
}

/// Trust tag: derived purely from trusted data (source literals, etc.).
pub const TRUSTED: u8 = 0;
/// Trust tag: originated from an LLM, network, file, shell, or stdin.
pub const TAINTED: u8 = 1;

/// Allocate `len` data bytes tagged `trust`. The returned pointer is 8-aligned
/// (an 8-byte header precedes it); the trust byte is at `data[-1]` and a NUL is
/// written at `data[len]`. Aborts on allocation failure. Mirrors C `jrt_str_new`.
#[inline]
pub fn new(len: usize, trust: u8) -> *mut u8 {
    unsafe {
        let raw = malloc(8 + len + 1);
        if raw.is_null() {
            oom();
        }
        let data = raw.add(8);
        // `malloc` leaves the header uninitialised, so the count is written
        // before anything can read it. One, because the caller owns what it
        // just made: a producer hands back a reference, and the slot it lands
        // in releases that one at scope exit.
        *(raw as *mut u32) = 1;
        *data.sub(1) = trust; // data[-1] = trust (header[7])
        *data.add(len) = 0; // NUL terminator
        data
    }
}

/// Read a tagged string's trust byte. NULL → [`TRUSTED`]. Mirrors C `jrt_trust_of`.
///
/// # Safety
/// `s` is null or a tagged-string data pointer (trust byte readable at `s[-1]`).
#[inline]
pub fn trust_of(s: *const u8) -> u8 {
    if s.is_null() { TRUSTED } else { unsafe { *s.sub(1) } }
}

/// Allocate a tagged copy of a NUL-terminated string. Mirrors C `jrt_str_dup`.
///
/// # Safety
/// `s` is null or a NUL-terminated readable string.
#[inline]
pub fn dup(s: *const u8, trust: u8) -> *mut u8 {
    unsafe {
        let n = if s.is_null() { 0 } else { strlen(s) };
        let out = new(n, trust);
        if n > 0 {
            core::ptr::copy_nonoverlapping(s, out, n);
        }
        out
    }
}

/// Free a string previously returned by [`new`]/[`dup`]/[`concat`]. Frees
/// `s-8` (the raw allocation). Mirrors C `jrt_str_free`.
///
/// # Safety
/// `s` is null or a pointer returned by this module (never a literal global).
#[inline]
pub fn free_str(s: *mut u8) {
    if !s.is_null() {
        unsafe { free(s.sub(8)) }
    }
}

/// Concatenate two tagged strings; trust = max (bitwise OR) of the inputs.
/// Mirrors C `jrt_str_concat`.
///
/// # Safety
/// `a`/`b` are null or tagged-string data pointers.
#[inline]
pub fn concat(a: *const u8, b: *const u8) -> *mut u8 {
    unsafe {
        let la = if a.is_null() { 0 } else { strlen(a) };
        let lb = if b.is_null() { 0 } else { strlen(b) };
        let t = trust_of(a) | trust_of(b);
        let out = new(la + lb, t);
        if la > 0 {
            core::ptr::copy_nonoverlapping(a, out, la);
        }
        if lb > 0 {
            core::ptr::copy_nonoverlapping(b, out.add(la), lb);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_string_is_owned_by_whoever_made_it() {
        let p = new(3, TRUSTED);
        assert_eq!(unsafe { *rc_ptr(p) }, 1, "a producer hands back one reference");
        decref(p); // frees
    }

    #[test]
    fn retain_and_release_balance() {
        let p = new(4, TRUSTED);
        incref(p);
        incref(p);
        assert_eq!(unsafe { *rc_ptr(p) }, 3);
        decref(p);
        decref(p);
        assert_eq!(unsafe { *rc_ptr(p) }, 1, "still held by the original owner");
        decref(p); // frees
    }

    #[test]
    fn a_literal_is_never_counted_and_never_freed() {
        // The shape codegen bakes into a read-only global: the marker, three
        // pad bytes, the trust byte, then the text. Releasing it must be inert
        // — a slot holding a literal is released at scope exit like any other.
        let mut lit = Vec::new();
        lit.extend_from_slice(&IMMORTAL.to_ne_bytes());
        lit.extend_from_slice(&[0u8; 3]);
        lit.push(TRUSTED);
        lit.extend_from_slice(b"hi\0");
        let data = unsafe { lit.as_mut_ptr().add(8) };
        incref(data);
        decref(data);
        decref(data);
        assert_eq!(unsafe { *rc_ptr(data) }, IMMORTAL, "an immortal count never moves");
        assert_eq!(trust_of(data), TRUSTED, "and the trust byte is untouched");
    }

    #[test]
    fn the_count_does_not_disturb_the_trust_byte() {
        let p = new(2, TAINTED);
        incref(p);
        assert_eq!(trust_of(p), TAINTED);
        decref(p);
        assert_eq!(trust_of(p), TAINTED);
        decref(p);
    }

    unsafe fn as_str(p: *const u8) -> String {
        unsafe {
            let n = strlen(p);
            let bytes = core::slice::from_raw_parts(p, n).to_vec();
            String::from_utf8(bytes).unwrap()
        }
    }

    #[test]
    fn new_carries_trust_and_nul() {
        let p = new(3, TAINTED);
        unsafe {
            assert_eq!(*p.sub(1), TAINTED); // trust at data[-1]
            assert_eq!(*p.add(3), 0); // NUL at data[len]
            assert_eq!(p as usize & 7, 0); // 8-aligned
            free_str(p);
        }
    }

    #[test]
    fn dup_copies_and_tags() {
        let src = b"hello\0";
        let p = dup(src.as_ptr(), TRUSTED);
        unsafe {
            assert_eq!(as_str(p), "hello");
            assert_eq!(trust_of(p), TRUSTED);
            free_str(p);
        }
    }

    #[test]
    fn concat_joins_and_maxes_trust() {
        let a = dup(b"foo\0".as_ptr(), TRUSTED);
        let b = dup(b"bar\0".as_ptr(), TAINTED);
        let c = concat(a, b);
        unsafe {
            assert_eq!(as_str(c), "foobar");
            assert_eq!(trust_of(c), TAINTED); // max(TRUSTED, TAINTED)
            free_str(a);
            free_str(b);
            free_str(c);
        }
    }

    #[test]
    fn trust_of_null_is_trusted() {
        assert_eq!(trust_of(core::ptr::null()), TRUSTED);
    }
}
