//! The types of everything a program calls without writing it: the bare
//! globals, every std package function, and the primitive methods on strings,
//! arrays, dicts, bytes and futures.
//!
//! Std results that v1 returned as dicts with mixed values are built-in structs
//! here, with the same field names, per design decision 3: `http.get` returns an
//! `HttpResponse`, `sh.output` a `ShOutput`, and `time.parts` a `TimeParts`. A
//! struct reads by `r.status` and also by `r["status"]`, so a program that read
//! one of these dicts reads the struct the same way.

use super::check::Frame;
use super::ty::{FuncRef, Ty};
use super::{Checker, StructInfo, Target};
use crate::compiler::tir::TExpr;
use crate::frontend::error::Span;

/// The structs every program has without declaring them, with their field types.
fn builtin_structs() -> Vec<(&'static str, Vec<(&'static str, Ty)>)> {
    let int = || Ty::Int;
    vec![
        ("RuntimeError", vec![("message", Ty::Str)]),
        ("HttpResponse", vec![("status", Ty::Int), ("body", Ty::Str)]),
        ("HttpBytesResponse", vec![("status", Ty::Int), ("body", Ty::Bytes)]),
        ("ShOutput", vec![("stdout", Ty::Str), ("stderr", Ty::Str), ("code", Ty::Int)]),
        (
            "TimeParts",
            vec![
                ("year", int()),
                ("month", int()),
                ("day", int()),
                ("hour", int()),
                ("minute", int()),
                ("second", int()),
                ("weekday", int()),
                ("yearday", int()),
            ],
        ),
    ]
}

/// Purpose: This function registers the built-in structs before the program's own.
///
/// Inputs:
/// - `c`: the checker.
///
/// Outputs: none.
pub(super) fn register_builtin_structs(c: &mut Checker) {
    for (name, fields) in builtin_structs() {
        c.structs.insert(
            name.to_string(),
            StructInfo {
                fields: fields.iter().map(|(f, _)| f.to_string()).collect(),
                prompt_fields: Default::default(),
                parents: vec![],
                public: None,
                declared: Vec::new(),
            },
        );
        for (f, t) in fields {
            let slot = c.store.slot_of(t);
            c.field_slots.insert((name.to_string(), f.to_string()), slot);
        }
        c.constructed.insert(name.to_string());
    }
}

/// Purpose: This function returns the struct `join(…, settle = true)` gives
/// each result, for one result type.
///
/// Inputs:
/// - `c`: the checker.
/// - `inner`: the futures' result type, a slot.
///
/// Outputs: the struct's name, made the first time.
pub(super) fn settled_struct(c: &mut Checker, inner: &Ty) -> String {
    let id = match inner {
        Ty::Var(id) => *id,
        _ => 0,
    };
    let name = format!("Settled$s{id}");
    if !c.structs.contains_key(&name) {
        c.structs.insert(
            name.clone(),
            StructInfo {
                fields: vec!["ok".into(), "value".into(), "error".into()],
                prompt_fields: Default::default(),
                parents: vec![],
                public: None,
                declared: Vec::new(),
            },
        );
        let ok = c.store.slot_of(Ty::Bool);
        let value = c.store.slot_of(Ty::opt(inner.clone()));
        let error = c.store.slot_of(Ty::opt(Ty::Error));
        c.field_slots.insert((name.clone(), "ok".into()), ok);
        c.field_slots.insert((name.clone(), "value".into()), value);
        c.field_slots.insert((name.clone(), "error".into()), error);
        c.constructed.insert(name.clone());
    }
    name
}

/// Purpose: This function says whether a bare name is a builtin a program may
/// call or pass around.
///
/// Inputs:
/// - `name`: the identifier.
///
/// Outputs: true for a builtin.
pub(super) fn is_builtin_value(name: &str) -> bool {
    matches!(
        name,
        "len"
            | "str"
            | "int"
            | "float"
            | "bool"
            | "char"
            | "print"
            | "write"
            | "input"
            | "max_tasks"
            | "set_max_tasks"
            | "cancelled"
    )
}

/// Purpose: This function says whether a type is a number.
///
/// Inputs:
/// - `t`: a type.
///
/// Outputs: true for int or float.
fn is_num(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float)
}

/// Purpose: This function works out a builtin's result from its argument
/// types, without reporting anything.
///
/// Inputs:
/// - `name`: the builtin.
/// - `tys`: the resolved argument types.
///
/// Outputs: the result type, or an error message.
fn builtin_result(name: &str, tys: &[Ty]) -> Result<Ty, String> {
    let arity = |n: usize| -> Result<(), String> {
        if tys.len() == n {
            Ok(())
        } else {
            Err(format!(
                "{name}() takes {n} argument{}, got {}",
                if n == 1 { "" } else { "s" },
                tys.len()
            ))
        }
    };
    match name {
        "print" => {
            if tys.is_empty() || tys.len() > 2 {
                return Err(format!(
                    "print() takes a value and an optional ending, got {} arguments",
                    tys.len()
                ));
            }
            if tys.len() == 2 && !matches!(tys[1], Ty::Str | Ty::Var(_)) {
                return Err(format!("print()'s ending must be a str, got {}", tys[1]));
            }
            Ok(Ty::Nil)
        }
        "write" => arity(1).map(|_| Ty::Nil),
        "input" => {
            if tys.len() > 1 {
                return Err(format!(
                    "input() takes an optional prompt, got {} arguments",
                    tys.len()
                ));
            }
            Ok(Ty::Str)
        }
        "len" => {
            arity(1)?;
            match &tys[0] {
                Ty::Str
                | Ty::Char
                | Ty::Array(_)
                | Ty::Map(_)
                | Ty::Bytes
                | Ty::Stream(_)
                | Ty::Json
                | Ty::Var(_) => Ok(Ty::Int),
                t => Err(format!("len() has no answer for {t}")),
            }
        }
        "str" => arity(1).map(|_| Ty::Str),
        "int" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Float | Ty::Str | Ty::Bool | Ty::Char | Ty::Json | Ty::Var(_) => {
                    Ok(Ty::Int)
                }
                t => Err(format!("int() cannot convert {t}")),
            }
        }
        "float" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Float | Ty::Str | Ty::Json | Ty::Var(_) => Ok(Ty::Float),
                t => Err(format!("float() cannot convert {t}")),
            }
        }
        "bool" => {
            arity(1)?;
            match &tys[0] {
                Ty::Bool | Ty::Int | Ty::Str | Ty::Json | Ty::Var(_) => Ok(Ty::Bool),
                t => Err(format!("bool() cannot convert {t}")),
            }
        }
        "char" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Str | Ty::Char | Ty::Var(_) => Ok(Ty::Char),
                t => Err(format!("char() cannot convert {t}")),
            }
        }
        "max_tasks" => arity(0).map(|_| Ty::Int),
        "cancelled" => arity(0).map(|_| Ty::Bool),
        "set_max_tasks" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Var(_) => Ok(Ty::Int),
                t => Err(format!("set_max_tasks() takes an int, got {t}")),
            }
        }
        other => Err(format!("'{other}' cannot be called")),
    }
}

/// Purpose: This function types a builtin reached through a joined function
/// type, outside any body.
///
/// Inputs:
/// - `c`: the checker.
/// - `name`: the builtin.
/// - `tys`: the argument types.
/// - `span`: where.
///
/// Outputs: the result type, or `None`.
pub(super) fn builtin_value_result(
    c: &mut Checker,
    name: &str,
    tys: &[Ty],
    span: Span,
) -> Option<Ty> {
    let resolved: Vec<Ty> = tys.iter().map(|t| c.store.resolve(t)).collect();
    match builtin_result(name, &resolved) {
        Ok(t) => Some(t),
        Err(m) => {
            c.report(span, m);
            None
        }
    }
}

/// Purpose: This function types a std package function used as a value
/// without calling it, which v2 does not allow.
///
/// Inputs:
/// - `c`: the checker.
/// - `pkg`, `field`: the package and function.
/// - `span`: where.
/// - `f`: the frame.
/// - `node`: an address for a slot.
///
/// Outputs: a placeholder type, after reporting.
pub(super) fn package_fn_value(
    c: &mut Checker,
    pkg: &str,
    field: &str,
    span: Span,
    f: &mut Frame,
    node: usize,
) -> Ty {
    let _ = (f, node);
    c.report(
        span,
        format!(
            "{pkg}.{field} can only be called; to pass it along, wrap it: |x| {pkg}.{field}(x)"
        ),
    );
    Ty::Nil
}

/// Purpose: This function says whether a primitive type has a method.
///
/// Inputs:
/// - `t`: the receiver type.
/// - `name`: the method.
///
/// Outputs: true when the method exists.
pub(super) fn primitive_has_method(t: &Ty, name: &str) -> bool {
    let table: &[&str] = match t {
        Ty::Str => crate::string::STR_METHODS,
        Ty::Array(_) => crate::array::ARRAY_METHODS,
        Ty::Map(_) => crate::dict::DICT_METHODS,
        Ty::Bytes => crate::bytes::BYTES_METHODS,
        Ty::Future(_) => crate::future::FUTURE_METHODS,
        _ => &[],
    };
    table.contains(&name)
        || (matches!(t, Ty::Map(_)) && name == "merge")
        || (matches!(t, Ty::Json) && name == "len")
}

impl<'a> Checker<'a> {
    /// Purpose: This function types a call to a bare builtin.
    ///
    /// Inputs:
    /// - `name`: the builtin.
    /// - `tys`: the argument types.
    /// - `args`: the argument expressions, possibly empty for a call through a value.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: the call's type.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn builtin_call(
        &mut self,
        name: &str,
        tys: Vec<Ty>,
        args: &'a [TExpr],
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        let _ = (args, f, node);
        let resolved: Vec<Ty> = tys.iter().map(|t| self.store.resolve(t)).collect();
        match builtin_result(name, &resolved) {
            Ok(t) => t,
            Err(m) => {
                self.report(span, m);
                Ty::Nil
            }
        }
    }

    /// Purpose: This function types a call to a std package function.
    ///
    /// Inputs:
    /// - `pkg`: the package's global name, or `Grammar`.
    /// - `name`: the function.
    /// - `args`, `kwargs`: the call's arguments.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: the call's type.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn package_call(
        &mut self,
        pkg: &str,
        name: &str,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        let mut tys = Vec::new();
        for a in args {
            tys.push(self.check_expr(a, f));
        }
        for (k, v) in kwargs {
            let t = self.check_expr(v, f);
            let known = matches!((pkg, name, k.as_str()), ("fs", "read" | "read_bytes", "trust"));
            if known {
                self.unify(&t, &Ty::Bool, v.span, "trust");
            } else {
                self.report(v.span, format!("{pkg}.{name} has no parameter named '{k}'"));
            }
        }
        self.record_target(f, node, Target::Builtin(format!("{pkg}.{name}")));

        // The receiver-first spellings of the primitive methods.
        let receiver_first = matches!(
            (pkg, name),
            ("string", _)
                | ("dict", "keys" | "values" | "has" | "get" | "merge")
                | ("array", "map" | "filter" | "join")
        );
        if receiver_first {
            if tys.is_empty() {
                self.report(
                    span,
                    format!("{pkg}.{name} needs the value to work on as its first argument"),
                );
                return Ty::Nil;
            }
            let recv = tys.remove(0);
            let rest: &'a [TExpr] = if args.is_empty() { args } else { &args[1..] };
            return self.method_call(&recv, name, tys, rest, span, f, node);
        }

        let r: Vec<Ty> = tys.iter().map(|t| self.store.resolve(t)).collect();
        let count = |n: std::ops::RangeInclusive<usize>| n.contains(&r.len());
        let bad_count = |c: &mut Checker, want: &str| {
            c.report(span, format!("{pkg}.{name} takes {want}, got {} arguments", r.len()));
            Ty::Nil
        };
        macro_rules! want {
            ($i:expr, $t:expr, $what:expr) => {
                if $i < tys.len() {
                    let t = tys[$i].clone();
                    self.unify(&t, &$t, span, &format!("{pkg}.{name}'s {}", $what));
                }
            };
        }
        let num_arg = |c: &mut Checker, i: usize| {
            if let Some(t) = r.get(i)
                && !is_num(t)
                && !matches!(t, Ty::Var(_))
            {
                c.report(span, format!("{pkg}.{name} takes a number, got {t}"));
            }
        };
        match (pkg, name) {
            // ── math ─────────────────────────────────────────────────────────
            ("math", "floor" | "ceil" | "round" | "trunc") => {
                if !count(1..=1) {
                    return bad_count(self, "one number");
                }
                num_arg(self, 0);
                Ty::Int
            }
            ("math", "abs" | "sign") => {
                if !count(1..=1) {
                    return bad_count(self, "one number");
                }
                num_arg(self, 0);
                r[0].clone()
            }
            (
                "math",
                "sqrt" | "ln" | "log2" | "log10" | "exp" | "sin" | "cos" | "tan" | "asin" | "acos"
                | "atan",
            ) => {
                if !count(1..=1) {
                    return bad_count(self, "one number");
                }
                num_arg(self, 0);
                Ty::Float
            }
            ("math", "is_nan" | "is_inf") => {
                if !count(1..=1) {
                    return bad_count(self, "one number");
                }
                num_arg(self, 0);
                Ty::Bool
            }
            ("math", "min" | "max" | "pow" | "atan2" | "hypot") => {
                if !count(2..=2) {
                    return bad_count(self, "two numbers");
                }
                num_arg(self, 0);
                num_arg(self, 1);
                let both_int = matches!((&r[0], &r[1]), (Ty::Int, Ty::Int));
                if both_int && matches!(name, "min" | "max" | "pow") { Ty::Int } else { Ty::Float }
            }
            ("math", "clamp") => {
                if !count(3..=3) {
                    return bad_count(self, "three numbers");
                }
                (0..3).for_each(|i| num_arg(self, i));
                if r.iter().all(|t| matches!(t, Ty::Int)) { Ty::Int } else { Ty::Float }
            }
            ("math", "pi" | "e" | "tau" | "inf" | "nan") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Float
            }
            // ── array (copies, not the in-place methods) ────────────────────
            ("array", "sort" | "reverse") => {
                if !count(1..=1) {
                    return bad_count(self, "one array");
                }
                match &r[0] {
                    Ty::Array(_) | Ty::Var(_) => tys[0].clone(),
                    t => {
                        self.report(span, format!("{pkg}.{name} takes an array, got {t}"));
                        Ty::Nil
                    }
                }
            }
            // ── fs ───────────────────────────────────────────────────────────
            ("fs", "read") => {
                want!(0, Ty::Str, "path");
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                Ty::Str
            }
            ("fs", "read_bytes") => {
                want!(0, Ty::Str, "path");
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                Ty::Bytes
            }
            ("fs", "write_bytes" | "append_bytes") => {
                if !count(2..=2) {
                    return bad_count(self, "a path and bytes");
                }
                want!(0, Ty::Str, "path");
                want!(1, Ty::Bytes, "data");
                Ty::Nil
            }
            ("fs", "read_stdin_bytes") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Bytes
            }
            ("fs", "write_stdout_bytes") => {
                if !count(1..=1) {
                    return bad_count(self, "bytes");
                }
                want!(0, Ty::Bytes, "data");
                Ty::Nil
            }
            ("fs", "write" | "append") => {
                if !count(2..=2) {
                    return bad_count(self, "a path and a str");
                }
                want!(0, Ty::Str, "path");
                want!(1, Ty::Str, "content");
                Ty::Nil
            }
            ("fs", "exists" | "is_file" | "is_dir") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Bool
            }
            ("fs", "delete" | "mkdir" | "rmdir") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Nil
            }
            ("fs", "list_dir") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Array(Box::new(Ty::Str))
            }
            ("fs", "size") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Int
            }
            ("fs", "copy" | "rename") => {
                if !count(2..=2) {
                    return bad_count(self, "two paths");
                }
                want!(0, Ty::Str, "source");
                want!(1, Ty::Str, "destination");
                Ty::Nil
            }
            // ── time ─────────────────────────────────────────────────────────
            ("time", "now" | "now_ms") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Int
            }
            ("time", "monotonic") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Float
            }
            ("time", "sleep" | "after") => {
                if !count(1..=1) {
                    return bad_count(self, "a number of seconds");
                }
                num_arg(self, 0);
                if name == "after" { Ty::Future(Box::new(Ty::Nil)) } else { Ty::Nil }
            }
            ("time", "local") => {
                if !count(1..=1) {
                    return bad_count(self, "a timezone name or nil");
                }
                want!(0, Ty::opt(Ty::Str), "timezone");
                Ty::Str
            }
            ("time", "utc") => {
                if !count(1..=1) {
                    return bad_count(self, "a timestamp");
                }
                want!(0, Ty::Int, "timestamp");
                Ty::Str
            }
            ("time", "parts") => {
                if !count(1..=1) {
                    return bad_count(self, "a timestamp");
                }
                want!(0, Ty::Int, "timestamp");
                Ty::Struct("TimeParts".into())
            }
            ("time", "stamp") => {
                if !count(3..=6) {
                    return bad_count(self, "three to six ints");
                }
                for i in 0..r.len() {
                    want!(i, Ty::Int, "calendar field");
                }
                Ty::Int
            }
            // ── http and uhttp ───────────────────────────────────────────────
            ("http" | "uhttp", "get" | "delete" | "head" | "get_bytes") => {
                if !count(1..=2) {
                    return bad_count(self, "a url and optional headers");
                }
                want!(0, Ty::Str, "url");
                want!(1, Ty::Map(Box::new(Ty::Str)), "headers");
                if name == "get_bytes" {
                    Ty::Struct("HttpBytesResponse".into())
                } else {
                    Ty::Struct("HttpResponse".into())
                }
            }
            ("http" | "uhttp", "post" | "put" | "post_bytes") => {
                if !count(2..=3) {
                    return bad_count(self, "a url, a body and optional headers");
                }
                want!(0, Ty::Str, "url");
                if name == "post_bytes" {
                    want!(1, Ty::Bytes, "body");
                } else {
                    want!(1, Ty::Str, "body");
                }
                want!(2, Ty::Map(Box::new(Ty::Str)), "headers");
                Ty::Struct("HttpResponse".into())
            }
            ("uhttp", "stream") => {
                if !count(2..=3) {
                    return bad_count(self, "a url, a handler and optional headers");
                }
                want!(0, Ty::Str, "url");
                want!(2, Ty::Map(Box::new(Ty::Str)), "headers");
                // The handler is called once per line; its answer only matters
                // when it is `false`, which stops the stream.
                let h = tys[1].clone();
                self.call_value(&h, vec![Ty::Str], span, f, node + 1);
                Ty::Nil
            }
            // ── sh ───────────────────────────────────────────────────────────
            ("sh", "exec") => {
                if !count(1..=1) {
                    return bad_count(self, "a command");
                }
                want!(0, Ty::Str, "command");
                Ty::Str
            }
            ("sh", "run") => {
                if !count(1..=1) {
                    return bad_count(self, "a command");
                }
                want!(0, Ty::Str, "command");
                Ty::Int
            }
            ("sh", "output") => {
                if !count(1..=1) {
                    return bad_count(self, "a command");
                }
                want!(0, Ty::Str, "command");
                Ty::Struct("ShOutput".into())
            }
            // ── json ─────────────────────────────────────────────────────────
            ("json", "parse") => {
                if !count(1..=1) {
                    return bad_count(self, "a str");
                }
                want!(0, Ty::Str, "input");
                Ty::Json
            }
            ("json", "stringify" | "stringify_pretty") => {
                if !count(1..=1) {
                    return bad_count(self, "one value");
                }
                Ty::Str
            }
            // ── env ──────────────────────────────────────────────────────────
            ("env", "get") => {
                if !count(1..=1) {
                    return bad_count(self, "a name");
                }
                want!(0, Ty::Str, "name");
                Ty::opt(Ty::Str)
            }
            ("env", "set") => {
                if !count(2..=2) {
                    return bad_count(self, "a name and a value");
                }
                want!(0, Ty::Str, "name");
                want!(1, Ty::Str, "value");
                Ty::Nil
            }
            ("env", "args") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Array(Box::new(Ty::Str))
            }
            ("env", "cwd") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Str
            }
            // ── path ─────────────────────────────────────────────────────────
            ("path", "join") => {
                if r.len() < 2 {
                    return bad_count(self, "two or more segments");
                }
                for i in 0..r.len() {
                    want!(i, Ty::Str, "segment");
                }
                Ty::Str
            }
            ("path", "basename" | "dirname" | "stem" | "abs") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Str
            }
            ("path", "ext") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::opt(Ty::Str)
            }
            ("path", "is_abs") => {
                if !count(1..=1) {
                    return bad_count(self, "a path");
                }
                want!(0, Ty::Str, "path");
                Ty::Bool
            }
            // ── random ───────────────────────────────────────────────────────
            ("random", "int") => {
                if !count(2..=2) {
                    return bad_count(self, "a low and a high int");
                }
                want!(0, Ty::Int, "low");
                want!(1, Ty::Int, "high");
                Ty::Int
            }
            ("random", "float") => {
                if !count(0..=0) {
                    return bad_count(self, "no arguments");
                }
                Ty::Float
            }
            ("random", "choice") => {
                if !count(1..=1) {
                    return bad_count(self, "an array");
                }
                let elem = self.slot(f, node + 2);
                want!(0, Ty::Array(Box::new(elem.clone())), "array");
                elem
            }
            ("random", "shuffle") => {
                if !count(1..=1) {
                    return bad_count(self, "an array");
                }
                let elem = self.slot(f, node + 2);
                want!(0, Ty::Array(Box::new(elem)), "array");
                Ty::Nil
            }
            ("random", "seed") => {
                if !count(1..=1) {
                    return bad_count(self, "an int");
                }
                want!(0, Ty::Int, "seed");
                Ty::Nil
            }
            // ── bytes ────────────────────────────────────────────────────────
            ("bytes", "zeros") => {
                if !count(1..=1) {
                    return bad_count(self, "a length");
                }
                want!(0, Ty::Int, "length");
                Ty::Bytes
            }
            ("bytes", "from_ints") => {
                if !count(1..=1) {
                    return bad_count(self, "an array of ints");
                }
                want!(0, Ty::Array(Box::new(Ty::Int)), "octets");
                Ty::Bytes
            }
            ("bytes", "concat") => {
                if !count(2..=2) {
                    return bad_count(self, "two bytes values");
                }
                want!(0, Ty::Bytes, "first");
                want!(1, Ty::Bytes, "second");
                Ty::Bytes
            }
            // ── Grammar ──────────────────────────────────────────────────────
            ("Grammar", "new") => {
                if !count(1..=3) {
                    return bad_count(self, "a pattern and an optional anchor and stop");
                }
                for i in 0..r.len() {
                    want!(i, Ty::Str, "argument");
                }
                Ty::Grammar
            }
            _ => {
                self.report(span, format!("std/{pkg} has no function '{name}'"));
                Ty::Nil
            }
        }
    }
}

/// Purpose: This function types a call to a primitive method.
///
/// Inputs:
/// - `c`: the checker.
/// - `recv`: the receiver's type, already followed one level.
/// - `name`: the method.
/// - `tys`: the argument types.
/// - `args`: the argument expressions, possibly empty.
/// - `span`: where.
/// - `f`: the frame.
/// - `node`: the call's address.
///
/// Outputs: the call's type.
#[allow(clippy::too_many_arguments)]
pub(super) fn primitive_method(
    c: &mut Checker,
    recv: &Ty,
    name: &str,
    tys: Vec<Ty>,
    args: &[TExpr],
    span: Span,
    f: &mut Frame,
    node: usize,
) -> Ty {
    let _ = args;
    let resolved_recv = c.store.resolve(recv);
    let want_n = crate::builtins::primitive_method_arity(name);
    if !primitive_has_method(&resolved_recv, name) {
        let hint = match (&resolved_recv, name) {
            (Ty::Map(_), "contains") => "; a dict answers key membership with has(key)",
            _ => "",
        };
        c.report(span, format!("{resolved_recv} has no method '{name}'{hint}"));
        return Ty::Nil;
    }
    // `map` and `filter` take their function and then any extra arguments.
    let at_least = matches!(name, "map" | "filter");
    if let Some(n) = want_n
        && (if at_least { tys.len() < n } else { tys.len() != n })
    {
        c.report(
            span,
            format!(
                "{resolved_recv}.{name}() takes {n} argument{}, got {}",
                if n == 1 { "" } else { "s" },
                tys.len()
            ),
        );
        return Ty::Nil;
    }
    let arg = |i: usize| tys[i].clone();
    let want = |c: &mut Checker, i: usize, t: Ty, what: &str| {
        let a = tys[i].clone();
        c.unify(&a, &t, span, &format!("{name}'s {what}"));
    };
    match recv {
        Ty::Str => match name {
            "len" | "index_of" | "last_index_of" | "count" => {
                if !tys.is_empty() {
                    want(c, 0, Ty::Str, "argument");
                }
                Ty::Int
            }
            "upper" | "lower" | "trim" | "trim_start" | "trim_end" | "capitalize" => Ty::Str,
            "split" => {
                want(c, 0, Ty::Str, "separator");
                Ty::Array(Box::new(Ty::Str))
            }
            "lines" => Ty::Array(Box::new(Ty::Str)),
            "contains" | "starts_with" | "ends_with" => {
                match c.store.shallow(&arg(0)) {
                    Ty::Char => {}
                    _ => want(c, 0, Ty::Str, "argument"),
                }
                Ty::Bool
            }
            "is_empty" => Ty::Bool,
            "replace" => {
                want(c, 0, Ty::Str, "pattern");
                want(c, 1, Ty::Str, "replacement");
                Ty::Str
            }
            "encode" => Ty::Bytes,
            "repeat" => {
                want(c, 0, Ty::Int, "count");
                Ty::Str
            }
            "slice" => {
                want(c, 0, Ty::Int, "start");
                want(c, 1, Ty::Int, "end");
                Ty::Str
            }
            "pad_start" | "pad_end" => {
                want(c, 0, Ty::Int, "width");
                want(c, 1, Ty::Str, "padding");
                Ty::Str
            }
            _ => Ty::Nil,
        },
        Ty::Array(elem) => {
            let elem = (**elem).clone();
            match name {
                "len" => Ty::Int,
                "push" => {
                    let a = arg(0);
                    c.unify(&elem, &a, span, "the elements of an array");
                    Ty::Nil
                }
                "pop" => elem,
                "contains" => {
                    let a = arg(0);
                    c.unify(&elem, &a, span, "contains");
                    Ty::Bool
                }
                "sort" | "reverse" => Ty::Nil,
                "join" => {
                    want(c, 0, Ty::Str, "separator");
                    Ty::Str
                }
                // Extra arguments go to the function after each element, in the
                // order a pipe uses: `xs.map(scale, 10)` calls `scale(x, 10)`.
                "map" => {
                    let fv = arg(0);
                    let mut call = vec![elem];
                    call.extend(tys[1..].iter().cloned());
                    let r = c.call_value(&fv, call, span, f, node + 1);
                    let out = c.slot(f, node + 2);
                    c.unify(&out, &r, span, "map's results");
                    Ty::Array(Box::new(out))
                }
                "filter" => {
                    let fv = arg(0);
                    let mut call = vec![elem.clone()];
                    call.extend(tys[1..].iter().cloned());
                    let r = c.call_value(&fv, call, span, f, node + 1);
                    c.unify(&r, &Ty::Bool, span, "filter's predicate (it must return a bool)");
                    Ty::Array(Box::new(elem))
                }
                _ => Ty::Nil,
            }
        }
        Ty::Map(val) => {
            let val = (**val).clone();
            match name {
                "len" => Ty::Int,
                "keys" => Ty::Array(Box::new(Ty::Str)),
                "values" => Ty::Array(Box::new(val)),
                "has" => {
                    want(c, 0, Ty::Str, "key");
                    Ty::Bool
                }
                "get" => {
                    want(c, 0, Ty::Str, "key");
                    Ty::opt(val)
                }
                "merge" => {
                    let other = arg(0);
                    let me = Ty::Map(Box::new(val));
                    c.unify(&me, &other, span, "merge");
                    me
                }
                _ => Ty::Nil,
            }
        }
        Ty::Bytes => match name {
            "len" => Ty::Int,
            "decode" => Ty::Str,
            "slice" => {
                want(c, 0, Ty::Int, "start");
                want(c, 1, Ty::Int, "end");
                Ty::Bytes
            }
            _ => Ty::Nil,
        },
        Ty::Future(_) => match name {
            "ready" => Ty::Bool,
            "cancel" => Ty::Nil,
            _ => Ty::Nil,
        },
        Ty::Json => Ty::Int,
        _ => {
            let _ = FuncRef::Builtin(String::new());
            Ty::Nil
        }
    }
}
