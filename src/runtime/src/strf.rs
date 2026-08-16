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

// ── Indices are characters, never bytes ──────────────────────────────────────
//
// `len()` counts scalars and `s[i]` walks them, so every index below has to as
// well. Rust's own `str::find` answers a *byte* offset, which is the same
// number only for ASCII — so a core written the obvious way agrees with `len`
// on the test string and disagrees on the first accented one. That is the exact
// shape of the bug this whole file was created to remove, one layer down.

/// Byte offset of character index `i`, clamped to the string's ends.
fn byte_at(s: &str, i: i64) -> usize {
    if i <= 0 {
        return 0;
    }
    s.char_indices().nth(i as usize).map(|(b, _)| b).unwrap_or(s.len())
}

/// `s.index_of(sub)` — the *character* index of the first occurrence, or -1.
///
/// -1 rather than nil because the answer is an int either way, so a caller can
/// compare without a type test. Every language that returns a sentinel here
/// picks -1, and Jade has no option type to do better with.
pub fn index_of(s: &str, sub: &str) -> i64 {
    match s.find(sub) {
        Some(byte) => s[..byte].chars().count() as i64,
        None => -1,
    }
}

/// `s.last_index_of(sub)` — the character index of the last occurrence, or -1.
pub fn last_index_of(s: &str, sub: &str) -> i64 {
    match s.rfind(sub) {
        Some(byte) => s[..byte].chars().count() as i64,
        None => -1,
    }
}

/// `s.count(sub)` — non-overlapping occurrences. An empty needle answers 0
/// rather than the string length plus one, which is what `matches("")` gives
/// and which nobody means.
pub fn count(s: &str, sub: &str) -> i64 {
    if sub.is_empty() {
        return 0;
    }
    s.matches(sub).count() as i64
}

/// `s.slice(start, end)` — characters `[start, end)`, clamped rather than
/// raising.
///
/// Clamping follows `bytes.slice`, and for the same reason: a slice that
/// silently gives you less is easier to notice than one that aborts a program,
/// and the alternative is every caller writing the same two bounds checks. A
/// `start` past `end` gives the empty string.
pub fn slice(s: &str, start: i64, end: i64) -> &str {
    let a = byte_at(s, start);
    let b = byte_at(s, end);
    if a >= b { "" } else { &s[a..b] }
}

/// `s.trim_start()` — leading whitespace removed.
pub fn trim_start(s: &str) -> &str {
    s.trim_start()
}

/// `s.trim_end()` — trailing whitespace removed.
pub fn trim_end(s: &str) -> &str {
    s.trim_end()
}

/// `s.repeat(n)` — `n` copies. Zero or negative gives the empty string rather
/// than raising, matching how `slice` treats an out-of-range bound.
pub fn repeat(s: &str, n: i64) -> String {
    if n <= 0 { String::new() } else { s.repeat(n as usize) }
}

/// `s.pad_start(width, pad)` — left-pad to `width` *characters*.
///
/// Already at or past the width, the string comes back untouched — padding
/// never truncates. A multi-character `pad` is repeated and then cut to fit, so
/// the result is always exactly `width`; an empty `pad` would loop forever, so
/// it answers the input unchanged.
pub fn pad_start(s: &str, width: i64, pad: &str) -> String {
    let (_, need) = pad_shape(s, width, pad);
    match need {
        None => s.to_string(),
        Some(n) => {
            let mut out: String = pad.chars().cycle().take(n).collect();
            out.push_str(s);
            out
        }
    }
}

/// `s.pad_end(width, pad)` — right-pad to `width` characters. See
/// [`pad_start`].
pub fn pad_end(s: &str, width: i64, pad: &str) -> String {
    let (_, need) = pad_shape(s, width, pad);
    match need {
        None => s.to_string(),
        Some(n) => {
            let mut out = s.to_string();
            out.extend(pad.chars().cycle().take(n));
            out
        }
    }
}

/// How many pad characters `s` needs to reach `width`, or `None` when it needs
/// none — which covers "already long enough" and "no pad to repeat".
fn pad_shape(s: &str, width: i64, pad: &str) -> (usize, Option<usize>) {
    let have = s.chars().count();
    if pad.is_empty() || width <= 0 || have >= width as usize {
        return (have, None);
    }
    (have, Some(width as usize - have))
}

/// `s.capitalize()` — first character upper, the rest lower.
///
/// Uses the same full Unicode mapping as [`upper`]/[`lower`], so a first
/// character whose uppercase form is longer than itself still works.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.extend(chars.as_str().to_lowercase().chars());
            out
        }
    }
}

/// `s.is_empty()`.
pub fn is_empty(s: &str) -> bool {
    s.is_empty()
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

/// `s.trim_start()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_trim_start(s: *const c_char) -> *mut c_char {
    unsafe { emit(trim_start(borrow(s)), s) }
}

/// `s.trim_end()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_trim_end(s: *const c_char) -> *mut c_char {
    unsafe { emit(trim_end(borrow(s)), s) }
}

/// `s.capitalize()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_capitalize(s: *const c_char) -> *mut c_char {
    unsafe { emit(&capitalize(borrow(s)), s) }
}

/// `s.index_of(sub)` — a character index, or -1.
///
/// # Safety
/// Both arguments must be valid tagged strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_index_of(s: *const c_char, sub: *const c_char) -> i64 {
    unsafe { index_of(borrow(s), borrow(sub)) }
}

/// `s.last_index_of(sub)` — a character index, or -1.
///
/// # Safety
/// Both arguments must be valid tagged strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_last_index_of(s: *const c_char, sub: *const c_char) -> i64 {
    unsafe { last_index_of(borrow(s), borrow(sub)) }
}

/// `s.count(sub)`.
///
/// # Safety
/// Both arguments must be valid tagged strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_count(s: *const c_char, sub: *const c_char) -> i64 {
    unsafe { count(borrow(s), borrow(sub)) }
}

/// `s.is_empty()`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_is_empty(s: *const c_char) -> i32 {
    i32::from(unsafe { is_empty(borrow(s)) })
}

/// `s.slice(start, end)` — character indices, clamped.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_slice(s: *const c_char, start: i64, end: i64) -> *mut c_char {
    unsafe { emit(slice(borrow(s), start, end), s) }
}

/// `s.repeat(n)`.
///
/// # Safety
/// `s` must be a valid tagged string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_repeat(s: *const c_char, n: i64) -> *mut c_char {
    unsafe { emit(&repeat(borrow(s), n), s) }
}

/// `s.pad_start(width, pad)`.
///
/// Trust comes from the receiver, not the padding: padding a tainted string
/// with a literal must not launder it.
///
/// # Safety
/// Both string arguments must be valid tagged strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_pad_start(
    s: *const c_char,
    width: i64,
    pad: *const c_char,
) -> *mut c_char {
    unsafe { emit(&pad_start(borrow(s), width, borrow(pad)), s) }
}

/// `s.pad_end(width, pad)`. See [`jrt_str_pad_start`].
///
/// # Safety
/// Both string arguments must be valid tagged strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_str_pad_end(
    s: *const c_char,
    width: i64,
    pad: *const c_char,
) -> *mut c_char {
    unsafe { emit(&pad_end(borrow(s), width, borrow(pad)), s) }
}

/// `x.slice(start, end)` where `x` may be a string *or* a blob.
///
/// The one primitive method whose name does not settle its receiver's kind, so
/// the tag has to. Compiled code cannot pick the arm from the method name the
/// way it does for `push` or `keys` — and picking wrong would read a tagged
/// string as a `BytesObj` pointer, which is a wild read rather than an error.
/// Dispatching here rather than in emitted IR keeps that decision in one place,
/// beside `jrt_in_any` and `jrt_len_chunk`, which face the same problem.
///
/// # Safety
/// `word` must be a tagged value word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_slice_any(word: i64, start: i64, end: i64) -> i64 {
    let v = crate::value::JadeValue::from_bits(word as u64);
    if v.is_str() {
        let p = v.as_ptr() as *const c_char;
        let out = unsafe { emit(slice(borrow(p), start, end), p) };
        return crate::value::JadeValue::from_str_ptr(out as *const ()).bits() as i64;
    }
    if v.is_ptr() {
        let p = v.as_ptr() as *const core::ffi::c_void;
        let out = unsafe { crate::bytesf::jrt_bytes_slice(p, start, end) };
        return crate::value::JadeValue::from_ptr(out as *const ()).bits() as i64;
    }
    // Neither — the receiver guard in codegen has already refused anything else,
    // so this is unreachable in a compiled program. Answering nil rather than
    // reading a scalar as a pointer is the safe thing to do if it ever is not.
    crate::value::JadeValue::from_bits(crate::value::NIL_BITS).bits() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_matches_the_old_behavior() {
        assert_eq!(upper("hello"), "HELLO");
        assert_eq!(lower("HELLO"), "hello");
    }

    // ── Character indices, not byte offsets ──────────────────────────────
    //
    // The trap one layer down from the case-mapping bug above. `len()` counts
    // scalars and `s[i]` walks them, but Rust's `find` answers a byte offset —
    // the same number only for ASCII. A core written the obvious way agrees
    // with `len` on "hello" and disagrees on "café".

    #[test]
    fn index_of_is_a_character_index_not_a_byte_offset() {
        assert_eq!(index_of("hello", "l"), 2);
        assert_eq!(index_of("hello", "zz"), -1);
        // 'é' is two bytes, so a byte offset would answer 5 here.
        assert_eq!(index_of("café!", "!"), 4);
        assert_eq!(last_index_of("café!é", "é"), 5);
        assert_eq!(last_index_of("hello", "l"), 3);
    }

    #[test]
    fn slice_walks_characters_and_never_splits_one() {
        assert_eq!(slice("hello", 1, 3), "el");
        // A byte-indexed slice would cut 'é' in half and produce invalid UTF-8.
        assert_eq!(slice("café", 0, 4), "café");
        assert_eq!(slice("café", 3, 4), "é");
        // Clamped rather than raising, at both ends and in either order.
        assert_eq!(slice("abc", -5, 99), "abc");
        assert_eq!(slice("abc", 2, 1), "");
    }

    #[test]
    fn pad_counts_characters() {
        assert_eq!(pad_start("7", 3, "0"), "007");
        assert_eq!(pad_end("7", 3, "0"), "700");
        // Four characters, not six bytes.
        assert_eq!(pad_start("café", 6, "."), "..café");
        // Padding never truncates, and a multi-character pad is cut to fit.
        assert_eq!(pad_start("abcd", 2, "0"), "abcd");
        assert_eq!(pad_start("x", 5, "ab"), "ababx");
        // An empty pad would loop forever.
        assert_eq!(pad_start("x", 5, ""), "x");
    }

    #[test]
    fn count_ignores_an_empty_needle() {
        assert_eq!(count("banana", "an"), 2);
        assert_eq!(count("aaaa", "aa"), 2, "non-overlapping");
        assert_eq!(count("abc", "z"), 0);
        // `matches("")` would answer 4 here, which nobody means by "count".
        assert_eq!(count("abc", ""), 0);
    }

    #[test]
    fn capitalize_uses_the_full_case_mapping() {
        assert_eq!(capitalize("hello world"), "Hello world");
        assert_eq!(capitalize("HELLO"), "Hello");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("élan"), "Élan");
    }

    #[test]
    fn repeat_and_trim_edges() {
        assert_eq!(repeat("ab", 3), "ababab");
        assert_eq!(repeat("ab", 0), "");
        assert_eq!(repeat("ab", -1), "");
        assert_eq!(trim_start("  x  "), "x  ");
        assert_eq!(trim_end("  x  "), "  x");
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
