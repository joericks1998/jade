//! Lowering calls to builtins and primitive methods into MIR operations.
//!
//! The checker resolves `len(x)`, `s.upper()` and `array.sort(a)` to a
//! `Target::Builtin` naming what was called: a bare builtin by its name, a
//! method on a string, array or dict as `method:<name>`, and a std package
//! function as `<pkg>.<name>`. This file turns each into the `Op` that does the
//! work, using the argument types to pick the right one.

use super::lower::{Fx, internal, not_yet};
use super::*;
use crate::compiler::tir::{TExpr, TExprKind, TStmt};
use crate::frontend::error::{JadeError, Span};

type Result<T> = std::result::Result<T, JadeError>;

impl<'a, 'l> Fx<'a, 'l> {
    /// Purpose: This function lowers a call the checker resolved to a builtin.
    ///
    /// Inputs:
    /// - `e`: the call node.
    /// - `et`: its type.
    /// - `name`: the builtin, `method:<name>`, or `<pkg>.<name>`.
    /// - `callee`, `args`, `kwargs`: the call's parts.
    ///
    /// Outputs: the result and its type.
    pub(super) fn builtin(
        &mut self,
        e: &'a TExpr,
        et: &Ty,
        name: &str,
        callee: &'a TExpr,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
    ) -> Result<(Operand, Ty)> {
        let span = e.span;
        if let Some(m) = name.strip_prefix("method:") {
            // A method call, written on the receiver or receiver-first through
            // its package: `s.upper()` or `string.upper(s)`.
            let (recv, rest): (&'a TExpr, &'a [TExpr]) = match &callee.kind {
                TExprKind::FieldAccess { object, .. } if !self.is_package(object) => (object, args),
                _ => match args.split_first() {
                    Some((r, rest)) => (r, rest),
                    None => return Err(internal("a method call with no receiver", span)),
                },
            };
            if !kwargs.is_empty() {
                return Err(internal("keyword arguments to a primitive method", span));
            }
            let (r, rt) = self.expr(recv)?;
            let mut argv = Vec::new();
            for a in rest {
                let (v, t) = self.expr(a)?;
                argv.push((v, t));
            }
            return self.method(m, r, &rt, argv, et, span);
        }
        if let Some((pkg, f)) = name.split_once('.') {
            return self.package_fn(pkg, f, args, et, span);
        }
        if name == "as" {
            // `Animal(x)`: the same object, read as its class or a parent.
            let first = args.first().ok_or_else(|| internal("a class view with no value", span))?;
            let (v, t) = self.expr(first)?;
            return Ok((self.coerce(v, &t, et, span)?, et.clone()));
        }
        if matches!(name, "max_tasks" | "set_max_tasks" | "cancelled" | "join" | "wait") {
            return Err(not_yet("tasks", span));
        }
        let mut argv = Vec::new();
        for a in args {
            let (v, t) = self.expr(a)?;
            argv.push((v, t));
        }
        if let Some((k, v)) = kwargs.first() {
            return Err(internal(format!("keyword argument '{k}' to a builtin"), v.span));
        }
        let nil = || (Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil);
        match name {
            "print" | "write" => {
                let mut it = argv.into_iter();
                let (v, t) = it.next().ok_or_else(|| internal("print with no argument", span))?;
                let s = self.text_of(v, &t);
                let end = match it.next() {
                    Some((v, _)) => v,
                    None => {
                        let text = if name == "print" { "\n" } else { "" };
                        Operand::Const(Const::Str(text.to_string()))
                    }
                };
                self.emit(Stmt::Eval(Rvalue::Op(Op::Print, vec![s, end])));
                Ok(nil())
            }
            "input" => {
                let prompt = match argv.into_iter().next() {
                    Some((v, t)) => self.text_of(v, &t),
                    None => Operand::Const(Const::Str(String::new())),
                };
                Ok((self.assign_temp(Ty::Str, Rvalue::Op(Op::Input, vec![prompt])), Ty::Str))
            }
            "len" => {
                let (v, t) = one(argv, span)?;
                let r = match &t {
                    Ty::Str => self.assign_temp(Ty::Int, Rvalue::Op(Op::StrLen, vec![v])),
                    Ty::Char => Operand::Const(Const::Int(1)),
                    Ty::Array(_) => self.assign_temp(Ty::Int, Rvalue::Op(Op::ArrayLen, vec![v])),
                    Ty::Map(_) => self.assign_temp(Ty::Int, Rvalue::Op(Op::DictLen, vec![v])),
                    other => return Err(not_yet(format!("len() of {other}"), span)),
                };
                Ok((r, Ty::Int))
            }
            "str" => {
                let (v, t) = one(argv, span)?;
                Ok((self.text_of(v, &t), Ty::Str))
            }
            "int" => {
                let (v, t) = one(argv, span)?;
                let r = match &t {
                    Ty::Int => v,
                    Ty::Float | Ty::Bool | Ty::Char | Ty::Str => {
                        self.assign_temp(Ty::Int, Rvalue::Op(Op::ToInt(t.clone()), vec![v]))
                    }
                    other => return Err(not_yet(format!("int() of {other}"), span)),
                };
                Ok((r, Ty::Int))
            }
            "float" => {
                let (v, t) = one(argv, span)?;
                let r = match &t {
                    Ty::Float => v,
                    Ty::Int => self.assign_temp(Ty::Float, Rvalue::IntToFloat(v)),
                    Ty::Str => {
                        self.assign_temp(Ty::Float, Rvalue::Op(Op::ToFloat(Ty::Str), vec![v]))
                    }
                    other => return Err(not_yet(format!("float() of {other}"), span)),
                };
                Ok((r, Ty::Float))
            }
            "bool" => {
                let (v, t) = one(argv, span)?;
                let r = match &t {
                    Ty::Bool => v,
                    Ty::Int | Ty::Str => {
                        self.assign_temp(Ty::Bool, Rvalue::Op(Op::ToBool(t.clone()), vec![v]))
                    }
                    other => return Err(not_yet(format!("bool() of {other}"), span)),
                };
                Ok((r, Ty::Bool))
            }
            "char" => {
                let (v, t) = one(argv, span)?;
                let r = match &t {
                    Ty::Char => v,
                    Ty::Int | Ty::Str => {
                        self.assign_temp(Ty::Char, Rvalue::Op(Op::ToChar(t.clone()), vec![v]))
                    }
                    other => return Err(not_yet(format!("char() of {other}"), span)),
                };
                Ok((r, Ty::Char))
            }
            other => {
                let _ = et;
                Err(internal(format!("an unknown builtin '{other}'"), span))
            }
        }
    }

    /// Purpose: This function says whether an expression names a std package
    /// rather than a value, the way the checker decides it.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: true for a bare package name no variable shadows.
    fn is_package(&self, e: &TExpr) -> bool {
        match &e.kind {
            TExprKind::Identifier(n) => {
                self.lookup(n).is_none() && (self.l.packages.contains(n) || n == "Grammar")
            }
            _ => false,
        }
    }

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
    fn package_fn(
        &mut self,
        pkg: &str,
        f: &str,
        args: &'a [TExpr],
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
            _ => Err(not_yet(format!("std/{pkg}"), span)),
        }
    }

    /// Purpose: This function lowers a method on a string, array or dict.
    ///
    /// Inputs:
    /// - `m`: the method name.
    /// - `r`, `rt`: the receiver and its type.
    /// - `argv`: the other arguments with their types.
    /// - `et`: the call's type.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    fn method(
        &mut self,
        m: &str,
        r: Operand,
        rt: &Ty,
        argv: Vec<(Operand, Ty)>,
        et: &Ty,
        span: Span,
    ) -> Result<(Operand, Ty)> {
        let nil = (Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil);
        let mut ops = vec![r];
        match rt {
            Ty::Str => {
                if m == "len" {
                    return Ok((self.assign_temp(Ty::Int, Rvalue::Op(Op::StrLen, ops)), Ty::Int));
                }
                let sm = match m {
                    "upper" => StrMethod::Upper,
                    "lower" => StrMethod::Lower,
                    "capitalize" => StrMethod::Capitalize,
                    "trim" => StrMethod::Trim,
                    "trim_start" => StrMethod::TrimStart,
                    "trim_end" => StrMethod::TrimEnd,
                    "split" => StrMethod::Split,
                    "lines" => StrMethod::Lines,
                    "contains" => StrMethod::Contains,
                    "starts_with" => StrMethod::StartsWith,
                    "ends_with" => StrMethod::EndsWith,
                    "index_of" => StrMethod::IndexOf,
                    "last_index_of" => StrMethod::LastIndexOf,
                    "count" => StrMethod::Count,
                    "replace" => StrMethod::Replace,
                    "repeat" => StrMethod::Repeat,
                    "slice" => StrMethod::Slice,
                    "pad_start" => StrMethod::PadStart,
                    "pad_end" => StrMethod::PadEnd,
                    "is_empty" => StrMethod::IsEmpty,
                    "encode" => return Err(not_yet("bytes", span)),
                    other => {
                        return Err(internal(format!("an unknown string method '{other}'"), span));
                    }
                };
                for (v, t) in argv {
                    ops.push(self.char_as_str(v, &t));
                }
                Ok((self.assign_temp(et.clone(), Rvalue::Op(Op::Str(sm), ops)), et.clone()))
            }
            Ty::Array(el) => {
                let el = (**el).clone();
                let op = match m {
                    "len" => Op::ArrayLen,
                    "pop" => Op::ArrayPop(el.clone()),
                    "sort" => Op::ArraySort(el.clone()),
                    "reverse" => Op::ArrayReverse(el.clone()),
                    "join" => Op::ArrayJoin(el.clone()),
                    "push" | "contains" => {
                        let (v, t) = argv
                            .into_iter()
                            .next()
                            .ok_or_else(|| internal("a missing argument", span))?;
                        let v = self.coerce(v, &t, &el, span)?;
                        if m == "push" {
                            let v = self.consume(v);
                            ops.push(v);
                            self.emit(Stmt::Eval(Rvalue::Op(Op::ArrayPush(el), ops)));
                            return Ok(nil);
                        }
                        ops.push(v);
                        let op = Op::ArrayContains(el);
                        return Ok((self.assign_temp(Ty::Bool, Rvalue::Op(op, ops)), Ty::Bool));
                    }
                    "map" | "filter" => {
                        return Err(not_yet("map and filter, which take a function", span));
                    }
                    other => {
                        return Err(internal(format!("an unknown array method '{other}'"), span));
                    }
                };
                for (v, _) in argv {
                    ops.push(v);
                }
                if matches!(op, Op::ArraySort(_) | Op::ArrayReverse(_)) {
                    self.emit(Stmt::Eval(Rvalue::Op(op, ops)));
                    return Ok(nil);
                }
                Ok((self.assign_temp(et.clone(), Rvalue::Op(op, ops)), et.clone()))
            }
            Ty::Map(vt) => {
                let vt = (**vt).clone();
                let op = match m {
                    "len" => Op::DictLen,
                    "keys" => Op::DictKeys(vt),
                    "values" => Op::DictValues(vt),
                    "has" => Op::DictHas(vt),
                    "get" => Op::DictGetOpt(vt),
                    "merge" => Op::DictMerge(vt),
                    other => {
                        return Err(internal(format!("an unknown dict method '{other}'"), span));
                    }
                };
                for (v, _) in argv {
                    ops.push(v);
                }
                Ok((self.assign_temp(et.clone(), Rvalue::Op(op, ops)), et.clone()))
            }
            Ty::Bytes => Err(not_yet("bytes", span)),
            Ty::Future(_) => Err(not_yet("tasks", span)),
            Ty::Json => Err(not_yet("json values", span)),
            other => Err(internal(format!("a method '{m}' on {other}"), span)),
        }
    }
}

/// Purpose: This function takes a builtin's single argument.
///
/// Inputs:
/// - `argv`: the arguments.
/// - `span`: where.
///
/// Outputs: the one argument.
fn one(argv: Vec<(Operand, Ty)>, span: Span) -> Result<(Operand, Ty)> {
    argv.into_iter().next().ok_or_else(|| internal("a builtin with no argument", span))
}

/// Purpose: This function says whether a function body contains a `yield`,
/// outside any closure.
///
/// Inputs:
/// - `body`: the statements.
///
/// Outputs: true when the function is a generator.
pub(super) fn body_yields(body: &[TStmt]) -> bool {
    body.iter().any(|s| match s {
        TStmt::Yield { .. } => true,
        TStmt::If { then_body, else_body, .. } => {
            body_yields(then_body) || else_body.as_deref().is_some_and(body_yields)
        }
        TStmt::While { body, .. } | TStmt::For { body, .. } => body_yields(body),
        TStmt::TryCatch { body, arms, .. } => {
            body_yields(body) || arms.iter().any(|a| body_yields(&a.body))
        }
        _ => false,
    })
}
