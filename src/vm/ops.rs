//! Dynamic (runtime-typed) operator evaluation for the VM.
//!
//! When the type inferrer can't specialize an operator, the interpreter falls
//! back to these helpers, which decide arithmetic/comparison through the shared
//! `jade_runtime::dynop` core (so the VM and AOT backend cannot diverge) and own
//! the VM-specific cases — bitwise/shift, `in`, indexing, and unary ops.

use super::*;

/// Decode a `VmValue` into the shared [`dynop`] core's kind view. Scalars carry
/// their payload; strings and every other value are markers (string bytes and
/// error type-names are recovered from the original `VmValue` by the caller).
pub(crate) fn vm_kind(v: &VmValue) -> dynop::Kind {
    match v {
        VmValue::Int(i) => dynop::Kind::Int(*i),
        VmValue::Float(f) => dynop::Kind::Float(*f),
        VmValue::Bool(b) => dynop::Kind::Bool(*b),
        VmValue::Str(_) => dynop::Kind::Str,
        VmValue::Nil => dynop::Kind::Nil,
        _ => dynop::Kind::Other,
    }
}

/// The arithmetic/comparison operators the shared core decides; everything else
/// (bitwise, shift, `in`, short-circuit) stays VM-owned.
pub(crate) fn binop_to_dynop(op: &BinOpKind) -> Option<dynop::Op> {
    use BinOpKind as B;
    use dynop::Op as O;
    Some(match op {
        B::Add => O::Add,
        B::Sub => O::Sub,
        B::Mul => O::Mul,
        B::Div => O::Div,
        B::Mod => O::Mod,
        B::Eq => O::Eq,
        B::Ne => O::Ne,
        B::Lt => O::Lt,
        B::Gt => O::Gt,
        B::Le => O::Le,
        B::Ge => O::Ge,
        _ => return None,
    })
}

/// Apply an equality/ordering operator to two already-known strings (the shared
/// core defers string bytes to us via `Outcome::StrRel`).
pub(crate) fn apply_str_rel(op: &BinOpKind, a: &str, b: &str) -> bool {
    use BinOpKind::*;
    match op {
        Eq => a == b,
        Ne => a != b,
        Lt => a < b,
        Gt => a > b,
        Le => a <= b,
        Ge => a >= b,
        _ => unreachable!("apply_str_rel only for eq/ordering ops"),
    }
}

/// Wrap a checked-arithmetic result as a Jade integer, or report overflow.
///
/// Jade integers are 63-bit. The compiled representation spends one bit on the
/// value tag, and the language follows the representation so the two engines
/// accept exactly the same programs — `(2^62 - 1) + 1` fits an i64 but is not a
/// Jade integer, and used to compute here while raising under `jade build`.
///
/// The bound lives in `jade_runtime::value` so there is one definition of what
/// a Jade integer is; the dynamic path reaches it through `dynop`.
pub(crate) fn int_ok(v: Option<i64>, span: Span) -> Result<VmValue> {
    match v {
        Some(i) if jade_runtime::value::JadeValue::int_fits(i) => Ok(VmValue::Int(i)),
        _ => Err(JadeError::IntegerOverflow { span }),
    }
}

/// Wrap a shared-core shift result as a Jade integer, or report why not.
///
/// The shift itself lives in `jade_runtime::ops` beside the compiled backend's
/// `jrt_shl_any`, so `1 << 62` — one past `INT_MAX` — is an overflow on both
/// engines rather than a value here and a wrapped word there.
pub(crate) fn shift_ok(
    r: core::result::Result<i64, jade_runtime::ops::ShiftErr>,
    span: Span,
) -> Result<VmValue> {
    use jade_runtime::ops::ShiftErr;
    match r {
        Ok(v) => Ok(VmValue::Int(v)),
        Err(ShiftErr::Amount(amount)) => Err(JadeError::InvalidShift { amount, span }),
        Err(ShiftErr::Overflow) => Err(JadeError::IntegerOverflow { span }),
    }
}

/// Map a shared-core error to the VM's `JadeError`, reconstructing the exact
/// message the VM produced before it delegated (tests match on the variants).
pub(crate) fn map_dynop_err(
    e: dynop::DynErr,
    op: &BinOpKind,
    l: &VmValue,
    r: &VmValue,
    span: Span,
) -> JadeError {
    use BinOpKind::*;
    use dynop::DynErr as D;
    match e {
        D::Overflow => JadeError::IntegerOverflow { span },
        D::DivZero => JadeError::DivisionByZero { span },
        D::RemZero => JadeError::RemainderByZero { span },
        D::Type => {
            // Both halves are spelled with the operator's *symbol*, matching what
            // a compiled binary says (`throw_num_type` / `throw_cmp_type` in
            // runtime_aot/common.c). Arithmetic used to interpolate the Rust enum
            // variant here, so `1 + "x"` reported `Add requires numeric operands`
            // under `jade run` and `'+' requires numeric operands` from the same
            // program built — a leaked internal name and a divergence at once.
            let sym = match op {
                Add => "+",
                Sub => "-",
                Mul => "*",
                Div => "/",
                Mod => "%",
                Eq => "==",
                Ne => "!=",
                Lt => "<",
                Gt => ">",
                Le => "<=",
                Ge => ">=",
                _ => "?",
            };
            let message = match op {
                Add | Sub | Mul | Div | Mod => format!("'{sym}' requires numeric operands"),
                _ => format!(
                    "'{sym}' cannot compare {} and {}",
                    value_type_name(l),
                    value_type_name(r)
                ),
            };
            JadeError::TypeError { message, span }
        }
    }
}

/// Turn a shared-core [`dynop::Outcome`] back into a `VmValue`, doing the
/// string byte-work the core deferred (`Concat` for `+`, `StrRel` for
/// comparisons). `l`/`r` are the original operands (needed for strings + errors).
pub(crate) fn finish_dynop(
    out: dynop::Outcome,
    op: &BinOpKind,
    l: VmValue,
    r: VmValue,
    span: Span,
) -> Result<VmValue> {
    use dynop::Outcome as O;
    match out {
        O::Int(v) => Ok(VmValue::Int(v)),
        O::Float(v) => Ok(VmValue::Float(v)),
        O::Bool(v) => Ok(VmValue::Bool(v)),
        O::Concat => match (l, r) {
            (VmValue::Str(a), VmValue::Str(b)) => {
                // The result is as untrustworthy as its most untrustworthy
                // part. Without this the model is escaped by `"" + tainted`.
                let trust = jade_runtime::trust::combine(a.trust(), b.trust());
                Ok(VmValue::Str(JStr::with_trust(format!("{}{}", a.as_str(), b.as_str()), trust)))
            }
            _ => unreachable!("Concat is only produced for two strings"),
        },
        O::StrRel => match (&l, &r) {
            (VmValue::Str(a), VmValue::Str(b)) => Ok(VmValue::Bool(apply_str_rel(op, a, b))),
            _ => unreachable!("StrRel is only produced for two strings"),
        },
        O::Err(e) => Err(map_dynop_err(e, op, &l, &r, span)),
    }
}

/// Spell a char as the one-character string it stands for, keeping its trust.
fn char_to_str(c: &jade_runtime::trust::JChar) -> VmValue {
    VmValue::Str(JStr::with_trust(c.ch().to_string(), c.trust()))
}

/// Apply a comparison to two chars without allocating.
fn char_rel(op: &BinOpKind, a: char, b: char) -> Option<bool> {
    use BinOpKind::*;
    Some(match op {
        Eq => a == b,
        Ne => a != b,
        Lt => a < b,
        Gt => a > b,
        Le => a <= b,
        Ge => a >= b,
        _ => return None,
    })
}

pub(crate) fn eval_binop_dynamic(
    op: &BinOpKind,
    l: VmValue,
    r: VmValue,
    span: Span,
) -> Result<VmValue> {
    use BinOpKind::*;

    // ── char ──────────────────────────────────────────────────────────────
    //
    // A char behaves as the one-character string spelling it. That is a
    // deliberate exception to the "no cross-type comparison" rule: indexing a
    // string yields a char as of v1.2.1, so without it every existing
    // `if s[0] == "a"` would have silently changed meaning.
    //
    // Two chars compare on their scalars directly. Comparing scalars and
    // comparing their UTF-8 spellings give the same answer, and a scan loop
    // testing `c == "x"` should not allocate once per character.
    match (&l, &r) {
        (VmValue::Char(a), VmValue::Char(b)) => {
            if let Some(v) = char_rel(op, a.ch(), b.ch()) {
                return Ok(VmValue::Bool(v));
            }
        }
        (VmValue::Char(a), VmValue::Str(_)) => {
            return eval_binop_dynamic(op, char_to_str(a), r, span);
        }
        (VmValue::Str(_), VmValue::Char(b)) => {
            return eval_binop_dynamic(op, l, char_to_str(b), span);
        }
        _ => {}
    }
    // Concatenation still has to build a string, so it takes the slow path.
    if matches!(op, Add) && matches!((&l, &r), (VmValue::Char(_), VmValue::Char(_))) {
        let (VmValue::Char(a), VmValue::Char(b)) = (&l, &r) else { unreachable!() };
        return eval_binop_dynamic(op, char_to_str(a), char_to_str(b), span);
    }

    // Arithmetic + comparison are decided by the shared `dynop` core, so the VM
    // and AOT cannot diverge on overflow/bool/cross-kind rules.
    if let Some(dop) = binop_to_dynop(op) {
        let out = dynop::binop(dop, vm_kind(&l), vm_kind(&r));
        return finish_dynop(out, op, l, r, span);
    }
    // Ops the VM owns: int-only bitwise/shift, container membership, short-circuit.
    match op {
        BitAnd => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => Ok(VmValue::Int(a & b)),
            (l, r) => Err(JadeError::TypeError {
                message: format!(
                    "'&' requires int operands, got {} and {}",
                    value_type_name(&l),
                    value_type_name(&r)
                ),
                span,
            }),
        },
        BitOr => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => Ok(VmValue::Int(a | b)),
            (l, r) => Err(JadeError::TypeError {
                message: format!(
                    "'|' requires int operands, got {} and {}",
                    value_type_name(&l),
                    value_type_name(&r)
                ),
                span,
            }),
        },
        BitXor => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => Ok(VmValue::Int(a ^ b)),
            (l, r) => Err(JadeError::TypeError {
                message: format!(
                    "'^' requires int operands, got {} and {}",
                    value_type_name(&l),
                    value_type_name(&r)
                ),
                span,
            }),
        },
        Shl => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => shift_ok(jade_runtime::ops::shl(a, b), span),
            _ => Err(JadeError::TypeError {
                message: "'<<' requires int operands".to_string(),
                span,
            }),
        },
        Shr => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => shift_ok(jade_runtime::ops::shr(a, b), span),
            _ => Err(JadeError::TypeError {
                message: "'>>' requires int operands".to_string(),
                span,
            }),
        },
        In => vm_contains(l, r, span).map(VmValue::Bool),
        NotIn => vm_contains(l, r, span).map(|b| VmValue::Bool(!b)),
        And | Or => unreachable!("short-circuit ops must not reach BinOp dynamic dispatch"),
        _ => unreachable!("arithmetic/comparison handled by the shared core"),
    }
}

/// Equality for *membership*, which never raises: values of different kinds are
/// simply not equal.
///
/// Decided by the same shared core as `==` (`dynop::binop(Op::Eq, …)`), with the
/// error case answered `false` instead of raised. `==` is strict across kinds on
/// purpose — `1 == "x"` is a TypeError — but `arr.contains(x)` cannot ask which
/// elements match without walking past the ones that do not.
///
/// The AOT half is `jrt_core_eq_total`, reached from `jrt_in_any`. Keep the two
/// answering alike: until v1.1.32 this returned `false` where a compiled binary
/// raised, and mixed arrays were unwritable, so nothing caught it.
pub(crate) fn vm_scalar_eq(a: &VmValue, b: &VmValue) -> bool {
    match dynop::binop(dynop::Op::Eq, vm_kind(a), vm_kind(b)) {
        dynop::Outcome::Bool(v) => v,
        // Two strings: the core defers the byte comparison to the caller, since
        // it holds no string representation of its own.
        dynop::Outcome::StrRel => match (a, b) {
            (VmValue::Str(x), VmValue::Str(y)) => x == y,
            _ => false,
        },
        // Different kinds, or a kind the core does not compare (`Other` — arrays,
        // structs, functions). Not equal, and not an error.
        _ => false,
    }
}

pub(crate) fn vm_contains(needle: VmValue, haystack: VmValue, span: Span) -> Result<bool> {
    match haystack {
        VmValue::Array(arc) => {
            let arr = arc.lock();
            Ok(arr.iter().any(|v| vm_scalar_eq(v, &needle)))
        }
        VmValue::Dict(map) => {
            let key = match needle {
                VmValue::Str(s) => s,
                ref other => {
                    return Err(JadeError::TypeError {
                        message: format!(
                            "'in' dict key must be str, got {}",
                            value_type_name(other)
                        ),
                        span,
                    });
                }
            };
            Ok(map.contains_key(&key))
        }
        VmValue::Str(s) => {
            let sub = match needle {
                VmValue::Str(sub) => sub,
                ref other => {
                    return Err(JadeError::TypeError {
                        message: format!(
                            "'in' substring must be str, got {}",
                            value_type_name(other)
                        ),
                        span,
                    });
                }
            };
            Ok(s.contains(sub.as_str()))
        }
        ref other => Err(JadeError::TypeError {
            message: format!("'in' requires array, dict, or str, got {}", value_type_name(other)),
            span,
        }),
    }
}

pub(crate) fn eval_unaryop_dynamic(op: &UnaryOpKind, v: VmValue, span: Span) -> Result<VmValue> {
    match op {
        UnaryOpKind::BitNot => match v {
            VmValue::Int(i) => Ok(VmValue::Int(!i)),
            ref v => Err(JadeError::TypeError {
                message: format!("'~' requires int, got {}", value_type_name(v)),
                span,
            }),
        },
        UnaryOpKind::Not => match v {
            VmValue::Bool(b) => Ok(VmValue::Bool(!b)),
            ref v => Err(JadeError::TypeError {
                message: format!("'!' requires bool, got {}", value_type_name(v)),
                span,
            }),
        },
        // Numeric negation is decided by the shared core (int/float only).
        UnaryOpKind::Neg => match dynop::neg(vm_kind(&v)) {
            dynop::Outcome::Int(i) => Ok(VmValue::Int(i)),
            dynop::Outcome::Float(f) => Ok(VmValue::Float(f)),
            _ => Err(JadeError::TypeError {
                message: format!("unary '-' requires int or float, got {}", value_type_name(&v)),
                span,
            }),
        },
    }
}

pub(crate) fn cmp_dynamic(
    slots: &[VmValue],
    l: Reg,
    r: Reg,
    op: &str,
    span: Span,
) -> Result<VmValue> {
    let lv = get(slots, l).clone();
    let rv = get(slots, r).clone();
    // The `CmpEq..CmpGe` opcodes map onto the same shared comparison core as
    // the `BinOp` path, so all three of the VM's former comparison copies (this
    // one, `eval_binop_dynamic`, and the AOT runtime) are now one implementation.
    let bop = match op {
        "==" => BinOpKind::Eq,
        "!=" => BinOpKind::Ne,
        "<" => BinOpKind::Lt,
        ">" => BinOpKind::Gt,
        "<=" => BinOpKind::Le,
        ">=" => BinOpKind::Ge,
        _ => unreachable!("cmp_dynamic op: {op}"),
    };
    // Delegate rather than repeat the dynop call: this path used to be a fourth
    // copy of the comparison rules, and `char` would have had to be taught to
    // each one separately.
    eval_binop_dynamic(&bop, lv, rv, span)
}

pub(crate) fn vm_index(obj: VmValue, idx: VmValue, span: Span) -> Result<VmValue> {
    match (obj, idx) {
        (VmValue::Str(s), VmValue::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                // A character of a tainted string is still tainted, which is
                // why `JChar` carries a trust byte at all.
                Ok(VmValue::Char(jade_runtime::trust::JChar::with_trust(
                    chars[i as usize],
                    s.trust(),
                )))
            }
        }
        // An octet is an int, not a char. A byte is not a Unicode scalar, and
        // making `b[0]` look like `s[0]` would hide that they differ on any
        // non-ASCII input.
        (VmValue::Bytes(b), VmValue::Int(i)) => {
            // One guard for the length and the read. `parking_lot::Mutex` is not
            // reentrant, so taking a second while this one is alive would hang
            // the process with no panic and no message.
            let g = b.lock();
            let len = g.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(VmValue::Int(g.as_slice()[i as usize] as i64))
            }
        }
        // A stream indexes like the buffer it is.
        (VmValue::Stream(buf), VmValue::Int(i)) => {
            let guard = buf.lock();
            let len = guard.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(guard[i as usize].clone())
            }
        }
        (VmValue::Array(arc), VmValue::Int(i)) => {
            let guard = arc.lock();
            let len = guard.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(guard[i as usize].clone())
            }
        }
        (VmValue::Dict(m), VmValue::Str(k)) => {
            m.get(&k).cloned().ok_or_else(|| JadeError::KeyNotFound { key: k.to_string(), span })
        }
        (VmValue::Dict(_), idx) => Err(JadeError::TypeError {
            message: format!("dict index must be str, got {}", value_type_name(&idx)),
            span,
        }),
        (obj, idx) => Err(JadeError::TypeError {
            message: format!(
                "value of type {} is not indexable with {}",
                value_type_name(&obj),
                value_type_name(&idx)
            ),
            span,
        }),
    }
}
