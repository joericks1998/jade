//! The C-string boundary: reading NUL-terminated strings in, writing tagged
//! strings out.
//!
//! Every `jrt_*` entry point takes `*const c_char` from generated code and most
//! hand back a tagged string, so almost every module in this crate needs these
//! two operations. They were written out per module instead — `mk_str` in six
//! files byte-for-byte identical, and `cstr` in eight, as **two different
//! functions sharing one name**:
//!
//! ```text
//!     -> &'static str   envf fsf httpf pathf shf timef   invalid UTF-8 -> ""
//!     -> String (lossy) coercef methods                  invalid UTF-8 -> U+FFFD
//! ```
//!
//! So the same call spelled the same way either dropped a string entirely or
//! preserved it, depending which file you were in. Nothing chose that; it is
//! what copy-paste does over time, and it is the shape of divergence this crate
//! exists to prevent.
//!
//! ## One rule for invalid input
//!
//! **Invalid UTF-8 truncates at the first bad byte; everything before it is
//! preserved.** One rule, one behaviour, no allocation.
//!
//! A Jade string is UTF-8 by construction — the lexer guarantees it — so bad
//! bytes only ever arrive from outside the program: a file, a shell command's
//! output, an environment variable. Discarding the whole string for one bad
//! byte is a silent wrong answer a caller cannot tell from a genuinely empty
//! value, which is what half these copies did.
//!
//! Full lossy repair (`U+FFFD` in place of each bad byte) preserves marginally
//! more, but it must allocate, which means returning `Cow` and a binding at
//! every one of the forty-odd call sites. That is a worse trade than losing a
//! malformed tail: it makes every caller noisier to buy fidelity on input that
//! is already broken. [`to_string`] is available where an owned copy is wanted.
//!
//! ## The lifetime
//!
//! The six `-> &'static str` copies fabricated a `'static` lifetime for memory
//! the *caller* owns — nothing stopped the borrow outliving the buffer. It
//! never bit because every caller happened to use the value immediately, which
//! is not a property anyone was checking. [`borrow`] ties the result to a
//! caller-chosen lifetime instead, so the borrow checker can see the constraint
//! the comments used to state.

use core::ffi::c_char;

use crate::string;
use crate::sys::strlen;

/// Borrow a NUL-terminated C string as text.
///
/// Invalid UTF-8 truncates at the first bad byte (see the module note); NULL is
/// empty. Never allocates.
///
/// # Safety
/// `p` is null, or points at a NUL-terminated buffer that outlives `'a`.
pub unsafe fn borrow<'a>(p: *const c_char) -> &'a str {
    let bytes = unsafe { borrow_bytes(p) };
    match core::str::from_utf8(bytes) {
        Ok(s) => s,
        // Safety: `valid_up_to()` is by definition the length of a valid prefix.
        Err(e) => unsafe { core::str::from_utf8_unchecked(&bytes[..e.valid_up_to()]) },
    }
}

/// Borrow a NUL-terminated C string as raw bytes, with no UTF-8 question at
/// all. For callers that are going to hand the bytes straight back out.
///
/// # Safety
/// `p` is null, or points at a NUL-terminated buffer that outlives `'a`.
pub unsafe fn borrow_bytes<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(p as *const u8, strlen(p as *const u8)) }
}

/// Read a NUL-terminated C string into an owned `String`, on the same terms as
/// [`borrow`]. Use `borrow` where the value need not outlive the call.
///
/// # Safety
/// `p` is null, or points at a NUL-terminated buffer valid for this call.
pub unsafe fn to_string(p: *const c_char) -> String {
    unsafe { borrow(p).to_owned() }
}

/// Allocate a tagged string holding `bytes` with the given trust.
///
/// The counterpart to [`borrow`]: this is how a `jrt_*` function hands a result
/// back to generated code. Returns the data pointer, NUL already written.
pub fn emit(bytes: &[u8], trust: u8) -> *mut c_char {
    let out = string::new(bytes.len(), trust);
    if !bytes.is_empty() {
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    out as *mut c_char
}

/// [`emit`] for text, which is the usual case.
pub fn emit_str(s: &str, trust: u8) -> *mut c_char {
    emit(s.as_bytes(), trust)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::{TAINTED, TRUSTED};

    /// A NUL-terminated buffer to read back, standing in for what generated
    /// code passes.
    fn buf(bytes: &[u8]) -> Vec<u8> {
        let mut v = bytes.to_vec();
        v.push(0);
        v
    }

    #[test]
    fn valid_text_round_trips() {
        let b = buf(b"hello");
        assert_eq!(unsafe { borrow(b.as_ptr() as *const c_char) }, "hello");
        assert_eq!(unsafe { to_string(b.as_ptr() as *const c_char) }, "hello");
    }

    #[test]
    fn null_is_empty_not_a_crash() {
        assert_eq!(unsafe { borrow(core::ptr::null()) }, "");
        assert_eq!(unsafe { to_string(core::ptr::null()) }, "");
        assert!(unsafe { borrow_bytes(core::ptr::null()) }.is_empty());
    }

    #[test]
    fn multibyte_text_is_preserved() {
        let b = buf("café → 日本語".as_bytes());
        assert_eq!(unsafe { borrow(b.as_ptr() as *const c_char) }, "café → 日本語");
    }

    /// The behaviour the two old copies disagreed about. Half the runtime
    /// returned `""` here, discarding a whole string because one byte was bad —
    /// a silent wrong answer indistinguishable from a genuinely empty value.
    #[test]
    fn invalid_utf8_keeps_the_valid_prefix() {
        let b = buf(b"ok\xffbad");
        let got = unsafe { borrow(b.as_ptr() as *const c_char) };
        assert_eq!(got, "ok", "the valid prefix survives, the rest truncates");
        assert_eq!(unsafe { to_string(b.as_ptr() as *const c_char) }, "ok",
                   "owned and borrowed must agree — one rule, not two");
    }

    /// A leading bad byte leaves nothing valid, which is the one case that
    /// legitimately yields empty.
    #[test]
    fn a_leading_bad_byte_yields_empty() {
        let b = buf(b"\xffrest");
        assert_eq!(unsafe { borrow(b.as_ptr() as *const c_char) }, "");
    }

    #[test]
    fn borrow_bytes_does_not_care_about_encoding() {
        let b = buf(b"\xff\xfe");
        assert_eq!(unsafe { borrow_bytes(b.as_ptr() as *const c_char) }, b"\xff\xfe");
    }

    #[test]
    fn emitted_strings_carry_their_trust_and_terminator() {
        let p = emit_str("hi", TAINTED);
        unsafe {
            assert_eq!(string::trust_of(p as *const u8), TAINTED);
            assert_eq!(borrow(p), "hi");
            assert_eq!(*(p as *const u8).add(2), 0, "NUL terminator written");
            string::free_str(p as *mut u8);
        }
    }

    #[test]
    fn an_empty_emit_is_still_a_valid_string() {
        let p = emit(b"", TRUSTED);
        unsafe {
            assert_eq!(borrow(p), "");
            assert_eq!(string::trust_of(p as *const u8), TRUSTED);
            string::free_str(p as *mut u8);
        }
    }
}
