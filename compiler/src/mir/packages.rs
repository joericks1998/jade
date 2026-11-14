//! Lowering calls to std package functions into MIR operations.
//!
//! The checker resolves `time.sleep(1)` or `fs.read(p)` to a `Target::Builtin`
//! named `<pkg>.<name>`, and `ops.rs` hands each such call here. Each package
//! has its own `Op`, or an `Op` carrying an enum of the package's functions,
//! such as `Op::Math(MathFn::Sqrt)` or `Op::Fs(FsFn::Read)`. The argument types
//! the checker settled pick the variant, so the backend never looks at a kind
//! at run time.

use super::lower::{Fx, internal, not_yet};
use super::ops::one;
use super::*;
use crate::frontend::error::{JadeError, Span};
use crate::typechecker::tir::TExpr;

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
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
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
            ("time", "now" | "now_ms" | "monotonic") => {
                let (op, t) = match f {
                    "now" => (Op::Now, Ty::Int),
                    "now_ms" => (Op::NowMs, Ty::Int),
                    _ => (Op::Monotonic, Ty::Float),
                };
                Ok((self.assign_temp(t.clone(), Rvalue::Op(op, Vec::new())), t))
            }
            ("time", "sleep" | "after") => {
                let first =
                    args.first().ok_or_else(|| internal("time.sleep with no delay", span))?;
                let secs = self.float_operand(first)?;
                if f == "sleep" {
                    self.emit(Stmt::Eval(Rvalue::Op(Op::Sleep, vec![secs])));
                    return Ok((Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil));
                }
                Ok((self.assign_temp(et.clone(), Rvalue::Op(Op::After, vec![secs])), et.clone()))
            }
            ("time", "local") => {
                let first =
                    args.first().ok_or_else(|| internal("time.local with no zone", span))?;
                let (v, t) = self.expr(first)?;
                let tz = self.coerce(v, &t, &Ty::opt(Ty::Str), span)?;
                self.package_op(Op::TimeLocal, vec![tz], Ty::Str)
            }
            ("time", "utc" | "parts" | "stamp") => {
                let mut ops = Vec::new();
                for a in args {
                    ops.push(self.expr(a)?.0);
                }
                let op = match f {
                    "utc" => Op::TimeUtc,
                    "parts" => Op::TimeParts,
                    _ => {
                        // The hour, minute and second default to 0.
                        ops.resize(6, Operand::Const(Const::Int(0)));
                        Op::TimeStamp
                    }
                };
                Ok((self.assign_temp(et.clone(), Rvalue::Op(op, ops)), et.clone()))
            }
            ("math", _) => self.math(f, args, et, span),
            ("fs", _) => self.fs(f, args, kwargs, span),
            ("env", _) => {
                let (m, t) = match f {
                    "get" => (EnvFn::Get, Ty::opt(Ty::Str)),
                    "set" => (EnvFn::Set, Ty::Nil),
                    "args" => (EnvFn::Args, Ty::Array(Box::new(Ty::Str))),
                    "cwd" => (EnvFn::Cwd, Ty::Str),
                    other => {
                        return Err(internal(format!("an unknown env function '{other}'"), span));
                    }
                };
                let ops = self.operands(args)?;
                self.package_op(Op::Env(m), ops, t)
            }
            ("path", "join") => {
                // A longer join is a chain of two-segment ones, which is what
                // pushing each segment in turn computes.
                let mut ops = self.operands(args)?.into_iter();
                let mut acc =
                    ops.next().ok_or_else(|| internal("path.join with no segments", span))?;
                for next in ops {
                    acc = self
                        .assign_temp(Ty::Str, Rvalue::Op(Op::Path(PathFn::Join), vec![acc, next]));
                }
                Ok((acc, Ty::Str))
            }
            ("path", _) => {
                let (m, t) = match f {
                    "basename" => (PathFn::Basename, Ty::Str),
                    "dirname" => (PathFn::Dirname, Ty::Str),
                    "stem" => (PathFn::Stem, Ty::Str),
                    "ext" => (PathFn::Ext, Ty::opt(Ty::Str)),
                    "abs" => (PathFn::Abs, Ty::Str),
                    "is_abs" => (PathFn::IsAbs, Ty::Bool),
                    other => {
                        return Err(internal(format!("an unknown path function '{other}'"), span));
                    }
                };
                let ops = self.operands(args)?;
                self.package_op(Op::Path(m), ops, t)
            }
            ("random", "choice") => {
                let first =
                    args.first().ok_or_else(|| internal("random.choice with no array", span))?;
                let (a, at) = self.expr(first)?;
                let Ty::Array(el) = &at else {
                    return Err(internal("random.choice of a non-array", span));
                };
                let n = self.assign_temp(Ty::Int, Rvalue::Op(Op::ArrayLen, vec![a.clone()]));
                let i = self.assign_temp(Ty::Int, Rvalue::Op(Op::Random(RandomFn::Index), vec![n]));
                Ok((
                    self.assign_temp(
                        (**el).clone(),
                        Rvalue::Op(Op::ArrayGet((**el).clone()), vec![a, i]),
                    ),
                    (**el).clone(),
                ))
            }
            ("random", _) => {
                let (m, t) = match f {
                    "int" => (RandomFn::Int, Ty::Int),
                    "float" => (RandomFn::Float, Ty::Float),
                    "shuffle" => (RandomFn::Shuffle, Ty::Nil),
                    "seed" => (RandomFn::Seed, Ty::Nil),
                    other => {
                        return Err(internal(
                            format!("an unknown random function '{other}'"),
                            span,
                        ));
                    }
                };
                let ops = self.operands(args)?;
                self.package_op(Op::Random(m), ops, t)
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
                    "generate" => (InferFn::Generate, Ty::Array(Box::new(Ty::Int))),
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
            ("json", "parse") => {
                let ops = self.operands(args)?;
                self.package_op(Op::Json(JsonFn::Parse), ops, Ty::Json)
            }
            ("json", "stringify" | "stringify_pretty") => {
                let m = if f == "stringify" { JsonFn::Stringify } else { JsonFn::StringifyPretty };
                let ops = self.operands(args)?;
                self.package_op(Op::Json(m), ops, Ty::Str)
            }
            ("http" | "uhttp", "stream") => self.uhttp_stream(args, span),
            ("http" | "uhttp", _) => {
                let m = match f {
                    "get" => HttpFn::Get,
                    "delete" => HttpFn::Delete,
                    "head" => HttpFn::Head,
                    "post" => HttpFn::Post,
                    "put" => HttpFn::Put,
                    "get_bytes" => HttpFn::GetBytes,
                    "post_bytes" => HttpFn::PostBytes,
                    other => {
                        return Err(internal(format!("an unknown http function '{other}'"), span));
                    }
                };
                let mut ops = self.operands(args)?;
                let want = if m.has_body() { 3 } else { 2 };
                // No headers is an empty set of them: a null dict.
                if ops.len() < want {
                    ops.push(Operand::Const(Const::Nil(Ty::Map(Box::new(Ty::Str)))));
                }
                self.package_op(Op::Http(m, pkg == "uhttp"), ops, m.result())
            }
            ("sh", "exec" | "run" | "output") => {
                let m = match f {
                    "exec" => ShFn::Exec,
                    "run" => ShFn::Run,
                    _ => ShFn::Output,
                };
                let first = args.first().ok_or_else(|| internal("sh with no command", span))?;
                let (cmd, _) = self.expr(first)?;
                self.package_op(Op::Sh(m), vec![cmd], m.result())
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

    /// Purpose: This function lowers a `std::math` call, picking the operation
    /// from the argument kinds the checker settled.
    ///
    /// Inputs:
    /// - `f`: the function's name.
    /// - `args`: its arguments.
    /// - `et`: the call's type.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    fn math(&mut self, f: &str, args: &'a [TExpr], et: &Ty, span: Span) -> Result<(Operand, Ty)> {
        let float = |v: f64| Ok((Operand::Const(Const::Float(v)), Ty::Float));
        match f {
            "pi" => return float(std::f64::consts::PI),
            "e" => return float(std::f64::consts::E),
            "tau" => return float(std::f64::consts::TAU),
            "inf" => return float(f64::INFINITY),
            "nan" => return float(f64::NAN),
            _ => {}
        }
        let mut argv = Vec::new();
        for a in args {
            argv.push(self.expr(a)?);
        }
        let all_int = argv.iter().all(|(_, t)| *t == Ty::Int);
        let call = |fx: &mut Self, m: MathFn, ops: Vec<Operand>, t: Ty| {
            Ok((fx.assign_temp(t.clone(), Rvalue::Op(Op::Math(m), ops)), t))
        };
        let floats = |fx: &mut Self, argv: Vec<(Operand, Ty)>| -> Vec<Operand> {
            argv.into_iter().map(|(v, t)| fx.as_float(v, &t)).collect()
        };
        match f {
            "floor" | "ceil" | "round" | "trunc" => {
                let (v, t) = one(argv, span)?;
                if t == Ty::Int {
                    return Ok((v, Ty::Int));
                }
                let m = match f {
                    "floor" => MathFn::Floor,
                    "ceil" => MathFn::Ceil,
                    "round" => MathFn::Round,
                    _ => MathFn::Trunc,
                };
                call(self, m, vec![v], Ty::Int)
            }
            "abs" | "sign" => {
                let (v, t) = one(argv, span)?;
                let m = match (f, t == Ty::Int) {
                    ("abs", true) => MathFn::AbsInt,
                    ("abs", false) => MathFn::AbsFloat,
                    (_, true) => MathFn::SignInt,
                    _ => MathFn::SignFloat,
                };
                call(self, m, vec![v], t)
            }
            "is_nan" | "is_inf" => {
                let (v, t) = one(argv, span)?;
                if t == Ty::Int {
                    return Ok((Operand::Const(Const::Bool(false)), Ty::Bool));
                }
                let m = if f == "is_nan" { MathFn::IsNan } else { MathFn::IsInf };
                call(self, m, vec![v], Ty::Bool)
            }
            "min" | "max" => {
                let m = match (f, all_int) {
                    ("min", true) => MathFn::MinInt,
                    ("max", true) => MathFn::MaxInt,
                    ("min", false) => MathFn::MinFloat,
                    _ => MathFn::MaxFloat,
                };
                let ops = if all_int {
                    argv.into_iter().map(|(v, _)| v).collect()
                } else {
                    floats(self, argv)
                };
                call(self, m, ops, et.clone())
            }
            "pow" => {
                if all_int {
                    let ops = argv.into_iter().map(|(v, _)| v).collect();
                    return call(self, MathFn::PowInt, ops, Ty::Int);
                }
                let exp_int = argv.get(1).is_some_and(|(_, t)| *t == Ty::Int);
                if exp_int {
                    let mut it = argv.into_iter();
                    let (b, bt) = it.next().ok_or_else(|| internal("pow with no base", span))?;
                    let (e, _) = it.next().ok_or_else(|| internal("pow with no exponent", span))?;
                    let b = self.as_float(b, &bt);
                    return call(self, MathFn::PowI, vec![b, e], Ty::Float);
                }
                let ops = floats(self, argv);
                call(self, MathFn::Pow, ops, Ty::Float)
            }
            "clamp" => {
                // `max(x, lo)`, then `min(that, hi)`, as v1 computed it.
                let (lo_m, hi_m) = if all_int {
                    (MathFn::MaxInt, MathFn::MinInt)
                } else {
                    (MathFn::MaxFloat, MathFn::MinFloat)
                };
                let ops = if all_int {
                    argv.into_iter().map(|(v, _)| v).collect()
                } else {
                    floats(self, argv)
                };
                let [x, lo, hi]: [Operand; 3] =
                    ops.try_into().map_err(|_| internal("clamp without three numbers", span))?;
                let (low, _) = call(self, lo_m, vec![x, lo], et.clone())?;
                call(self, hi_m, vec![low, hi], et.clone())
            }
            "atan2" | "hypot" => {
                let m = if f == "atan2" { MathFn::Atan2 } else { MathFn::Hypot };
                let ops = floats(self, argv);
                call(self, m, ops, Ty::Float)
            }
            _ => {
                let m = match f {
                    "sqrt" => MathFn::Sqrt,
                    "ln" => MathFn::Ln,
                    "log2" => MathFn::Log2,
                    "log10" => MathFn::Log10,
                    "exp" => MathFn::Exp,
                    "sin" => MathFn::Sin,
                    "cos" => MathFn::Cos,
                    "tan" => MathFn::Tan,
                    "asin" => MathFn::Asin,
                    "acos" => MathFn::Acos,
                    "atan" => MathFn::Atan,
                    other => {
                        return Err(internal(format!("an unknown math function '{other}'"), span));
                    }
                };
                let ops = floats(self, argv);
                call(self, m, ops, Ty::Float)
            }
        }
    }

    /// Purpose: This function lowers a number argument as a float.
    fn float_operand(&mut self, e: &'a TExpr) -> Result<Operand> {
        let (v, t) = self.expr(e)?;
        Ok(self.as_float(v, &t))
    }

    /// Purpose: This function lowers a `std::fs` call.
    ///
    /// Inputs:
    /// - `f`: the function's name.
    /// - `args`, `kwargs`: its arguments; only `read` and `read_bytes` take
    ///   one, `trust`.
    /// - `span`: where.
    ///
    /// Outputs: the result and its type.
    fn fs(
        &mut self,
        f: &str,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
        span: Span,
    ) -> Result<(Operand, Ty)> {
        let m = match f {
            "read" => FsFn::Read,
            "read_bytes" => FsFn::ReadBytes,
            "write" => FsFn::Write,
            "append" => FsFn::Append,
            "write_bytes" => FsFn::WriteBytes,
            "append_bytes" => FsFn::AppendBytes,
            "read_stdin_bytes" => FsFn::ReadStdinBytes,
            "write_stdout_bytes" => FsFn::WriteStdoutBytes,
            "exists" => FsFn::Exists,
            "is_file" => FsFn::IsFile,
            "is_dir" => FsFn::IsDir,
            "delete" => FsFn::Delete,
            "mkdir" => FsFn::Mkdir,
            "rmdir" => FsFn::Rmdir,
            "list_dir" => FsFn::ListDir,
            "size" => FsFn::Size,
            "copy" => FsFn::Copy,
            "rename" => FsFn::Rename,
            other => return Err(internal(format!("an unknown fs function '{other}'"), span)),
        };
        let mut ops = Vec::new();
        for a in args {
            ops.push(self.expr(a)?.0);
        }
        if matches!(m, FsFn::Read | FsFn::ReadBytes) {
            let trust = match kwargs.iter().find(|(k, _)| k == "trust") {
                Some((_, v)) => self.expr(v)?.0,
                None => Operand::Const(Const::Bool(false)),
            };
            ops.push(trust);
        }
        self.package_op(Op::Fs(m), ops, m.result())
    }

    /// Purpose: This function lowers `uhttp.stream(url, handler, headers?)`,
    /// giving the handler to the backend as a function pointer.
    fn uhttp_stream(&mut self, args: &'a [TExpr], span: Span) -> Result<(Operand, Ty)> {
        let [url, handler, rest @ ..] = args else {
            return Err(internal("uhttp.stream without a url and a handler", span));
        };
        let (u, _) = self.expr(url)?;
        let (h, ht) = self.expr(handler)?;
        let ft = match &ht {
            Ty::Fn(ft) => (**ft).clone(),
            Ty::Func(crate::typechecker::infer::FuncRef::Named(n)) => {
                let key = crate::typechecker::infer::InstKey {
                    func: crate::typechecker::infer::FnKey::Named(n.clone()),
                    args: vec![Ty::Str],
                };
                let (_, _, ret) = self.l.sig(&key, span)?;
                crate::typechecker::infer::FnTy { params: vec![Ty::Str], ret, is_async: false }
            }
            _ => return Err(not_yet("a builtin as a stream handler", span)),
        };
        let ret = super::lower::norm(&ft.ret);
        let fp = self.coerce(h, &ht, &Ty::Fn(Box::new(ft)), span)?;
        let headers = match rest.first() {
            Some(e) => self.expr(e)?.0,
            None => Operand::Const(Const::Nil(Ty::Map(Box::new(Ty::Str)))),
        };
        self.package_op(Op::UhttpStream(ret), vec![u, fp, headers], Ty::Nil)
    }

    /// Purpose: This function lowers each argument in order.
    fn operands(&mut self, args: &'a [TExpr]) -> Result<Vec<Operand>> {
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
