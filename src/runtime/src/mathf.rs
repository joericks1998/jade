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
fn int_word(i: i64) -> i64 {
    JadeValue::from_int(i).bits() as i64
}
#[inline]
fn float_word(f: f64) -> i64 {
    box_float(f).bits() as i64
}

// ── Neutral cores (one implementation, both engines) ─────────────────────────
//
// `math.*` used to be written twice: once here over tagged words for AOT, and
// once in the VM over `VmValue` (jadelang `src/math/mod.rs`), with a comment
// promising the two "mirror" each other. They did not. `math.pow(2, 64)`
// panicked with a raw Rust overflow message under `jade run` and silently
// printed 0 under `jade build`.
//
// Neither matched Jade's own rule: `a + 1` past i64::MAX raises
// "integer overflow" (the VM uses `checked_add`/`checked_mul`). So the shared
// core is overflow-checked, and both engines now raise the same error the rest
// of the language's arithmetic already raises.

/// A Jade number, kind-preserving. `math.*` returns an int where both operands
/// were ints and a float otherwise, which is the VM's long-standing behavior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i64),
    Float(f64),
}

/// The only way a `math.*` core can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathErr {
    /// The exact result does not fit in an i64. Reported as Jade's
    /// `IntegerOverflow`, the same error `+`/`-`/`*` raise.
    Overflow,
}

impl Num {
    #[inline]
    pub fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

/// `math.floor(x)`: float → floored int; int → unchanged.
pub fn floor(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i),
        Num::Float(f) => Num::Int(f.floor() as i64),
    }
}

/// `math.ceil(x)`: float → ceiled int; int → unchanged.
pub fn ceil(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i),
        Num::Float(f) => Num::Int(f.ceil() as i64),
    }
}

/// `math.abs(x)`: preserves the operand's int/float kind.
///
/// Checked: `i64::MIN.abs()` has no i64 representation. Both engines used to
/// call `i64::abs` directly, which panics in a debug build and wraps to
/// `i64::MIN` in a release one.
pub fn abs(x: Num) -> Result<Num, MathErr> {
    match x {
        Num::Int(i) => i.checked_abs().map(Num::Int).ok_or(MathErr::Overflow),
        Num::Float(f) => Ok(Num::Float(f.abs())),
    }
}

/// `math.sqrt(x)`: always a float.
pub fn sqrt(x: Num) -> Num {
    Num::Float(x.as_f64().sqrt())
}

/// `math.min(a, b)`: int if both int, else float.
pub fn min(a: Num, b: Num) -> Num {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Num::Int(x.min(y)),
        _ => Num::Float(a.as_f64().min(b.as_f64())),
    }
}

/// `math.max(a, b)`: int if both int, else float.
pub fn max(a: Num, b: Num) -> Num {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Num::Int(x.max(y)),
        _ => Num::Float(a.as_f64().max(b.as_f64())),
    }
}

/// `math.pow(base, exp)`: `int ** non-negative int` stays an int and is
/// overflow-checked; a negative exponent yields a float (`powi`), and any float
/// operand yields a float (`powi` for an int exponent, `powf` otherwise).
pub fn pow(a: Num, b: Num) -> Result<Num, MathErr> {
    match (a, b) {
        (Num::Int(base), Num::Int(exp)) if exp >= 0 => {
            // `exp` can exceed u32; anything that large overflows regardless.
            let e = u32::try_from(exp).map_err(|_| MathErr::Overflow)?;
            base.checked_pow(e).map(Num::Int).ok_or(MathErr::Overflow)
        }
        (_, Num::Int(exp)) => Ok(Num::Float(a.as_f64().powi(exp as i32))),
        _ => Ok(Num::Float(a.as_f64().powf(b.as_f64()))),
    }
}

/// `math.round(x)`: float → nearest int, ties away from zero; int → unchanged.
///
/// Ties away from zero rather than Rust's default-for-`f64` (which is the same
/// thing) stated explicitly, because the other obvious choice — banker's
/// rounding — differs at exactly the values people test with, and a reader
/// should not have to run it to find out. `round(2.5)` is 3.
pub fn round(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i),
        Num::Float(f) => Num::Int(f.round() as i64),
    }
}

/// `math.trunc(x)`: float → toward zero; int → unchanged. The difference from
/// [`floor`] is only visible for negatives: `floor(-1.5)` is -2, `trunc` is -1.
pub fn trunc(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i),
        Num::Float(f) => Num::Int(f.trunc() as i64),
    }
}

/// `math.sign(x)`: -1, 0 or 1, preserving the operand's kind.
///
/// A float answers with a float so the kind rule is the same one every other
/// `math.*` follows. `sign(NaN)` is NaN, which is what `f64::signum` does not
/// do — it answers 1.0 — so this special-cases it rather than inventing an
/// order for a value that has none.
pub fn sign(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i.signum()),
        Num::Float(f) if f.is_nan() => Num::Float(f),
        Num::Float(f) if f == 0.0 => Num::Float(0.0),
        Num::Float(f) => Num::Float(f.signum()),
    }
}

/// `math.clamp(x, lo, hi)`: int only when all three are ints, matching
/// [`min`]/[`max`].
///
/// A `lo` above `hi` is not rejected: the result is `x.max(lo).min(hi)`, so the
/// `min` wins and the answer is `hi`. Deliberately not a panic — the standard
/// library's `clamp` panics on a reversed range, and a Jade `math.*` call has no
/// business aborting the process over an argument order.
pub fn clamp(x: Num, lo: Num, hi: Num) -> Num {
    match (x, lo, hi) {
        (Num::Int(a), Num::Int(l), Num::Int(h)) => Num::Int(a.max(l).min(h)),
        _ => Num::Float(x.as_f64().max(lo.as_f64()).min(hi.as_f64())),
    }
}

/// `math.ln(x)` — natural log. Always a float, following [`sqrt`].
pub fn ln(x: Num) -> Num {
    Num::Float(x.as_f64().ln())
}

/// `math.log2(x)`. Always a float.
pub fn log2(x: Num) -> Num {
    Num::Float(x.as_f64().log2())
}

/// `math.log10(x)`. Always a float.
pub fn log10(x: Num) -> Num {
    Num::Float(x.as_f64().log10())
}

/// `math.exp(x)` — e to the x. Always a float.
pub fn exp(x: Num) -> Num {
    Num::Float(x.as_f64().exp())
}

/// `math.sin(x)`, in radians. Always a float.
pub fn sin(x: Num) -> Num {
    Num::Float(x.as_f64().sin())
}

/// `math.cos(x)`, in radians. Always a float.
pub fn cos(x: Num) -> Num {
    Num::Float(x.as_f64().cos())
}

/// `math.tan(x)`, in radians. Always a float.
pub fn tan(x: Num) -> Num {
    Num::Float(x.as_f64().tan())
}

/// `math.asin(x)`. Always a float; outside [-1, 1] the answer is NaN, which is
/// what IEEE says and what both engines will print identically.
pub fn asin(x: Num) -> Num {
    Num::Float(x.as_f64().asin())
}

/// `math.acos(x)`. Always a float; NaN outside [-1, 1].
pub fn acos(x: Num) -> Num {
    Num::Float(x.as_f64().acos())
}

/// `math.atan(x)`. Always a float.
pub fn atan(x: Num) -> Num {
    Num::Float(x.as_f64().atan())
}

/// `math.atan2(y, x)` — the angle of the point, quadrant-correct. Always a
/// float. Argument order is `(y, x)`, matching C and every other language that
/// has it; the reverse is a classic silent bug, so the parameter names say so.
pub fn atan2(y: Num, x: Num) -> Num {
    Num::Float(y.as_f64().atan2(x.as_f64()))
}

/// `math.hypot(a, b)` — `sqrt(a² + b²)` without the intermediate overflow.
/// Always a float.
pub fn hypot(a: Num, b: Num) -> Num {
    Num::Float(a.as_f64().hypot(b.as_f64()))
}

/// `math.is_nan(x)`. An int is never NaN.
pub fn is_nan(x: Num) -> bool {
    matches!(x, Num::Float(f) if f.is_nan())
}

/// `math.is_inf(x)`. An int is never infinite.
pub fn is_inf(x: Num) -> bool {
    matches!(x, Num::Float(f) if f.is_infinite())
}

/// `math.pi()` / `math.e()` / `math.tau()` / `math.inf()` / `math.nan()`.
///
/// Spelled as calls rather than as values because a package namespace is only
/// ever reached as a field access that a call immediately consumes — there is
/// no lowering for reading one in value position, so `math.pi` has nowhere to
/// go. See the changelog entry for v1.3.23.
pub fn pi() -> Num {
    Num::Float(core::f64::consts::PI)
}

/// See [`pi`].
pub fn e() -> Num {
    Num::Float(core::f64::consts::E)
}

/// See [`pi`]. Two pi, for the turn-based trigonometry that wants it.
pub fn tau() -> Num {
    Num::Float(core::f64::consts::TAU)
}

/// See [`pi`]. Positive infinity — reachable no other way, since the lexer
/// caps a numeric literal.
pub fn inf() -> Num {
    Num::Float(f64::INFINITY)
}

/// See [`pi`]. Not-a-number — likewise unwritable as a literal.
pub fn nan() -> Num {
    Num::Float(f64::NAN)
}

// ── AOT word wrappers ────────────────────────────────────────────────────────

#[inline]
fn to_num(w: i64) -> Num {
    let x = v(w);
    if x.is_int() { Num::Int(x.as_int()) } else { Num::Float(unbox_float(x)) }
}

#[inline]
fn from_num(n: Num) -> i64 {
    match n {
        Num::Int(i) => int_word(i),
        Num::Float(f) => float_word(f),
    }
}

/// Error code written to the `err` out-param by the checked wrappers; the C
/// forwarder raises Jade's `integer overflow` when it is non-zero.
pub const JRT_MATH_OK: u32 = 0;
pub const JRT_MATH_OVERFLOW: u32 = 1;

/// A tagged bool word, for the predicate wrappers.
#[inline]
fn bool_word(b: bool) -> i64 {
    JadeValue::from_bool(b).bits() as i64
}

/// `math.floor(x)`: float → floored int; int → unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_floor(w: i64) -> i64 {
    from_num(floor(to_num(w)))
}

/// `math.ceil(x)`: float → ceiled int; int → unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_ceil(w: i64) -> i64 {
    from_num(ceil(to_num(w)))
}

/// `math.abs(x)`. Overflow-checked: see [`abs`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_abs(w: i64, err: *mut u32) -> i64 {
    match abs(to_num(w)) {
        Ok(n) => {
            unsafe { *err = JRT_MATH_OK };
            from_num(n)
        }
        Err(_) => {
            unsafe { *err = JRT_MATH_OVERFLOW };
            0
        }
    }
}

/// `math.sqrt(x)`: always a float.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_sqrt(w: i64) -> i64 {
    from_num(sqrt(to_num(w)))
}

/// `math.min(a, b)`: int if both int, else float.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_min(a: i64, b: i64) -> i64 {
    from_num(min(to_num(a), to_num(b)))
}

/// `math.max(a, b)`: int if both int, else float.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_max(a: i64, b: i64) -> i64 {
    from_num(max(to_num(a), to_num(b)))
}

/// `math.pow(base, exp)`. Overflow-checked: see [`pow`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_pow(a: i64, b: i64, err: *mut u32) -> i64 {
    match pow(to_num(a), to_num(b)) {
        Ok(n) => {
            unsafe { *err = JRT_MATH_OK };
            from_num(n)
        }
        Err(_) => {
            unsafe { *err = JRT_MATH_OVERFLOW };
            0
        }
    }
}

/// `math.round(x)`. See [`round`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_round(w: i64) -> i64 {
    from_num(round(to_num(w)))
}

/// `math.trunc(x)`. See [`trunc`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_trunc(w: i64) -> i64 {
    from_num(trunc(to_num(w)))
}

/// `math.sign(x)`. See [`sign`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_sign(w: i64) -> i64 {
    from_num(sign(to_num(w)))
}

/// `math.ln(x)`. See [`ln`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_ln(w: i64) -> i64 {
    from_num(ln(to_num(w)))
}

/// `math.log2(x)`. See [`log2`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_log2(w: i64) -> i64 {
    from_num(log2(to_num(w)))
}

/// `math.log10(x)`. See [`log10`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_log10(w: i64) -> i64 {
    from_num(log10(to_num(w)))
}

/// `math.exp(x)`. See [`exp`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_exp(w: i64) -> i64 {
    from_num(exp(to_num(w)))
}

/// `math.sin(x)`. See [`sin`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_sin(w: i64) -> i64 {
    from_num(sin(to_num(w)))
}

/// `math.cos(x)`. See [`cos`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_cos(w: i64) -> i64 {
    from_num(cos(to_num(w)))
}

/// `math.tan(x)`. See [`tan`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_tan(w: i64) -> i64 {
    from_num(tan(to_num(w)))
}

/// `math.asin(x)`. See [`asin`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_asin(w: i64) -> i64 {
    from_num(asin(to_num(w)))
}

/// `math.acos(x)`. See [`acos`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_acos(w: i64) -> i64 {
    from_num(acos(to_num(w)))
}

/// `math.atan(x)`. See [`atan`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_atan(w: i64) -> i64 {
    from_num(atan(to_num(w)))
}

/// `math.atan2(a, b)`. See [`atan2`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_atan2(a: i64, b: i64) -> i64 {
    from_num(atan2(to_num(a), to_num(b)))
}

/// `math.hypot(a, b)`. See [`hypot`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_hypot(a: i64, b: i64) -> i64 {
    from_num(hypot(to_num(a), to_num(b)))
}

/// `math.clamp(x, lo, hi)`. See [`clamp`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_clamp(x: i64, lo: i64, hi: i64) -> i64 {
    from_num(clamp(to_num(x), to_num(lo), to_num(hi)))
}

/// `math.is_nan(x)`. Answers a tagged bool word. See [`is_nan`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_is_nan(w: i64) -> i64 {
    bool_word(is_nan(to_num(w)))
}

/// `math.is_inf(x)`. Answers a tagged bool word. See [`is_inf`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_is_inf(w: i64) -> i64 {
    bool_word(is_inf(to_num(w)))
}

/// `math.pi()`. See [`pi`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_pi() -> i64 {
    from_num(pi())
}

/// `math.e()`. See [`e`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_e() -> i64 {
    from_num(e())
}

/// `math.tau()`. See [`tau`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_tau() -> i64 {
    from_num(tau())
}

/// `math.inf()`. See [`inf`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_inf() -> i64 {
    from_num(inf())
}

/// `math.nan()`. See [`nan`].
#[unsafe(no_mangle)]
pub extern "C" fn jrt_math_nan() -> i64 {
    from_num(nan())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cores are the single implementation both engines use, so they are what
    // these test. The word wrappers are exercised through `ok()` below.

    /// Call a checked word wrapper, asserting it did not report overflow.
    fn ok(f: impl FnOnce(*mut u32) -> i64) -> i64 {
        let mut err = 0u32;
        let r = f(&mut err);
        assert_eq!(err, JRT_MATH_OK, "unexpected overflow");
        r
    }

    /// Call a checked word wrapper, asserting it *did* report overflow.
    fn overflows(f: impl FnOnce(*mut u32) -> i64) {
        let mut err = 0u32;
        f(&mut err);
        assert_eq!(err, JRT_MATH_OVERFLOW, "expected overflow");
    }

    #[test]
    fn floor_ceil_abs_preserve_kind() {
        assert_eq!(floor(Num::Float(3.7)), Num::Int(3));
        assert_eq!(floor(Num::Int(5)), Num::Int(5));
        assert_eq!(ceil(Num::Float(3.2)), Num::Int(4));
        assert_eq!(abs(Num::Int(-4)), Ok(Num::Int(4)));
        assert_eq!(abs(Num::Float(-2.5)), Ok(Num::Float(2.5)));
    }

    #[test]
    fn pow_int_and_float() {
        assert_eq!(pow(Num::Int(2), Num::Int(10)), Ok(Num::Int(1024)));
        // A negative exponent leaves the integers.
        assert_eq!(pow(Num::Int(2), Num::Int(-1)), Ok(Num::Float(0.5)));
        assert_eq!(pow(Num::Float(2.0), Num::Int(3)), Ok(Num::Float(8.0)));
        assert_eq!(pow(Num::Float(4.0), Num::Float(0.5)), Ok(Num::Float(2.0)));
    }

    /// The bug this unification existed to fix. `math.pow(2, 64)` used to panic
    /// with a raw Rust overflow message under `jade run` and silently produce 0
    /// under `jade build`; neither matched `a + 1`, which raises "integer
    /// overflow". Both engines now get this one answer.
    #[test]
    fn int_pow_overflow_is_an_error_not_a_wrap_or_a_panic() {
        assert_eq!(pow(Num::Int(2), Num::Int(64)), Err(MathErr::Overflow));
        // An exponent too large even to fit a u32 is overflow, not a panic.
        assert_eq!(pow(Num::Int(2), Num::Int(i64::MAX)), Err(MathErr::Overflow));
        // A float base is unaffected — infinity is a value, not an error.
        assert!(matches!(pow(Num::Float(2.0), Num::Int(4096)), Ok(Num::Float(_))));
    }

    #[test]
    fn abs_of_i64_min_is_an_error() {
        // No i64 represents it; `i64::abs` panics in debug and wraps in release.
        assert_eq!(abs(Num::Int(i64::MIN)), Err(MathErr::Overflow));
    }

    #[test]
    fn min_max_kind_preservation() {
        assert_eq!(min(Num::Int(3), Num::Int(7)), Num::Int(3));
        assert_eq!(max(Num::Int(3), Num::Float(7.5)), Num::Float(7.5));
        assert!(matches!(min(Num::Int(1), Num::Float(2.0)), Num::Float(_)));
    }

    #[test]
    fn word_wrappers_round_trip_through_the_cores() {
        assert_eq!(jrt_math_floor(float_word(3.7)), int_word(3));
        assert_eq!(jrt_math_ceil(float_word(3.2)), int_word(4));
        assert_eq!(jrt_math_min(int_word(3), int_word(7)), int_word(3));
        assert_eq!(ok(|e| jrt_math_abs(int_word(-4), e)), int_word(4));
        assert_eq!(ok(|e| jrt_math_pow(int_word(2), int_word(10), e)), int_word(1024));
    }

    #[test]
    fn word_wrappers_report_overflow() {
        overflows(|e| jrt_math_pow(int_word(2), int_word(64), e));
    }

    /// `abs(i64::MIN)` overflows in the *core*, but cannot be reached through a
    /// word: AOT integers are 63-bit SMIs (`JadeValue::from_int` shifts left by
    /// one and drops the high bit), so `int_word(i64::MIN)` does not round-trip
    /// to `i64::MIN` and the wrapper sees a representable value instead.
    ///
    /// That gap is a real divergence between the engines — the VM's integers are
    /// full i64 — and it is wider than this one function: `print(i64::MAX)` is
    /// exact under `jade run` and prints `-1` under `jade build`. It is not
    /// fixable here; it is a property of the value representation.
    #[test]
    fn i64_min_is_not_representable_as_a_word() {
        // A tagged word holds 63 bits, so i64::MIN has no representation.
        // `try_from_int` is how that stops being silent truncation.
        assert!(JadeValue::try_from_int(i64::MIN).is_none());
        assert!(JadeValue::try_from_int(JadeValue::INT_MIN).is_some());
        assert!(JadeValue::try_from_int(JadeValue::INT_MAX).is_some());
        assert!(JadeValue::try_from_int(JadeValue::INT_MAX + 1).is_none());
        // The core checks independently of the representation.
        assert_eq!(abs(Num::Int(i64::MIN)), Err(MathErr::Overflow));
    }

    /// Rounding follows the same kind rule as `floor`: an int passes through
    /// untouched, a float answers an int.
    #[test]
    fn round_and_trunc_preserve_kind() {
        assert_eq!(round(Num::Int(7)), Num::Int(7));
        assert_eq!(trunc(Num::Int(-7)), Num::Int(-7));
        assert_eq!(round(Num::Float(2.5)), Num::Int(3), "ties go away from zero");
        assert_eq!(round(Num::Float(-2.5)), Num::Int(-3));
        // The one place trunc and floor differ.
        assert_eq!(trunc(Num::Float(-1.9)), Num::Int(-1));
        assert_eq!(floor(Num::Float(-1.9)), Num::Int(-2));
    }

    /// `f64::signum` answers 1.0 for NaN, which would invent an order for a
    /// value that has none.
    #[test]
    fn sign_keeps_nan_as_nan() {
        assert_eq!(sign(Num::Int(-3)), Num::Int(-1));
        assert_eq!(sign(Num::Int(0)), Num::Int(0));
        assert_eq!(sign(Num::Float(-0.5)), Num::Float(-1.0));
        assert_eq!(sign(Num::Float(0.0)), Num::Float(0.0));
        assert!(matches!(sign(Num::Float(f64::NAN)), Num::Float(f) if f.is_nan()));
    }

    /// Int only when all three are, matching `min`/`max`.
    #[test]
    fn clamp_is_int_only_when_all_three_are() {
        assert_eq!(clamp(Num::Int(15), Num::Int(0), Num::Int(10)), Num::Int(10));
        assert_eq!(clamp(Num::Int(-5), Num::Int(0), Num::Int(10)), Num::Int(0));
        assert!(matches!(clamp(Num::Float(1.5), Num::Int(0), Num::Int(10)), Num::Float(_)));
        // A reversed range does not panic the way the standard library's
        // `clamp` does; the trailing `min` wins, so the answer is `hi`.
        assert_eq!(clamp(Num::Int(5), Num::Int(10), Num::Int(0)), Num::Int(0));
    }

    /// The transcendentals are float whatever they are handed, following sqrt.
    #[test]
    fn transcendentals_are_always_float() {
        for n in [ln(Num::Int(1)), exp(Num::Int(0)), sin(Num::Int(0)), atan(Num::Int(0))] {
            assert!(matches!(n, Num::Float(_)), "expected a float, got {n:?}");
        }
        assert_eq!(log2(Num::Int(8)), Num::Float(3.0));
        assert_eq!(log10(Num::Int(1000)), Num::Float(3.0));
        assert_eq!(hypot(Num::Int(3), Num::Int(4)), Num::Float(5.0));
    }

    /// An int is never NaN and never infinite, so the predicates answer for a
    /// kind that cannot hold either.
    #[test]
    fn predicates_answer_for_every_kind() {
        assert!(is_nan(Num::Float(f64::NAN)));
        assert!(!is_nan(Num::Float(1.0)));
        assert!(!is_nan(Num::Int(1)));
        assert!(is_inf(Num::Float(f64::INFINITY)));
        assert!(is_inf(Num::Float(f64::NEG_INFINITY)));
        assert!(!is_inf(Num::Int(i64::MAX)));
    }

    /// `inf` and `nan` are reachable no other way — the lexer caps a numeric
    /// literal, so neither can be written down in Jade source.
    #[test]
    fn the_constants_are_the_only_way_to_write_them() {
        assert!(matches!(inf(), Num::Float(f) if f.is_infinite() && f > 0.0));
        assert!(matches!(nan(), Num::Float(f) if f.is_nan()));
        assert_eq!(tau(), Num::Float(core::f64::consts::TAU));
    }
}
