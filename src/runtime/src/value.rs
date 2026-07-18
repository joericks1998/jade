//! The tagged 64-bit value ABI shared by the VM and AOT binaries.
//!
//! This is a **byte-identical** Rust mirror of the "Tagged value ABI" section
//! of `jade-buildd/runtime_lib/runtime.h`. Every Jade runtime value fits in 64
//! bits and carries its kind in the low bits, so a value read back from a
//! type-erased slot (dict value, array element, `Unknown` param/return,
//! exception payload) knows whether it is an int, float, bool, nil, or a heap
//! pointer — matching the VM's dynamically-typed values.
//!
//! Layout (low bits):
//! ```text
//!   bit0 == 0          -> INT    (63-bit signed; value = (i64)v >> 1)
//!   low3 == 0b001 (1)  -> heap POINTER, non-string (dict/array/struct/fn/future)
//!   low3 == 0b011 (3)  -> BOXED FLOAT pointer (untag, then load the f64)
//!   low3 == 0b101 (5)  -> heap STRING pointer
//!   low3 == 0b111 (7)  -> IMMEDIATE: nil or bool, disambiguated by bit3:
//!                           low4 == 0b0111 ( 7) -> NIL
//!                           low4 == 0b1111 (15) -> BOOL, value in bit4
//!                                                  (false=15, true=31)
//! ```
//!
//! All three heap kinds (ptr/float/str) untag with `& !7`. Heap allocations
//! are >= 8-byte aligned, so the low 3 bits of a real pointer are always 0 and
//! free for the tag. INT uses only bit0, so add/sub of two tagged ints is
//! correct without untagging (SMI).
//!
//! This module is pure bit-twiddling — it never allocates. Float boxing (which
//! needs a heap allocation) and the heap object header live in [`crate::heap`].

// ── Tag constants (mirror runtime.h JRT_TAG_*) ─────────────────────────────

/// Mask selecting the low-3-bit tag.
pub const TAG_MASK: u64 = 7;
/// Non-string heap object (dict/array/struct/fn/future).
pub const TAG_PTR: u64 = 1;
/// Boxed float pointer.
pub const TAG_FLOAT: u64 = 3;
/// Heap string pointer.
pub const TAG_STR: u64 = 5;
/// Immediate (nil or bool).
pub const TAG_IMM: u64 = 7;

/// Bit pattern for `nil` (`0b00111`). Mirrors `JRT_NIL`.
pub const NIL_BITS: u64 = 0x07;
/// Bit pattern for `false` (`0b01111`). Mirrors `JRT_FALSE`.
pub const FALSE_BITS: u64 = 0x0F;
/// Bit pattern for `true` (`0b11111`). Mirrors `JRT_TRUE`.
pub const TRUE_BITS: u64 = 0x1F;

// ── JadeValue ──────────────────────────────────────────────────────────────

/// A Jade runtime value: exactly 64 bits, kind carried in the low bits.
///
/// Corresponds to C's `jade_value_t` (`int64_t`). We keep the raw bits in a
/// `u64` and cast to `i64` only where a sign-extending (arithmetic) shift is
/// required (integer unboxing). Across the FFI boundary the value travels as a
/// plain `i64`; [`JadeValue`] is `#[repr(transparent)]` so the two are
/// bit-for-bit interchangeable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct JadeValue(pub u64);

/// The canonical `nil` value.
pub const NIL: JadeValue = JadeValue(NIL_BITS);
/// The canonical `true` value.
pub const TRUE: JadeValue = JadeValue(TRUE_BITS);
/// The canonical `false` value.
pub const FALSE: JadeValue = JadeValue(FALSE_BITS);

impl JadeValue {
    /// Raw 64-bit representation.
    #[inline]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reinterpret raw bits (e.g. an `i64` arriving over the C ABI) as a value.
    #[inline]
    pub const fn from_bits(bits: u64) -> Self {
        JadeValue(bits)
    }

    /// The low-3-bit tag.
    #[inline]
    pub const fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    // ── Kind predicates (mirror jrt_is_*) ──────────────────────────────────

    #[inline]
    pub const fn is_int(self) -> bool {
        self.0 & 1 == 0
    }
    #[inline]
    pub const fn is_ptr(self) -> bool {
        self.tag() == TAG_PTR
    }
    #[inline]
    pub const fn is_float(self) -> bool {
        self.tag() == TAG_FLOAT
    }
    #[inline]
    pub const fn is_str(self) -> bool {
        self.tag() == TAG_STR
    }
    #[inline]
    pub const fn is_nil(self) -> bool {
        self.0 & 0xf == 0x7
    }
    #[inline]
    pub const fn is_bool(self) -> bool {
        self.0 & 0xf == 0xf
    }
    /// Any heap kind (non-string ptr, boxed float, or string): all untag with
    /// `& !7` and may be dereferenced as a pointer.
    #[inline]
    pub const fn is_heap(self) -> bool {
        self.is_ptr() || self.is_float() || self.is_str()
    }

    // ── Immediate box/unbox (mirror jrt_box_*/jrt_unbox_*) ──────────────────

    /// Tag a 63-bit integer. High bits beyond 63 are dropped (SMI semantics),
    /// matching the C macro `jrt_box_int`.
    #[inline]
    pub const fn from_int(i: i64) -> Self {
        JadeValue((i as u64) << 1)
    }
    /// Untag an integer with an arithmetic (sign-extending) shift.
    #[inline]
    pub const fn as_int(self) -> i64 {
        (self.0 as i64) >> 1
    }

    /// Tag a bool into the immediate representation (`false=15`, `true=31`).
    #[inline]
    pub const fn from_bool(b: bool) -> Self {
        JadeValue(((b as u64) << 4) | 0xf)
    }
    /// Untag a bool (reads bit4).
    #[inline]
    pub const fn as_bool(self) -> bool {
        (self.0 >> 4) & 1 != 0
    }

    // ── Heap pointer box/unbox (mirror jrt_box_ptr/str, jrt_unbox_ptr) ──────

    /// Tag a non-string heap pointer. The pointer must be 8-byte aligned.
    #[inline]
    pub fn from_ptr(p: *const ()) -> Self {
        JadeValue(p as usize as u64 | TAG_PTR)
    }
    /// Tag a heap string pointer. The pointer must be 8-byte aligned.
    #[inline]
    pub fn from_str_ptr(p: *const ()) -> Self {
        JadeValue(p as usize as u64 | TAG_STR)
    }
    /// Untag any heap value (`& !7`) to its raw pointer. Valid for
    /// ptr/float/str kinds; nonsensical for ints/immediates.
    #[inline]
    pub fn as_ptr(self) -> *mut () {
        (self.0 & !7) as usize as *mut ()
    }
}

impl core::fmt::Debug for JadeValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_int() {
            write!(f, "JadeValue::Int({})", self.as_int())
        } else if self.is_nil() {
            write!(f, "JadeValue::Nil")
        } else if self.is_bool() {
            write!(f, "JadeValue::Bool({})", self.as_bool())
        } else if self.is_float() {
            write!(f, "JadeValue::Float(@{:p})", self.as_ptr())
        } else if self.is_str() {
            write!(f, "JadeValue::Str(@{:p})", self.as_ptr())
        } else {
            write!(f, "JadeValue::Ptr(@{:p})", self.as_ptr())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_round_trips_including_negatives_and_edges() {
        for i in [0i64, 1, -1, 42, -42, i64::MAX >> 1, i64::MIN >> 1] {
            let v = JadeValue::from_int(i);
            assert!(v.is_int(), "{i} should tag as int");
            assert!(!v.is_heap());
            assert_eq!(v.as_int(), i, "round-trip failed for {i}");
        }
    }

    #[test]
    fn int_low_bit_is_zero() {
        // SMI invariant: two tagged ints add without untagging.
        let a = JadeValue::from_int(3);
        let b = JadeValue::from_int(4);
        let sum = JadeValue(a.bits().wrapping_add(b.bits()));
        assert_eq!(sum.as_int(), 7);
    }

    #[test]
    fn bool_round_trips() {
        assert!(TRUE.is_bool());
        assert!(FALSE.is_bool());
        assert!(TRUE.as_bool());
        assert!(!FALSE.as_bool());
        assert_eq!(JadeValue::from_bool(true), TRUE);
        assert_eq!(JadeValue::from_bool(false), FALSE);
        // Bools are not ints, not nil.
        assert!(!TRUE.is_int());
        assert!(!TRUE.is_nil());
    }

    #[test]
    fn nil_is_distinct() {
        assert!(NIL.is_nil());
        assert!(!NIL.is_bool());
        assert!(!NIL.is_int());
        assert!(!NIL.is_heap());
    }

    #[test]
    fn constants_match_runtime_h() {
        // These literals are the contract with runtime.h — do not "fix" them
        // without changing the C side in lockstep.
        assert_eq!(NIL.bits(), 0x07);
        assert_eq!(FALSE.bits(), 0x0F);
        assert_eq!(TRUE.bits(), 0x1F);
        assert_eq!(TAG_PTR, 1);
        assert_eq!(TAG_FLOAT, 3);
        assert_eq!(TAG_STR, 5);
        assert_eq!(TAG_IMM, 7);
    }

    #[test]
    fn ptr_tags_untag_cleanly() {
        // A fabricated 8-aligned address; never dereferenced.
        let addr = 0x1000usize as *const ();
        let p = JadeValue::from_ptr(addr);
        assert!(p.is_ptr());
        assert!(p.is_heap());
        assert!(!p.is_str());
        assert_eq!(p.as_ptr() as usize, 0x1000);

        let s = JadeValue::from_str_ptr(addr);
        assert!(s.is_str());
        assert!(s.is_heap());
        assert!(!s.is_ptr());
        assert_eq!(s.as_ptr() as usize, 0x1000);
    }
}
