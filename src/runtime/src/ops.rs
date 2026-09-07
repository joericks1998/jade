//! Tag-erased ("dynamic") arithmetic, comparison, and truthiness for AOT.
//!
//! This module is the AOT-facing adapter: it decodes tagged [`JadeValue`]
//! operands into the representation-agnostic [`crate::dynop`] core, runs the
//! **single** shared decision logic there, and re-encodes the result into
//! tagged values (boxing floats, concatenating strings). The VM routes through
//! the same `dynop` core natively, so the two engines cannot disagree.
//!
//! ## Errors are values, never `longjmp`s
//!
//! The AOT runtime raises catchable exceptions with `longjmp`, which cannot
//! cross a Rust stack frame without undefined behavior. So these functions
//! **return** [`DynErr`] instead of raising; the thin C forwarders in
//! `common.c` translate the error into `jade_exc_throw_typed`, keeping the
//! `longjmp` entirely on the C side.
//!
//! ## Semantics = the VM's (see [`crate::dynop`])
//!
//! Overflow-checked int `+ - *`; bool is not numeric; `==`/`!=` need matching
//! kinds; ordering allows int/float mixing and bool/bool; div/mod by zero
//! error. (Historically this file matched the *old permissive AOT* behavior;
//! as of the Option-A unification it matches the VM.)

use crate::dynop::{self, DynErr, Kind, Op, Outcome};
use crate::float::{box_float, unbox_float};
use crate::num::ipow;
use crate::strval;
use crate::value::JadeValue;

/// Result of a dynamic op that produces a value.
pub type OpResult = Result<JadeValue, DynErr>;

/// Decode a tagged value into the [`dynop`] core's kind view. Scalars carry
/// their payload; strings and everything else are markers (string bytes and
/// heap objects are handled by this adapter / rejected by the core).
#[inline]
fn kind(v: JadeValue) -> Kind {
    if v.is_int() {
        Kind::Int(v.as_int())
    } else if v.is_float() {
        Kind::Float(unbox_float(v))
    } else if v.is_bool() {
        Kind::Bool(v.as_bool())
    } else if v.is_str() {
        Kind::Str
    } else if v.is_nil() {
        Kind::Nil
    } else {
        Kind::Other
    }
}

/// Numeric coercion for the `math.*` helpers (`pow`/`to_double`), which are
/// builtins rather than dynamic operators — kept permissive (int/float/bool) as
/// before. The dynamic *operators* use the strict `dynop` core instead.
#[inline]
fn as_num(v: JadeValue) -> Option<f64> {
    if v.is_int() {
        Some(v.as_int() as f64)
    } else if v.is_float() {
        Some(unbox_float(v))
    } else if v.is_bool() {
        Some(if v.as_bool() { 1.0 } else { 0.0 })
    } else {
        None
    }
}

/// Tag an integer result, or report overflow.
///
/// `dynop` checks its arithmetic against i64, but a tagged word only holds 63
/// bits — one goes to the tag. So a result could pass `checked_add` and still
/// not survive `from_int`: `(2^62 - 1) + 1` fits an i64 and does not fit a word,
/// and used to come back as a large negative number. Same class of bug as
/// `print(9223372036854775807)` compiling to `-1`.
///
/// Reporting it as `Overflow` puts it under the error the language already
/// raises when arithmetic leaves the representable range.
#[inline]
fn int_result(v: i64) -> OpResult {
    JadeValue::try_from_int(v).ok_or(DynErr::Overflow)
}

/// Re-encode a core [`Outcome`] into a tagged value (boxing floats, doing the
/// string concat for `+`).
fn finish(out: Outcome, a: JadeValue, b: JadeValue) -> OpResult {
    match out {
        Outcome::Int(v) => int_result(v),
        Outcome::Float(v) => Ok(box_float(v)),
        Outcome::Concat => {
            let p = crate::string::concat(a.as_ptr() as *const u8, b.as_ptr() as *const u8);
            Ok(JadeValue::from_str_ptr(p as *const ()))
        }
        Outcome::Err(e) => Err(e),
        Outcome::Bool(_) | Outcome::StrRel => {
            unreachable!("arithmetic ops never yield bool/strrel")
        }
    }
}

// ── char ───────────────────────────────────────────────────────────────────
//
// A char behaves as the one-character string spelling it, so `s[0] == "a"` kept
// working when indexing began yielding a char in v1.2.1. The core `dynop` sees
// only scalar kinds and string *markers*, and a char has no string bytes to
// point at, so these three entry points materialize one first. The temporary
// carries the char's trust and is freed before returning; without the trust the
// model would be escaped by `tainted[0] + ""`.

/// Build a temporary Jade string holding one char. Caller frees it.
fn char_temp(v: JadeValue) -> *mut u8 {
    let c = v.as_char().unwrap_or('\u{0}');
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    let p = crate::string::new(s.len(), v.char_trust());
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
    }
    p
}

/// Run `f` with both operands as strings, materializing either char operand.
fn with_char_as_str<T>(a: JadeValue, b: JadeValue, f: impl FnOnce(JadeValue, JadeValue) -> T) -> T {
    let (ta, tb) = (a.is_char().then(|| char_temp(a)), b.is_char().then(|| char_temp(b)));
    let av = ta.map_or(a, |p| JadeValue::from_str_ptr(p as *const ()));
    let bv = tb.map_or(b, |p| JadeValue::from_str_ptr(p as *const ()));
    let out = f(av, bv);
    if let Some(p) = ta {
        crate::string::free_str(p);
    }
    if let Some(p) = tb {
        crate::string::free_str(p);
    }
    out
}

/// Whether either operand is a char, and so needs the string treatment.
#[inline]
fn either_char(a: JadeValue, b: JadeValue) -> bool {
    a.is_char() || b.is_char()
}

// ── Arithmetic (delegate to the shared core) ───────────────────────────────

pub fn add(a: JadeValue, b: JadeValue) -> OpResult {
    if either_char(a, b) {
        return with_char_as_str(a, b, |x, y| {
            finish(dynop::binop(Op::Add, kind(x), kind(y)), x, y)
        });
    }
    finish(dynop::binop(Op::Add, kind(a), kind(b)), a, b)
}
pub fn sub(a: JadeValue, b: JadeValue) -> OpResult {
    finish(dynop::binop(Op::Sub, kind(a), kind(b)), a, b)
}
pub fn mul(a: JadeValue, b: JadeValue) -> OpResult {
    finish(dynop::binop(Op::Mul, kind(a), kind(b)), a, b)
}
pub fn div(a: JadeValue, b: JadeValue) -> OpResult {
    finish(dynop::binop(Op::Div, kind(a), kind(b)), a, b)
}
pub fn rem(a: JadeValue, b: JadeValue) -> OpResult {
    finish(dynop::binop(Op::Mod, kind(a), kind(b)), a, b)
}

pub fn neg(a: JadeValue) -> OpResult {
    match dynop::neg(kind(a)) {
        Outcome::Int(v) => int_result(v),
        Outcome::Float(v) => Ok(box_float(v)),
        Outcome::Err(e) => Err(e),
        _ => unreachable!("neg yields int/float/err"),
    }
}

/// Three-way ordering `-1/0/1` for `< > <= >=` (codegen applies the predicate).
/// Derived from the shared `dynop` per-operator logic so validity and numeric
/// ordering match the VM; strings are byte-compared here (the core only marks
/// them). Errors ([`DynErr::Type`]) on non-orderable kinds.
pub fn cmp(a: JadeValue, b: JadeValue) -> Result<i32, DynErr> {
    if either_char(a, b) {
        // Two chars order by scalar, which matches ordering their UTF-8
        // spellings, so this needs no allocation.
        if a.is_char() && b.is_char() {
            return Ok(match a.as_char().cmp(&b.as_char()) {
                core::cmp::Ordering::Less => -1,
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 1,
            });
        }
        return with_char_as_str(a, b, cmp);
    }
    let (ka, kb) = (kind(a), kind(b));
    if ka == Kind::Str && kb == Kind::Str {
        return Ok(unsafe {
            strval::cmp_bounded(a.as_ptr() as *const u8, b.as_ptr() as *const u8)
        });
    }
    match dynop::binop(Op::Lt, ka, kb) {
        Outcome::Bool(true) => Ok(-1),
        Outcome::Bool(false) => match dynop::binop(Op::Gt, ka, kb) {
            Outcome::Bool(true) => Ok(1),
            Outcome::Bool(false) => Ok(0),
            Outcome::Err(e) => Err(e),
            _ => unreachable!(),
        },
        Outcome::Err(e) => Err(e),
        _ => unreachable!("non-string ordering yields bool/err"),
    }
}

/// Equality (`1`/`0`), VM-strict. Errors ([`DynErr::Type`]) on cross-kind
/// operands (e.g. `2 == 2.0`, `1 == "x"`) — codegen negates the result for `!=`.
pub fn eq(a: JadeValue, b: JadeValue) -> Result<i32, DynErr> {
    if either_char(a, b) {
        // Trust is provenance, not identity: compare the scalar and tag only.
        if a.is_char() && b.is_char() {
            return Ok((a.char_bits() == b.char_bits()) as i32);
        }
        return with_char_as_str(a, b, eq);
    }
    match dynop::binop(Op::Eq, kind(a), kind(b)) {
        Outcome::Bool(v) => Ok(v as i32),
        Outcome::StrRel => Ok(unsafe {
            strval::eq_bounded(a.as_ptr() as *const u8, b.as_ptr() as *const u8) as i32
        }),
        Outcome::Err(e) => Err(e),
        _ => unreachable!("eq yields bool/strrel/err"),
    }
}

/// Equality for *membership*, which never raises: operands of different kinds
/// are simply not equal.
///
/// [`eq`] is the `==` operator and is deliberately strict — `1 == "x"` is a
/// TypeError, and so is `2 == 2.0`, because silently comparing across kinds hides
/// bugs. Membership is a different question. `arr.contains(x)` asks whether any
/// element *is* `x`, and an element of another kind answers that with "no", not
/// with an error: you cannot ask which elements match without walking past the
/// ones that do not.
///
/// The distinction only became reachable when mixed arrays became expressible in
/// v1.1.32. Before then the two engines disagreed and nothing noticed — the VM
/// answered `true`/`false` here while a compiled binary raised
/// `'==' requires numeric operands` on the first element of another kind.
pub fn eq_total(a: JadeValue, b: JadeValue) -> bool {
    matches!(eq(a, b), Ok(1))
}

// ── math.* helpers (builtins, kept permissive) ─────────────────────────────

pub fn pow(a: JadeValue, b: JadeValue) -> OpResult {
    if a.is_int() && b.is_int() {
        let (base, exp) = (a.as_int(), b.as_int());
        if exp >= 0 {
            return Ok(JadeValue::from_int(ipow(base, exp)));
        }
        return Ok(box_float((base as f64).powf(exp as f64)));
    }
    match (as_num(a), as_num(b)) {
        (Some(x), Some(y)) => Ok(box_float(x.powf(y))),
        _ => Err(DynErr::Type),
    }
}

/// Coerce to `f64` for `math.*` with `Unknown` args. Errors on non-numeric.
pub fn to_double(v: JadeValue) -> Result<f64, DynErr> {
    as_num(v).ok_or(DynErr::Type)
}

/// Truthiness (`1`/`0`). Never errors. (Not a dynamic operator — used by
/// explicit `bool()`.) nil→false, bool→itself, int→(x!=0), float→(x!=0),
/// string→`bool_of`.
pub fn to_bool(v: JadeValue) -> i32 {
    if v.is_int() {
        return (v.as_int() != 0) as i32;
    }
    if v.is_bool() {
        return v.as_bool() as i32;
    }
    if v.is_nil() {
        return 0;
    }
    if v.is_float() {
        return (unbox_float(v) != 0.0) as i32;
    }
    unsafe { strval::bool_of(v.as_ptr() as *const u8) as i32 }
}

// ── Bitwise and shift (int-only) ────────────────────────────────────────────
//
// These are int-only on both engines. The compiled backend used to untag both
// operand words and emit the native LLVM op with no check at all, so a str
// operand shifted its pointer bits, a float its payload, and a shift amount of
// 64 or more was undefined behaviour that in practice produced a garbage word
// which `print` then followed as a pointer. The interpreter raised on every one
// of those programs. Both engines now come through here.

/// Why a shift was refused.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShiftErr {
    /// The amount is outside `0..64`. Carries it for the message.
    Amount(i64),
    /// The shifted value is not a Jade integer (63-bit).
    Overflow,
}

/// `a << n`. The amount must be in `0..64`, and the result must still fit a
/// Jade integer: `1 << 62` is `2^62`, which is one past `INT_MAX`, so it is an
/// overflow and not a value the compiled representation can hold. The shift is
/// done in 128 bits so no bit is lost before the range check.
pub fn shl(a: i64, n: i64) -> Result<i64, ShiftErr> {
    if !(0..64).contains(&n) {
        return Err(ShiftErr::Amount(n));
    }
    let wide = (a as i128) << n;
    if wide > JadeValue::INT_MAX as i128 || wide < JadeValue::INT_MIN as i128 {
        return Err(ShiftErr::Overflow);
    }
    Ok(wide as i64)
}

/// `a >> n`, arithmetic (sign-preserving). The amount must be in `0..64`. The
/// result of a right shift never leaves the range its operand was in.
pub fn shr(a: i64, n: i64) -> Result<i64, ShiftErr> {
    if !(0..64).contains(&n) {
        return Err(ShiftErr::Amount(n));
    }
    Ok(a >> n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(n: i64) -> JadeValue {
        JadeValue::from_int(n)
    }

    #[test]
    fn int_arithmetic_stays_int() {
        assert_eq!(add(i(2), i(3)).unwrap().as_int(), 5);
        assert_eq!(sub(i(2), i(3)).unwrap().as_int(), -1);
        assert_eq!(mul(i(4), i(5)).unwrap().as_int(), 20);
        assert_eq!(div(i(7), i(2)).unwrap().as_int(), 3);
        assert_eq!(rem(i(7), i(2)).unwrap().as_int(), 1);
        assert_eq!(neg(i(9)).unwrap().as_int(), -9);
    }

    #[test]
    fn mixed_promotes_to_float() {
        let r = add(i(2), box_float(0.5)).unwrap();
        assert!(r.is_float());
        assert_eq!(unbox_float(r), 2.5);
    }

    #[test]
    fn strict_semantics_error() {
        // The kind-based divergences Option A fixes at the ops layer: bool is
        // not numeric, and cross-kind equality errors. (i64-overflow checking
        // lives in `dynop`, exercised there with real i64::MAX; it can't be
        // reached through the 63-bit SMI operands here — a known ABI residual.)
        assert_eq!(add(JadeValue::from_bool(true), i(1)), Err(DynErr::Type));
        assert_eq!(eq(i(2), box_float(2.0)), Err(DynErr::Type));
        assert_eq!(neg(JadeValue::from_bool(true)), Err(DynErr::Type));
    }

    #[test]
    fn div_and_mod_by_zero_error() {
        assert_eq!(div(i(1), i(0)), Err(DynErr::DivZero));
        assert_eq!(rem(i(1), i(0)), Err(DynErr::RemZero));
    }

    #[test]
    fn equality_and_ordering() {
        assert_eq!(eq(i(2), i(2)).unwrap(), 1);
        assert_eq!(eq(i(2), i(3)).unwrap(), 0);
        assert_eq!(eq(crate::value::NIL, i(1)).unwrap(), 0);
        assert_eq!(cmp(i(2), i(5)).unwrap(), -1);
        assert_eq!(cmp(i(5), i(2)).unwrap(), 1);
        assert_eq!(cmp(i(3), box_float(3.0)).unwrap(), 0); // int/float mix ok for ordering
        assert_eq!(cmp(i(1), JadeValue::from_bool(true)), Err(DynErr::Type));
    }

    #[test]
    fn truthiness() {
        assert_eq!(to_bool(i(0)), 0);
        assert_eq!(to_bool(i(5)), 1);
        assert_eq!(to_bool(crate::value::NIL), 0);
        assert_eq!(to_bool(box_float(0.0)), 0);
    }

    /// Membership equality answers where `==` raises. `arr.contains(x)` has to
    /// walk past elements of other kinds, so a cross-kind pair is "not equal"
    /// rather than an error.
    #[test]
    fn eq_total_answers_false_where_eq_raises() {
        // Same kind: identical to `eq`.
        assert!(eq_total(i(2), i(2)));
        assert!(!eq_total(i(2), i(3)));
        assert!(eq_total(crate::value::NIL, crate::value::NIL));

        // Cross-kind: `eq` errors, `eq_total` says "not equal" and never raises.
        for (a, b) in [(i(1), JadeValue::from_bool(true)), (i(1), box_float(1.0))] {
            assert_eq!(eq(a, b), Err(DynErr::Type), "expected `==` to reject");
            assert!(!eq_total(a, b), "membership must answer, not raise");
        }

        // `nil` against another kind is already answerable rather than an error,
        // so the two agree there — `eq_total` only diverges where `eq` rejects.
        assert_eq!(eq(crate::value::NIL, i(0)), Ok(0));
        assert!(!eq_total(crate::value::NIL, i(0)));
    }

    /// Kind-strict, not value-coercing: `1` is not `1.0` here either, matching
    /// `1 == 1.0` being a type error rather than true.
    #[test]
    fn eq_total_does_not_coerce_numerics() {
        assert!(!eq_total(i(1), box_float(1.0)));
        assert!(eq_total(box_float(1.0), box_float(1.0)));
    }
}
