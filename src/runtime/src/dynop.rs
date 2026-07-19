//! The **single** decision core for dynamic (tag-erased) binary operations and
//! numeric negation — shared by the bytecode VM and AOT-compiled binaries so
//! they cannot disagree.
//!
//! ## Why this exists
//!
//! Historically the VM (`vm.rs::eval_binop_dynamic` / `cmp_dynamic` /
//! `cmp_order`) and the AOT path (`ops.rs` → C) each implemented dynamic
//! dispatch independently, and they diverged in exactly the cases a
//! VM-goldened difftest can't see (because they *error* in the VM): integer
//! overflow, bool-in-arithmetic, cross-kind `==`, bool comparison. This module
//! is the reconciliation: **one** implementation of the decision logic, which
//! both engines route through.
//!
//! ## Whose semantics (decided 2026-07-17: Option A — the VM's, strict)
//!
//! The VM is the language's source of truth, so this core reproduces the VM's
//! rules exactly:
//!  * `+ - *` on two ints are **overflow-checked** (→ [`DynErr::Overflow`]);
//!  * **bool is not numeric** — `true + 1`, `1 < true`, `-true` all error;
//!  * `==`/`!=` require **matching kinds** (no cross-kind `2 == 2.0`); `nil`
//!    compares equal only to `nil`, unequal to everything else (no error);
//!  * ordering (`< > <= >=`) allows int/float mixing and bool/bool, nothing else;
//!  * `/ %` by zero error ([`DynErr::DivZero`] / [`DynErr::RemZero`]).
//!
//! ## Representation-agnostic
//!
//! Callers decode their own value into a [`Kind`] (cheap: scalars carry their
//! payload; strings and everything else are markers) and map the [`Outcome`]
//! back to their own representation and error type. **String byte-operations
//! stay caller-side** — the core only signals [`Outcome::Concat`] /
//! [`Outcome::StrRel`], because the VM holds a Rust `String` and AOT holds a
//! tagged `char*`, and concatenation/ordering of strings has never diverged.
//!
//! Note: this unifies the dispatch *logic*. A residual representation gap
//! remains out of scope — AOT ints are 63-bit (SMI), the VM's are 64-bit — so
//! the largest magnitudes still differ; that is an ABI tradeoff, not a dispatch
//! disagreement.

/// A value decoded to just the kind information dynamic dispatch needs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A string (payload stays with the caller).
    Str,
    Nil,
    /// Any other value (heap object, callable, …) — never valid in these ops.
    Other,
}

/// The dynamic binary operators this core decides. Bitwise/shift/`in` are
/// int-/container-only and stay with each engine (they don't diverge).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// Failure of a dynamic op. Each engine maps these to its own error type
/// (the VM's `JadeError`, or the AOT runtime's catchable exception).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DynErr {
    /// `+ - *` on two ints overflowed i64.
    Overflow,
    /// Division by zero.
    DivZero,
    /// Modulo by zero.
    RemZero,
    /// Operand kinds are invalid for this operator.
    Type,
}

/// What the caller must do to finish an op the core decided.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Outcome {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// `Add` of two strings — caller concatenates them.
    Concat,
    /// An equality/ordering op on two strings — caller performs the byte
    /// comparison and applies the operator itself.
    StrRel,
    Err(DynErr),
}

/// Numeric coercion, **VM-strict**: only `Int`/`Float` are numbers. `Bool` is
/// deliberately *not* numeric (matches the VM's `to_floats`).
#[inline]
fn num(k: Kind) -> Option<f64> {
    match k {
        Kind::Int(i) => Some(i as f64),
        Kind::Float(f) => Some(f),
        _ => None,
    }
}

/// Decide a dynamic binary operation. See the module docs for the semantics.
pub fn binop(op: Op, a: Kind, b: Kind) -> Outcome {
    use Op::*;
    match op {
        Add => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => match x.checked_add(y) {
                Some(v) => Outcome::Int(v),
                None => Outcome::Err(DynErr::Overflow),
            },
            (Kind::Str, Kind::Str) => Outcome::Concat,
            _ => float_arith(a, b, |x, y| x + y),
        },
        Sub => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => match x.checked_sub(y) {
                Some(v) => Outcome::Int(v),
                None => Outcome::Err(DynErr::Overflow),
            },
            _ => float_arith(a, b, |x, y| x - y),
        },
        Mul => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => match x.checked_mul(y) {
                Some(v) => Outcome::Int(v),
                None => Outcome::Err(DynErr::Overflow),
            },
            _ => float_arith(a, b, |x, y| x * y),
        },
        Div => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => {
                if y == 0 {
                    Outcome::Err(DynErr::DivZero)
                } else {
                    Outcome::Int(x.wrapping_div(y))
                }
            }
            _ => match (num(a), num(b)) {
                (Some(x), Some(y)) => {
                    if y == 0.0 {
                        Outcome::Err(DynErr::DivZero)
                    } else {
                        Outcome::Float(x / y)
                    }
                }
                _ => Outcome::Err(DynErr::Type),
            },
        },
        Mod => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => {
                if y == 0 {
                    Outcome::Err(DynErr::RemZero)
                } else {
                    Outcome::Int(x.wrapping_rem(y))
                }
            }
            _ => match (num(a), num(b)) {
                (Some(x), Some(y)) => {
                    if y == 0.0 {
                        Outcome::Err(DynErr::RemZero)
                    } else {
                        Outcome::Float(x % y)
                    }
                }
                _ => Outcome::Err(DynErr::Type),
            },
        },
        Eq | Ne => {
            let ne = matches!(op, Ne);
            match (a, b) {
                (Kind::Int(x), Kind::Int(y)) => Outcome::Bool((x == y) ^ ne),
                (Kind::Float(x), Kind::Float(y)) => Outcome::Bool((x == y) ^ ne),
                (Kind::Bool(x), Kind::Bool(y)) => Outcome::Bool((x == y) ^ ne),
                (Kind::Str, Kind::Str) => Outcome::StrRel,
                (Kind::Nil, Kind::Nil) => Outcome::Bool(!ne), // == → true, != → false
                (Kind::Nil, _) | (_, Kind::Nil) => Outcome::Bool(ne), // == → false, != → true
                _ => Outcome::Err(DynErr::Type),
            }
        }
        Lt | Gt | Le | Ge => match (a, b) {
            (Kind::Int(x), Kind::Int(y)) => Outcome::Bool(ord_i(op, x, y)),
            (Kind::Float(x), Kind::Float(y)) => Outcome::Bool(ord_f(op, x, y)),
            (Kind::Int(x), Kind::Float(y)) => Outcome::Bool(ord_f(op, x as f64, y)),
            (Kind::Float(x), Kind::Int(y)) => Outcome::Bool(ord_f(op, x, y as f64)),
            (Kind::Bool(x), Kind::Bool(y)) => Outcome::Bool(ord_bool(op, x, y)),
            (Kind::Str, Kind::Str) => Outcome::StrRel,
            _ => Outcome::Err(DynErr::Type),
        },
    }
}

/// Numeric negation, VM-strict (only int/float; int negation wraps like the VM).
pub fn neg(a: Kind) -> Outcome {
    match a {
        Kind::Int(i) => Outcome::Int(i.wrapping_neg()),
        Kind::Float(f) => Outcome::Float(-f),
        _ => Outcome::Err(DynErr::Type),
    }
}

/// The float-arithmetic fallback shared by `+ - *`: both operands must be
/// numeric (int/float), else a type error.
#[inline]
fn float_arith(a: Kind, b: Kind, f: impl Fn(f64, f64) -> f64) -> Outcome {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => Outcome::Float(f(x, y)),
        _ => Outcome::Err(DynErr::Type),
    }
}

#[inline]
fn ord_i(op: Op, x: i64, y: i64) -> bool {
    match op {
        Op::Lt => x < y,
        Op::Gt => x > y,
        Op::Le => x <= y,
        Op::Ge => x >= y,
        _ => unreachable!(),
    }
}

#[inline]
fn ord_f(op: Op, x: f64, y: f64) -> bool {
    match op {
        Op::Lt => x < y,
        Op::Gt => x > y,
        Op::Le => x <= y,
        Op::Ge => x >= y,
        _ => unreachable!(),
    }
}

/// Bool ordering (false < true), matching the VM's per-operator closures.
#[inline]
fn ord_bool(op: Op, a: bool, b: bool) -> bool {
    match op {
        Op::Lt => !a && b,
        Op::Gt => a && !b,
        Op::Le => a == b || (!a && b),
        Op::Ge => a == b || (a && !b),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_arithmetic_overflow_checked() {
        assert_eq!(binop(Op::Add, Kind::Int(2), Kind::Int(3)), Outcome::Int(5));
        assert_eq!(
            binop(Op::Add, Kind::Int(i64::MAX), Kind::Int(1)),
            Outcome::Err(DynErr::Overflow)
        );
        assert_eq!(
            binop(Op::Mul, Kind::Int(i64::MAX), Kind::Int(2)),
            Outcome::Err(DynErr::Overflow)
        );
    }

    #[test]
    fn mixed_int_float_promotes() {
        assert_eq!(binop(Op::Add, Kind::Int(2), Kind::Float(0.5)), Outcome::Float(2.5));
        assert_eq!(binop(Op::Mul, Kind::Float(2.0), Kind::Int(3)), Outcome::Float(6.0));
    }

    #[test]
    fn bool_is_not_numeric() {
        // The core divergence Option A fixes: bool must error in arithmetic
        // and comparison, matching the VM (AOT used to coerce it to 0/1).
        assert_eq!(binop(Op::Add, Kind::Bool(true), Kind::Int(1)), Outcome::Err(DynErr::Type));
        assert_eq!(binop(Op::Lt, Kind::Int(1), Kind::Bool(true)), Outcome::Err(DynErr::Type));
        assert_eq!(neg(Kind::Bool(true)), Outcome::Err(DynErr::Type));
    }

    #[test]
    fn equality_requires_matching_kinds() {
        assert_eq!(binop(Op::Eq, Kind::Int(2), Kind::Int(2)), Outcome::Bool(true));
        // No cross-kind numeric equality (VM raises; AOT used to say true).
        assert_eq!(binop(Op::Eq, Kind::Int(2), Kind::Float(2.0)), Outcome::Err(DynErr::Type));
        // nil compares without error.
        assert_eq!(binop(Op::Eq, Kind::Nil, Kind::Nil), Outcome::Bool(true));
        assert_eq!(binop(Op::Eq, Kind::Nil, Kind::Int(1)), Outcome::Bool(false));
        assert_eq!(binop(Op::Ne, Kind::Nil, Kind::Int(1)), Outcome::Bool(true));
    }

    #[test]
    fn ordering_allows_int_float_and_bool() {
        assert_eq!(binop(Op::Lt, Kind::Int(1), Kind::Float(1.5)), Outcome::Bool(true));
        assert_eq!(binop(Op::Ge, Kind::Float(2.0), Kind::Int(2)), Outcome::Bool(true));
        assert_eq!(binop(Op::Lt, Kind::Bool(false), Kind::Bool(true)), Outcome::Bool(true));
        // int vs bool is NOT allowed for ordering.
        assert_eq!(binop(Op::Lt, Kind::Int(0), Kind::Bool(true)), Outcome::Err(DynErr::Type));
    }

    #[test]
    fn div_mod_by_zero() {
        assert_eq!(binop(Op::Div, Kind::Int(1), Kind::Int(0)), Outcome::Err(DynErr::DivZero));
        assert_eq!(binop(Op::Mod, Kind::Int(1), Kind::Int(0)), Outcome::Err(DynErr::RemZero));
        assert_eq!(binop(Op::Div, Kind::Float(1.0), Kind::Int(0)), Outcome::Err(DynErr::DivZero));
    }

    #[test]
    fn string_directives() {
        assert_eq!(binop(Op::Add, Kind::Str, Kind::Str), Outcome::Concat);
        assert_eq!(binop(Op::Lt, Kind::Str, Kind::Str), Outcome::StrRel);
        assert_eq!(binop(Op::Eq, Kind::Str, Kind::Str), Outcome::StrRel);
        // string paired with a non-string is a type error (except via nil in ==).
        assert_eq!(binop(Op::Add, Kind::Str, Kind::Int(1)), Outcome::Err(DynErr::Type));
        assert_eq!(binop(Op::Eq, Kind::Str, Kind::Int(1)), Outcome::Err(DynErr::Type));
        assert_eq!(binop(Op::Eq, Kind::Str, Kind::Nil), Outcome::Bool(false));
    }
}
