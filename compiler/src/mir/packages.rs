//! Lowering calls to std package functions into MIR operations.
//!
//! The checker resolves `bytes.concat(a, b)` to a `Target::Builtin` named
//! `<pkg>.<name>`, and `ops.rs` hands each such call here. Each function has
//! its own `Op`, and the argument types the checker settled pick it, so the
//! backend never looks at a kind at run time. The packages left here are the
//! value types' own, `array` and `bytes`, and the two that only the compiler's
//! own Jade can name: `__sys`, the std library's hooks, and `__infer`, the
//! inference prelude's.
//!
//! A package written in Jade, such as `std::fs`, never reaches here: its
//! functions are ordinary functions of the program. What it asks of `__sys`
//! does, as `Op::Sys`.

use super::lower::{Fx, internal, not_yet};
use super::*;
use crate::frontend::ast::Expr;
use crate::frontend::error::{JadeError, Span};

type Result<T> = std::result::Result<T, JadeError>;

impl<'a, 'l> Fx<'a, 'l> {
    /// Purpose: This function lowers a std package function the checker did
    /// not route to a method.
    ///
    /// Inputs:
    /// - `pkg`, `f`: the package and function.
    /// - `args`: the arguments.
    /// - `et`: the call's type.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    pub(super) fn package_fn(
        &mut self,
        pkg: &str,
        f: &str,
        args: &'a [Expr],
        et: &Ty,
        span: Span,
    ) -> Result<(Operand, Ty)> {
        match (pkg, f) {
            ("array", "sort" | "reverse") => {
                let first =
                    args.first().ok_or_else(|| internal("array function with no array", span))?;
                let (v, t) = self.expr(first)?;
                let Ty::Array(el) = &t else {
                    return Err(internal("array function on a non-array", span));
                };
                let op = if f == "sort" {
                    Op::ArraySorted((**el).clone())
                } else {
                    Op::ArrayReversed((**el).clone())
                };
                Ok((self.assign_temp(et.clone(), Rvalue::Op(op, vec![v])), et.clone()))
            }
            // A hook takes exactly the types it declares; an optional argument
            // is widened to the hook's optional parameter.
            ("__sys", _) => {
                let h = SysFn::from_name(f)
                    .ok_or_else(|| internal(format!("an unknown __sys hook '{f}'"), span))?;
                let mut ops = Vec::with_capacity(args.len());
                for (a, want) in args.iter().zip(h.params()) {
                    let (v, t) = self.expr(a)?;
                    ops.push(self.coerce(v, &t, &want, span)?);
                }
                self.package_op(Op::Sys(h), ops, h.result())
            }
            ("__infer", _) => {
                let (m, t) = match f {
                    "data" => (InferFn::Data, Ty::Str),
                    "config" => (InferFn::Config, Ty::Int),
                    "char_class" => (InferFn::CharClass, Ty::Int),
                    "taint" => (InferFn::Taint, Ty::Str),
                    "text" => (InferFn::Text, Ty::Str),
                    "ready" => (InferFn::Ready, Ty::Bool),
                    "map" => (InferFn::Map, Ty::Nil),
                    "setup" => (InferFn::Setup, Ty::Nil),
                    "next" => (InferFn::Next, Ty::Int),
                    "conversation" => (InferFn::Conversation, Ty::Int),
                    other => {
                        return Err(internal(
                            format!("an unknown __infer function '{other}'"),
                            span,
                        ));
                    }
                };
                let ops = self.operands(args)?;
                self.package_op(Op::Infer(m), ops, t)
            }
            ("bytes", "zeros" | "from_ints" | "concat") => {
                let mut ops = Vec::new();
                for a in args {
                    ops.push(self.expr(a)?.0);
                }
                let op = match f {
                    "zeros" => Op::BytesZeros,
                    "from_ints" => Op::BytesFromInts,
                    _ => Op::BytesConcat,
                };
                Ok((self.assign_temp(Ty::Bytes, Rvalue::Op(op, ops)), Ty::Bytes))
            }
            _ => Err(not_yet(format!("std/{pkg}"), span)),
        }
    }

    /// Purpose: This function lowers each argument in order.
    fn operands(&mut self, args: &'a [Expr]) -> Result<Vec<Operand>> {
        let mut ops = Vec::with_capacity(args.len());
        for a in args {
            ops.push(self.expr(a)?.0);
        }
        Ok(ops)
    }

    /// Purpose: This function emits a package operation, as a statement when
    /// it returns nil.
    fn package_op(&mut self, op: Op, ops: Vec<Operand>, t: Ty) -> Result<(Operand, Ty)> {
        if t == Ty::Nil {
            self.emit(Stmt::Eval(Rvalue::Op(op, ops)));
            return Ok((Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil));
        }
        Ok((self.assign_temp(t.clone(), Rvalue::Op(op, ops)), t))
    }
}
