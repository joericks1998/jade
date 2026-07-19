//! Numeric helpers shared by both engines.

/// Integer exponentiation by squaring. Mirrors C `jrt_ipow`.
///
/// A negative exponent yields `0`: the AOT `Int` result path cannot produce a
/// float, so the dynamic `**` operator special-cases negative exponents to a
/// float *before* reaching here (see the `pow_any` path). Overflow wraps,
/// matching C's signed multiplication (and avoiding a debug-build panic).
pub fn ipow(base: i64, exp: i64) -> i64 {
    if exp < 0 {
        return 0;
    }
    let mut result: i64 = 1;
    let mut b = base;
    let mut e = exp as u64;
    while e != 0 {
        if e & 1 != 0 {
            result = result.wrapping_mul(b);
        }
        e >>= 1;
        if e != 0 {
            b = b.wrapping_mul(b);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_cases() {
        assert_eq!(ipow(2, 10), 1024);
        assert_eq!(ipow(3, 0), 1);
        assert_eq!(ipow(5, 1), 5);
        assert_eq!(ipow(10, 3), 1000);
        assert_eq!(ipow(-2, 3), -8);
        assert_eq!(ipow(-2, 4), 16);
    }

    #[test]
    fn negative_exponent_is_zero() {
        assert_eq!(ipow(2, -1), 0);
        assert_eq!(ipow(7, -5), 0);
    }

    #[test]
    fn zero_base() {
        assert_eq!(ipow(0, 5), 0);
        assert_eq!(ipow(0, 0), 1);
    }
}
