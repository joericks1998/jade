//! Boxed floats.
//!
//! A float cannot fit inline alongside the low-bit tag, so it is heap-boxed: an
//! 8-byte (>= 8-aligned) allocation holding the `f64`, tagged `TAG_FLOAT`. This
//! is the accepted cost of keeping every [`JadeValue`] 64 bits wide.
//!
//! Byte-identical to `runtime.h`'s `jrt_box_float` / `jrt_unbox_float`, and
//! allocated through the *system* allocator ([`crate::sys`]) so a float boxed
//! here and a float boxed by the C runtime are the same shape and freeable by
//! either side.

use crate::sys::{malloc, oom};
use crate::value::{JadeValue, TAG_FLOAT};

/// Heap-box a double. Aborts on allocation failure. Mirrors C `jrt_box_float`.
#[inline]
pub fn box_float(d: f64) -> JadeValue {
    unsafe {
        let p = malloc(8) as *mut f64;
        if p.is_null() {
            oom();
        }
        p.write(d);
        JadeValue::from_bits(p as usize as u64 | TAG_FLOAT)
    }
}

/// Load a boxed double. Mirrors C `jrt_unbox_float`.
///
/// # Safety-adjacent
/// Only valid for a value whose tag is `TAG_FLOAT`. Callers are the tag-erased
/// dispatch paths, which check the tag first. Kept as a safe fn to match the C
/// ABI shape; a wrong tag reads whatever the pointer bits address (same
/// contract as the C macro).
#[inline]
pub fn unbox_float(v: JadeValue) -> f64 {
    unsafe { *(v.as_ptr() as *const f64) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_round_trips() {
        for d in [0.0f64, 1.5, -3.25, 1e300, -1e-300, f64::MAX, f64::MIN] {
            let v = box_float(d);
            assert!(v.is_float(), "{d} should tag as float");
            assert!(!v.is_int());
            assert_eq!(unbox_float(v), d, "round-trip failed for {d}");
        }
    }

    #[test]
    fn nan_round_trips_bitwise() {
        let v = box_float(f64::NAN);
        assert!(v.is_float());
        assert!(unbox_float(v).is_nan());
    }

    #[test]
    fn boxed_pointer_is_8_aligned() {
        // The tag scheme requires the low 3 bits free.
        let v = box_float(2.0);
        assert_eq!(v.as_ptr() as usize & 7, 0);
    }
}
