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
            return self.method(m, r, &rt, argv, et, span, addr_of(e));
        }
        if let Some((pkg, f)) = name.split_once('.') {
            return self.package_fn(pkg, f, args, kwargs, et, span);
        }
        if name == "as" {
            // `Animal(x)`: the same object, read as its class or a parent.
            let first = args.first().ok_or_else(|| internal("a class view with no value", span))?;
            let (v, t) = self.expr(first)?;
            return Ok((self.coerce(v, &t, et, span)?, et.clone()));
        }
        if name == "join" {
            let Ty::Array(el) = et else {
                return Err(internal("join that does not make an array", span));
            };
            let settle = kwargs
                .iter()
                .any(|(k, v)| k == "settle" && matches!(v.kind, TExprKind::Bool(true)));
            let mut fs = Vec::new();
            let mut inner = Ty::Nil;
            for a in args {
                let (v, t) = self.expr(a)?;
                if let Ty::Future(i) = &t {
                    inner = (**i).clone();
                }
                fs.push(v);
            }
            let op = match (settle, &**el) {
                (true, Ty::Struct(s)) => Op::JoinSettled(inner, s.clone()),
                _ => Op::Join((**el).clone()),
            };
            return Ok((self.assign_temp(et.clone(), Rvalue::Op(op, fs)), et.clone()));
        }
        if name == "wait" {
            let first = args.first().ok_or_else(|| internal("wait with no futures", span))?;
            // A literal list may hold futures of different result types, since
            // `wait` never reads a result; each is passed as it is.
            if let TExprKind::Array { elements } = &first.kind {
                let mut fs = Vec::new();
                for x in elements {
                    let (v, _) = self.expr(x)?;
                    fs.push(v);
                }
                return Ok((self.assign_temp(Ty::Int, Rvalue::Op(Op::Wait, fs)), Ty::Int));
            }
            let (v, t) = self.expr(first)?;
            let Ty::Array(f) = &t else {
                return Err(internal("wait over something that is not an array", span));
            };
            let Ty::Future(inner) = &**f else {
                return Err(internal("wait over an array of non-futures", span));
            };
            return Ok((
                self.assign_temp(Ty::Int, Rvalue::Op(Op::WaitArray((**inner).clone()), vec![v])),
                Ty::Int,
            ));
        }
        let mut argv = Vec::new();
        for a in args {
            let (v, t) = self.expr(a)?;
            argv.push((v, t));
        }
        if let Some((k, v)) = kwargs.first() {
            return Err(internal(format!("keyword argument '{k}' to a builtin"), v.span));
        }
        self.builtin_apply(name, argv, et, span)
    }

    /// Purpose: This function applies a bare builtin to arguments already
    /// lowered, for a direct call and for a builtin passed as a value.
    ///
    /// Inputs:
    /// - `name`: the builtin.
    /// - `argv`: the arguments with their types.
    /// - `et`: the call's type.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    pub(super) fn builtin_apply(
        &mut self,
        name: &str,
        argv: Vec<(Operand, Ty)>,
        et: &Ty,
        span: Span,
    ) -> Result<(Operand, Ty)> {
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
                    Ty::Bytes => self.assign_temp(Ty::Int, Rvalue::Op(Op::BytesLen, vec![v])),
                    Ty::Json => {
                        self.assign_temp(Ty::Int, Rvalue::Op(Op::Json(JsonFn::Len), vec![v]))
                    }
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
                    Ty::Json => {
                        self.assign_temp(Ty::Int, Rvalue::Op(Op::Json(JsonFn::ToInt), vec![v]))
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
                    Ty::Json => {
                        self.assign_temp(Ty::Float, Rvalue::Op(Op::Json(JsonFn::ToFloat), vec![v]))
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
                    Ty::Json => {
                        self.assign_temp(Ty::Bool, Rvalue::Op(Op::Json(JsonFn::ToBool), vec![v]))
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
            "max_tasks" => {
                Ok((self.assign_temp(Ty::Int, Rvalue::Op(Op::MaxTasks, Vec::new())), Ty::Int))
            }
            "cancelled" => {
                Ok((self.assign_temp(Ty::Bool, Rvalue::Op(Op::Cancelled, Vec::new())), Ty::Bool))
            }
            "set_max_tasks" => {
                let (v, _) = one(argv, span)?;
                Ok((self.assign_temp(Ty::Int, Rvalue::Op(Op::SetMaxTasks, vec![v])), Ty::Int))
            }
            "max_tokens" => Ok((
                self.assign_temp(Ty::Int, Rvalue::Op(Op::Infer(InferFn::MaxTokens), Vec::new())),
                Ty::Int,
            )),
            "set_max_tokens" => {
                let (v, _) = one(argv, span)?;
                let op = Op::Infer(InferFn::SetMaxTokens);
                Ok((self.assign_temp(Ty::Int, Rvalue::Op(op, vec![v])), Ty::Int))
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
    #[allow(clippy::too_many_arguments)]
    fn method(
        &mut self,
        m: &str,
        r: Operand,
        rt: &Ty,
        argv: Vec<(Operand, Ty)>,
        et: &Ty,
        span: Span,
        node: usize,
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
                    "encode" => {
                        return Ok((
                            self.assign_temp(Ty::Bytes, Rvalue::Op(Op::StrEncode, ops)),
                            Ty::Bytes,
                        ));
                    }
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
                        let r = ops.pop().ok_or_else(|| internal("map with no array", span))?;
                        return self.map_filter(m == "map", r, &el, argv, et, span, node);
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
            Ty::Bytes => {
                for (v, _) in argv {
                    ops.push(v);
                }
                let (op, t) = match m {
                    "decode" => (Op::BytesDecode, Ty::Str),
                    "slice" => (Op::BytesSlice, Ty::Bytes),
                    "len" => (Op::BytesLen, Ty::Int),
                    other => {
                        return Err(internal(format!("an unknown bytes method '{other}'"), span));
                    }
                };
                Ok((self.assign_temp(t.clone(), Rvalue::Op(op, ops)), t))
            }
            Ty::Future(_) => match m {
                "ready" => Ok((self.assign_temp(Ty::Bool, Rvalue::Op(Op::Ready, ops)), Ty::Bool)),
                "cancel" => {
                    self.emit(Stmt::Eval(Rvalue::Op(Op::Cancel, ops)));
                    Ok(nil)
                }
                other => Err(internal(format!("an unknown future method '{other}'"), span)),
            },
            Ty::Json => match m {
                "len" => {
                    Ok((self.assign_temp(Ty::Int, Rvalue::Op(Op::Json(JsonFn::Len), ops)), Ty::Int))
                }
                other => Err(internal(format!("an unknown json method '{other}'"), span)),
            },
            other => Err(internal(format!("a method '{m}' on {other}"), span)),
        }
    }
}

impl<'a, 'l> Fx<'a, 'l> {
    /// Purpose: This function lowers `xs.map(f, extra…)` and
    /// `xs.filter(f, extra…)` as a loop over the array.
    ///
    /// Inputs:
    /// - `is_map`: true for map, false for filter.
    /// - `arr`: the array.
    /// - `el`: its element type.
    /// - `argv`: the function value, then the extra arguments.
    /// - `et`: the result's type.
    /// - `span`: where.
    /// - `node`: the call's address; the checker recorded the function's call
    ///   at the next address.
    ///
    /// Outputs: the new array.
    #[allow(clippy::too_many_arguments)]
    fn map_filter(
        &mut self,
        is_map: bool,
        arr: Operand,
        el: &Ty,
        argv: Vec<(Operand, Ty)>,
        et: &Ty,
        span: Span,
        node: usize,
    ) -> Result<(Operand, Ty)> {
        let mut it = argv.into_iter();
        let (fv, ft) = it.next().ok_or_else(|| internal("map with no function", span))?;
        let extras: Vec<(Operand, Ty)> = it.collect();
        let Ty::Array(out_el) = et else {
            return Err(internal("map that does not make an array", span));
        };
        let out_el = (**out_el).clone();
        let out = self.assign_temp(et.clone(), Rvalue::Array(out_el.clone(), Vec::new()));
        let n = self.assign_temp(Ty::Int, Rvalue::Op(Op::ArrayLen, vec![arr.clone()]));
        let i = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(Place::Local(i), Rvalue::Use(Operand::Const(Const::Int(0)))));
        let head = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.terminate(Term::Goto(head));
        self.switch_to(head);
        let more = self.assign_temp(
            Ty::Bool,
            Rvalue::Bin(BinOp::Cmp(CmpOp::Lt, Ty::Int), Operand::Copy(Place::Local(i)), n),
        );
        self.terminate(Term::If(more, body, exit));
        self.switch_to(body);
        let x = self.assign_temp(
            el.clone(),
            Rvalue::Op(Op::ArrayGet(el.clone()), vec![arr, Operand::Copy(Place::Local(i))]),
        );
        let mut call = vec![(x.clone(), el.clone())];
        call.extend(extras);
        let (r, rt) = self.call_function_value(fv, &ft, call, node + 1, span)?;
        if is_map {
            let r = self.coerce(r, &rt, &out_el, span)?;
            self.emit(Stmt::Eval(Rvalue::Op(Op::ArrayPush(out_el.clone()), vec![out.clone(), r])));
        } else {
            let keep = self.new_block();
            let skip = self.new_block();
            self.terminate(Term::If(r, keep, skip));
            self.switch_to(keep);
            self.emit(Stmt::Eval(Rvalue::Op(Op::ArrayPush(el.clone()), vec![out.clone(), x])));
            self.terminate(Term::Goto(skip));
            self.switch_to(skip);
        }
        self.emit(Stmt::Assign(
            Place::Local(i),
            Rvalue::Bin(
                BinOp::Arith(ArithOp::Add, NumTy::Int),
                Operand::Copy(Place::Local(i)),
                Operand::Const(Const::Int(1)),
            ),
        ));
        self.terminate(Term::Goto(head));
        self.switch_to(exit);
        Ok((out, et.clone()))
    }

    /// Purpose: This function calls a function value with arguments already
    /// lowered: a named function or builtin known when compiling, or a pointer
    /// of a joined function type.
    ///
    /// Inputs:
    /// - `fv`, `ft`: the function value and its type.
    /// - `args`: the arguments with their types.
    /// - `call_node`: the address the checker recorded the call's target under.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    pub(super) fn call_function_value(
        &mut self,
        fv: Operand,
        ft: &Ty,
        args: Vec<(Operand, Ty)>,
        call_node: usize,
        span: Span,
    ) -> Result<(Operand, Ty)> {
        match ft {
            Ty::Fn(f) => {
                let mut argv = Vec::new();
                for ((v, t), pt) in args.into_iter().zip(&f.params) {
                    argv.push(self.coerce(v, &t, &super::lower::norm(pt), span)?);
                }
                let rt = super::lower::norm(&f.ret);
                Ok((self.assign_temp(rt.clone(), Rvalue::CallIndirect(fv, argv)), rt))
            }
            Ty::Func(crate::compiler::infer::FuncRef::Builtin(name)) => {
                let rt = builtin_result(name);
                self.builtin_apply(name, args, &rt, span)
            }
            Ty::Func(_) => match self.targets.get(&call_node).cloned() {
                Some(crate::compiler::infer::Target::Instance(k)) => {
                    let def = self
                        .l
                        .def_of(&k.func)
                        .ok_or_else(|| internal("a function value with no body", span))?;
                    let mut vals: Vec<Option<(Operand, Ty, Span)>> = vec![None; def.params.len()];
                    for (i, (v, t)) in args.into_iter().enumerate() {
                        if i < vals.len() {
                            vals[i] = Some((v, t, span));
                        }
                    }
                    let (_, _, ret) = self.l.sig(&k, span)?;
                    let r = self.call_instance(&k, def, vals, span)?;
                    Ok((r, ret))
                }
                _ => Err(internal("a function value whose call was not resolved", span)),
            },
            other => Err(internal(format!("calling a {other}"), span)),
        }
    }
}

/// Purpose: This function gives a builtin's result type.
///
/// Inputs:
/// - `name`: the builtin.
///
/// Outputs: the type it returns.
fn builtin_result(name: &str) -> Ty {
    match name {
        "len" | "int" | "max_tasks" | "set_max_tasks" | "max_tokens" | "set_max_tokens" => Ty::Int,
        "str" | "input" => Ty::Str,
        "float" => Ty::Float,
        "bool" | "cancelled" => Ty::Bool,
        "char" => Ty::Char,
        _ => Ty::Nil,
    }
}

/// Purpose: This function gives a syntax node its identity, as the checker does.
fn addr_of<T>(x: &T) -> usize {
    x as *const T as usize
}

/// Purpose: This function takes a builtin's single argument.
///
/// Inputs:
/// - `argv`: the arguments.
/// - `span`: where.
///
/// Outputs: the one argument.
pub(super) fn one(argv: Vec<(Operand, Ty)>, span: Span) -> Result<(Operand, Ty)> {
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
