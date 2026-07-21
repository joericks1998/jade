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

/// Re-encode a core [`Outcome`] into a tagged value (boxing floats, doing the
/// string concat for `+`).
#[inline]
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

// ── Arithmetic (delegate to the shared core) ───────────────────────────────

pub fn add(a: JadeValue, b: JadeValue) -> OpResult {
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
    match dynop::binop(Op::Eq, kind(a), kind(b)) {
        Outcome::Bool(v) => Ok(v as i32),
        Outcome::StrRel => Ok(unsafe {
            strval::eq_bounded(a.as_ptr() as *const u8, b.as_ptr() as *const u8) as i32
        }),
        Outcome::Err(e) => Err(e),
        _ => unreachable!("eq yields bool/strrel/err"),
    }
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
}
