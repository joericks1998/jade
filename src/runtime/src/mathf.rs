//! The `math.*` stdlib operations on tagged value words, for the AOT backend.
//!
//! These mirror the VM's `crate::math` (jadelang `src/math/mod.rs`) exactly on
//! the runtime int/float kind, so `math.floor`/`pow`/… produce byte-identical
//! results in both engines. Each takes and returns a tagged [`JadeValue`] word
//! (`i64` over the C ABI): the kind is read at runtime, the op computed, and the
//! result re-tagged as an int (`Int`) or a boxed float (`Float`) to match the VM.
//!
//! Non-numeric operands can't occur (the frontend type-checks `math.*` args), so
//! these never raise — a stray non-number falls through to the float path rather
//! than crossing a Rust frame with a `longjmp`.

use crate::float::{box_float, unbox_float};
use crate::value::JadeValue;

#[inline]
fn v(w: i64) -> JadeValue {
    JadeValue::from_bits(w as u64)
}
#[inline]
fn is_int(w: i64) -> bool {
    v(w).is_int()
}
#[inline]
fn as_int(w: i64) -> i64 {
    v(w).as_int()
}
/// Coerce a numeric word to `f64` (int → widened, float → unboxed).
#[inline]
fn as_f64(w: i64) -> f64 {
    let x = v(w);
    if x.is_int() {
        x.as_int() as f64
    } else {
        unbox_float(x)
    }
}
#[inline]
fn int_word(i: i64) -> i64 {
    JadeValue::from_int(i).bits() as i64
}
#[inline]
fn float_word(f: f64) -> i64 {
    box_float(f).bits() as i64
}

/// `math.floor(x)`: float → floored int; int → unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_floor(w: i64) -> i64 {
    if is_int(w) { w } else { int_word(as_f64(w).floor() as i64) }
}

/// `math.ceil(x)`: float → ceiled int; int → unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_ceil(w: i64) -> i64 {
    if is_int(w) { w } else { int_word(as_f64(w).ceil() as i64) }
}

/// `math.abs(x)`: preserves the operand's int/float kind.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_abs(w: i64) -> i64 {
    if is_int(w) { int_word(as_int(w).abs()) } else { float_word(as_f64(w).abs()) }
}

/// `math.sqrt(x)`: always a float.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_sqrt(w: i64) -> i64 {
    float_word(as_f64(w).sqrt())
}

/// `math.min(a, b)`: int if both int, else float (VM semantics).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_min(a: i64, b: i64) -> i64 {
    if is_int(a) && is_int(b) {
        int_word(as_int(a).min(as_int(b)))
    } else {
        float_word(as_f64(a).min(as_f64(b)))
    }
}

/// `math.max(a, b)`: int if both int, else float (VM semantics).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_max(a: i64, b: i64) -> i64 {
    if is_int(a) && is_int(b) {
        int_word(as_int(a).max(as_int(b)))
    } else {
        float_word(as_f64(a).max(as_f64(b)))
    }
}

/// `math.pow(base, exp)`: `int**non-neg-int` is int (wrapping at the 63-bit SMI
/// range — the documented AOT residual); every other combination is a float,
/// with `**int` using `powi` and `**float` using `powf` (matching the VM).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_pow(a: i64, b: i64) -> i64 {
    if is_int(a) && is_int(b) {
        let (base, exp) = (as_int(a), as_int(b));
        if exp >= 0 {
            int_word(base.wrapping_pow(exp as u32))
        } else {
            float_word((base as f64).powi(exp as i32))
        }
    } else if is_int(b) {
        // (Float, Int) → powi
        float_word(as_f64(a).powi(as_int(b) as i32))
    } else {
        // (Float, Float) or (Int, Float) → powf
        float_word(as_f64(a).powf(as_f64(b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_ceil_abs_match_vm() {
        assert_eq!(jrt_math_floor(float_word(3.7)), int_word(3));
        assert_eq!(jrt_math_floor(int_word(5)), int_word(5));
        assert_eq!(jrt_math_ceil(float_word(3.2)), int_word(4));
        assert_eq!(jrt_math_abs(int_word(-4)), int_word(4));
        assert_eq!(as_f64(jrt_math_abs(float_word(-2.5))), 2.5);
    }

    #[test]
    fn pow_int_and_float() {
        assert_eq!(jrt_math_pow(int_word(2), int_word(10)), int_word(1024));
        // negative exponent → float
        assert_eq!(as_f64(jrt_math_pow(int_word(2), int_word(-1))), 0.5);
        assert_eq!(as_f64(jrt_math_pow(float_word(2.0), int_word(3))), 8.0);
    }

    #[test]
    fn min_max_kind_preservation() {
        assert_eq!(jrt_math_min(int_word(3), int_word(7)), int_word(3));
        assert_eq!(as_f64(jrt_math_max(int_word(3), float_word(7.5))), 7.5);
        assert!(v(jrt_math_min(int_word(1), float_word(2.0))).is_float());
    }
}
