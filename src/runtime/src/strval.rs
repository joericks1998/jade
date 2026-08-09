//! String **value** helpers for the dynamic ops: bounded compare and
//! truthiness. These read tagged-string data pointers directly (the 8-byte-
//! header layout from `runtime.h`). The allocator and concatenation live in
//! [`crate::string`].

/// Cap on how far the bounded compares scan. JRT_TAG_PTR is shared by strings
/// and dicts/arrays (no heap *kind* tag), so a non-string heap pointer can
/// reach these helpers when a runtime `Unknown == Unknown` turns out to be two
/// dicts. A raw scan of a non-NUL-terminated object could run off the
/// allocation; this cap prevents that. Mirrors `JRT_STR_CMP_CAP` in common.c.
const CAP: usize = 1 << 20; // 1 MiB — beyond any real Jade string.

/// `strnlen(p, CAP)` — length of the NUL-terminated string at `p`, bounded.
///
/// # Safety
/// `p` must be non-null and point at readable bytes up to the first NUL or
/// `CAP`, whichever comes first (the tagged-string invariant).
#[inline]
unsafe fn strnlen(p: *const u8, cap: usize) -> usize {
    let mut i = 0;
    while i < cap && unsafe { *p.add(i) } != 0 {
        i += 1;
    }
    i
}

/// Bounded equality of two tagged strings. Mirrors C `jrt_streq_bounded`:
/// a value with no NUL within the cap is not a well-formed string → not equal.
///
/// # Safety
/// `a`/`b` must be tagged-string data pointers (or reachable heap pointers,
/// which are handled conservatively by the cap).
pub unsafe fn eq_bounded(a: *const u8, b: *const u8) -> bool {
    unsafe {
        let la = strnlen(a, CAP);
        let lb = strnlen(b, CAP);
        if la >= CAP || lb >= CAP {
            return false;
        }
        la == lb && core::slice::from_raw_parts(a, la) == core::slice::from_raw_parts(b, lb)
    }
}

/// Bounded ordering of two tagged strings (-1/0/1). Mirrors C
/// `jrt_strcmp_bounded`: unequal-length prefix ties break by length; an
/// over-cap value compares as equal-ish (0), matching the C fallback.
///
/// # Safety
/// See [`eq_bounded`].
pub unsafe fn cmp_bounded(a: *const u8, b: *const u8) -> i32 {
    unsafe {
        let la = strnlen(a, CAP);
        let lb = strnlen(b, CAP);
        if la >= CAP || lb >= CAP {
            return 0;
        }
        let n = la.min(lb);
        // Unsigned byte lexicographic compare == memcmp.
        match core::slice::from_raw_parts(a, n).cmp(core::slice::from_raw_parts(b, n)) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Greater => 1,
            core::cmp::Ordering::Equal => {
                if la < lb {
                    -1
                } else if la > lb {
                    1
                } else {
                    0
                }
            }
        }
    }
}

/// Truthiness of a string, matching the VM's `bool()` and C `jrt_bool_of_str`:
/// empty/NULL → false; the (case-insensitive) text `"false"` → false; every
/// other non-empty string → true.
///
/// # Safety
/// `s` must be null or a NUL-terminated readable string.
pub unsafe fn bool_of(s: *const u8) -> bool {
    unsafe {
        if s.is_null() || *s == 0 {
            return false;
        }
        let f: &[u8] = b"false";
        let mut i = 0usize;
        while i < f.len() && *s.add(i) != 0 {
            if (*s.add(i)).to_ascii_lowercase() != f[i] {
                return true;
            }
            i += 1;
        }
        // Matched a prefix of "false"; it's `false` only if BOTH ended together.
        let f_end = i >= f.len();
        let s_end = *s.add(i) == 0;
        !(f_end && s_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a fake tagged string: just NUL-terminated bytes (the compare/bool
    // helpers only read forward from the data pointer; they don't touch the
    // trust header).
    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    #[test]
    fn eq_and_cmp() {
        let a = cstr("apple");
        let b = cstr("apple");
        let c = cstr("banana");
        let ab = cstr("ap");
        unsafe {
            assert!(eq_bounded(a.as_ptr(), b.as_ptr()));
            assert!(!eq_bounded(a.as_ptr(), c.as_ptr()));
            assert_eq!(cmp_bounded(a.as_ptr(), b.as_ptr()), 0);
            assert_eq!(cmp_bounded(a.as_ptr(), c.as_ptr()), -1); // 'a' < 'b'
            assert_eq!(cmp_bounded(c.as_ptr(), a.as_ptr()), 1);
            assert_eq!(cmp_bounded(a.as_ptr(), ab.as_ptr()), 1); // "apple" > "ap"
            assert_eq!(cmp_bounded(ab.as_ptr(), a.as_ptr()), -1);
        }
    }

    #[test]
    fn bool_of_str_matches_vm() {
        let cases = [
            ("", false),
            ("false", false),
            ("False", false),
            ("FALSE", false),
            ("true", true),
            ("t", true),
            ("0", true), // only literal "false" is falsey
            ("false ", true),
            (" false", true),
            ("falsehood", true),
        ];
        for (s, want) in cases {
            let c = cstr(s);
            assert_eq!(unsafe { bool_of(c.as_ptr()) }, want, "bool_of({s:?})");
        }
    }

    #[test]
    fn empty_strings_compare_equal() {
        let a = cstr("");
        let b = cstr("");
        let x = cstr("x");
        unsafe {
            assert!(eq_bounded(a.as_ptr(), b.as_ptr()));
            assert_eq!(cmp_bounded(a.as_ptr(), b.as_ptr()), 0);
            assert_eq!(cmp_bounded(a.as_ptr(), x.as_ptr()), -1);
        }
    }
}
