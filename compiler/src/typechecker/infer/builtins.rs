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
use super::ty::{FuncRef, SizedNum, Ty, with_article};
use super::{Checker, StructInfo, Target};
use crate::frontend::ast::Expr;
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
    SizedNum::from_name(name).is_some()
        || SizedNum::from_converter(name).is_some()
        || matches!(name, "long" | "double" | "toLong" | "toDouble")
        || matches!(
            name,
            "len"
                | "str"
                | "int"
                | "float"
                | "bool"
                | "char"
                | "toInt"
                | "toFloat"
                | "toString"
                | "toBool"
                | "toChar"
                | "toDict"
                | "print"
                | "write"
                | "input"
                | "max_tasks"
                | "set_max_tasks"
                | "max_tokens"
                | "set_max_tokens"
                | "cancelled"
        )
}

/// Purpose: This function gives the name a builtin goes by: `long` and
/// `double` are `int` and `float`, and so are their converters.
///
/// Inputs:
/// - `name`: the name called.
///
/// Outputs: the builtin's own name.
pub(crate) fn canonical(name: &str) -> &str {
    match name {
        "long" => "int",
        "double" => "float",
        "toLong" => "toInt",
        "toDouble" => "toFloat",
        other => other,
    }
}

/// Purpose: This function gives the type a type function checks for.
///
/// Inputs:
/// - `name`: `int`, `float`, `str`, `bool` or `char`.
///
/// Outputs: that type.
fn type_fn_ty(name: &str) -> Ty {
    match name {
        "int" => Ty::Int,
        "float" => Ty::Float,
        "str" => Ty::Str,
        "bool" => Ty::Bool,
        _ => Ty::Char,
    }
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

/// Purpose: This function gives the message for a `toDict` argument that is
/// not text.
///
/// Inputs:
/// - `t`: the argument's type.
///
/// Outputs: the message, naming what `toDict` takes.
fn todict_text_error(t: &Ty) -> String {
    format!(
        "toDict() reads JSON text, so it takes a str, got {t}; a dict of values is written as a \
         literal, such as {{\"k\": v}}"
    )
}

/// Purpose: This function says whether `toDict` can read a value type: a
/// str, int, float or bool, or a dict or array of those, nested any depth.
///
/// Inputs:
/// - `t`: a resolved value type.
///
/// Outputs: true when the reader can build it.
fn todict_readable(t: &Ty) -> bool {
    match t {
        Ty::Str | Ty::Int | Ty::Float | Ty::Bool => true,
        Ty::Map(v) | Ty::Array(v) => todict_readable(v),
        _ => false,
    }
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
    let name = canonical(name);
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
                | Ty::Var(_) => Ok(Ty::Int),
                t => Err(format!("len() has no answer for {t}")),
            }
        }
        // The type functions check that a value already has their type and
        // return it unchanged; an int is also a float.
        "int" | "float" | "str" | "bool" | "char" => {
            arity(1)?;
            let want = type_fn_ty(name);
            let fits = match &tys[0] {
                t if *t == want => true,
                Ty::Var(_) => true,
                Ty::Int => want == Ty::Float,
                _ => false,
            };
            if fits {
                return Ok(want);
            }
            let (article, conv) = match name {
                "int" => ("an int", "toInt"),
                "float" => ("a float or an int", "toFloat"),
                "str" => ("a str", "toString"),
                "bool" => ("a bool", "toBool"),
                _ => ("a char", "toChar"),
            };
            Err(format!("{name}() needs {article}, got {}; convert one with {conv}()", tys[0]))
        }
        // A sized type function checks the same way; a literal that fits is
        // let through in `Checker::builtin_call`, before this.
        n if SizedNum::from_name(n).is_some() => {
            arity(1)?;
            let s = SizedNum::from_name(n).unwrap_or(SizedNum::Byte);
            match &tys[0] {
                Ty::Sized(t) if *t == s => Ok(Ty::Sized(s)),
                Ty::Var(_) => Ok(Ty::Sized(s)),
                t => Err(format!(
                    "{n}() needs {}, got {t}; convert one with {}()",
                    with_article(n),
                    s.converter()
                )),
            }
        }
        // The converters make a value of their type from another, and raise
        // when it does not fit.
        "toString" => arity(1).map(|_| Ty::Str),
        "toInt" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Float | Ty::Sized(_) | Ty::Str | Ty::Bool | Ty::Char | Ty::Var(_) => {
                    Ok(Ty::Int)
                }
                t => Err(format!("toInt() cannot convert {t}")),
            }
        }
        "toFloat" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Float | Ty::Sized(_) | Ty::Str | Ty::Var(_) => Ok(Ty::Float),
                t => Err(format!("toFloat() cannot convert {t}")),
            }
        }
        "toBool" => {
            arity(1)?;
            match &tys[0] {
                Ty::Bool | Ty::Int | Ty::Str | Ty::Var(_) => Ok(Ty::Bool),
                Ty::Sized(s) if !s.is_float() => Ok(Ty::Bool),
                t => Err(format!("toBool() cannot convert {t}")),
            }
        }
        "toChar" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Str | Ty::Char | Ty::Var(_) => Ok(Ty::Char),
                Ty::Sized(s) if !s.is_float() => Ok(Ty::Char),
                t => Err(format!("toChar() cannot convert {t}")),
            }
        }
        n if SizedNum::from_converter(n).is_some() => {
            arity(1)?;
            let s = SizedNum::from_converter(n).unwrap_or(SizedNum::Byte);
            match &tys[0] {
                Ty::Int | Ty::Float | Ty::Sized(_) | Ty::Str | Ty::Bool | Ty::Char | Ty::Var(_) => {
                    Ok(Ty::Sized(s))
                }
                t => Err(format!("{n}() cannot convert {t}")),
            }
        }
        // `toDict` called by name gets a value type from how the program
        // uses the dict, in `Checker::todict_call`; passed as a value, as in
        // `texts.map(toDict)`, it reads a dict of strings.
        "toDict" => {
            arity(1)?;
            match &tys[0] {
                Ty::Str | Ty::Var(_) => Ok(Ty::Map(Box::new(Ty::Str))),
                t => Err(todict_text_error(t)),
            }
        }
        "max_tasks" | "max_tokens" => arity(0).map(|_| Ty::Int),
        "cancelled" => arity(0).map(|_| Ty::Bool),
        "set_max_tasks" | "set_max_tokens" => {
            arity(1)?;
            match &tys[0] {
                Ty::Int | Ty::Var(_) => Ok(Ty::Int),
                t => Err(format!("{name}() takes an int, got {t}")),
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
    table.contains(&name) || (matches!(t, Ty::Map(_)) && name == "merge")
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
        args: &'a [Expr],
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        if name == "toDict" {
            return self.todict_call(&tys, span, f, node);
        }
        // A literal takes the type of the sized type function it meets, when it
        // fits: `byte(5)` is the byte 5.
        if let Some(s) = SizedNum::from_name(name)
            && let [arg] = args
            && let Some(lit) = super::check::num_literal(arg)
        {
            self.literal_fits(lit, s, span);
            return Ty::Sized(s);
        }
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
        args: &'a [Expr],
        kwargs: &'a [(String, Expr)],
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
            let rest: &'a [Expr] = if args.is_empty() { args } else { &args[1..] };
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
            // ── the inference prelude's own package; see src/infer/ ─────────
            ("__infer", _) => {
                let (params, ret): (Vec<Ty>, Ty) = match name {
                    "data" => (vec![Ty::Int], Ty::Str),
                    "config" => (vec![Ty::Int], Ty::Int),
                    "char_class" => (vec![Ty::Char], Ty::Int),
                    "taint" => (vec![Ty::Str], Ty::Str),
                    "ready" => (Vec::new(), Ty::Bool),
                    "setup" | "map" => (Vec::new(), Ty::Nil),
                    "next" => (vec![Ty::Array(Box::new(Ty::Int)), Ty::Int, Ty::Int], Ty::Int),
                    "conversation" => (Vec::new(), Ty::Int),
                    // A prompt's text, from a prompt or a string.
                    "text" => {
                        if !count(1..=1) {
                            return bad_count(self, "a prompt");
                        }
                        let t = self.store.shallow(&tys[0]);
                        if !matches!(t, Ty::Prompt | Ty::Str | Ty::Var(_)) {
                            let t = self.store.resolve(&t);
                            self.report(span, format!("'?' dereferences a prompt, got {t}"));
                        }
                        return Ty::Str;
                    }
                    other => {
                        self.report(span, format!("__infer has no function '{other}'"));
                        return Ty::Nil;
                    }
                };
                if r.len() != params.len() {
                    return bad_count(self, &format!("{} arguments", params.len()));
                }
                for (i, p) in params.iter().enumerate() {
                    want!(i, p.clone(), "argument");
                }
                ret
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
    args: &[Expr],
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
        _ => {
            let _ = FuncRef::Builtin(String::new());
            Ty::Nil
        }
    }
}

// ── toDict ────────────────────────────────────────────────────────────────────
//
// `toDict(text)` returns a dict whose value type is a slot the program's use
// fills, per part 2 of `design/v2-simplify.md`: `d["a"] + 1` makes it an int,
// `d["a"]["x"]` a dict whose own value type follows the same rule, `d["a"][0]`
// an array. Every such slot is kept in `Checker::todict_slots`, and once the
// passes settle, `Checker::run` makes each one nothing filled a str. A call
// that converts, such as `toInt(d["k"])`, accepts a str and fills nothing.

impl<'a> Checker<'a> {
    /// Purpose: This function types a call to `toDict` by name.
    ///
    /// Inputs:
    /// - `tys`: the argument types.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: a dict whose value type is the call's own slot.
    fn todict_call(&mut self, tys: &[Ty], span: Span, f: &mut Frame, node: usize) -> Ty {
        // `+ 5`, past the offsets a call site keeps its own slots under: `map`
        // calls its function value at `node + 1` and keeps its results at
        // `node + 2`, and a method a `toDict` value takes uses `node + 3`.
        let value = self.todict_slot(f, node + 5);
        if tys.len() != 1 {
            self.report(span, format!("toDict() takes 1 argument, got {}", tys.len()));
            return Ty::Map(Box::new(value));
        }
        match self.store.shallow(&tys[0]) {
            Ty::Str => {}
            Ty::Var(_) => {
                self.unify(&tys[0], &Ty::Str, span, "toDict's text");
            }
            other => {
                let other = self.store.resolve(&other);
                self.report(span, todict_text_error(&other));
            }
        }
        if self.final_pass {
            let t = self.store.resolve(&value);
            if !todict_readable(&t) {
                self.report(
                    span,
                    format!(
                        "toDict reads each value as a str, int, float, bool, dict or array, but \
                         this program uses the values as {t}; read them as strings and convert \
                         at the use, such as toInt(d[\"k\"])"
                    ),
                );
            }
        }
        Ty::Map(Box::new(value))
    }

    /// Purpose: This function returns a node's slot and records it as a value
    /// `toDict` reads.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `node`: the address the slot is cached under.
    ///
    /// Outputs: the slot.
    fn todict_slot(&mut self, f: &mut Frame, node: usize) -> Ty {
        let slot = self.slot(f, node);
        if !self.is_todict_slot(&slot) {
            self.todict_slots.push(slot.clone());
        }
        slot
    }

    /// Purpose: This function says whether a type is a slot holding values
    /// `toDict` reads.
    ///
    /// Inputs:
    /// - `t`: a type.
    ///
    /// Outputs: true when `t` is one of `todict_slots`.
    pub(super) fn is_todict_slot(&mut self, t: &Ty) -> bool {
        for i in 0..self.todict_slots.len() {
            let s = self.todict_slots[i].clone();
            if self.store.same_slot(&s, t) {
                return true;
            }
        }
        false
    }

    /// Purpose: This function gives an empty `toDict` slot the shape its use
    /// asks for: a dict when it is indexed by a str or given a dict method, an
    /// array when indexed by an int or given an array method.
    ///
    /// Inputs:
    /// - `v`: the empty slot.
    /// - `as_dict`: a dict when true, an array when false.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the address the inner slot is cached under.
    ///
    /// Outputs: the inner slot: the dict's value type or the array's element
    /// type, which follows the same rule.
    pub(super) fn settle_todict(
        &mut self,
        v: &Ty,
        as_dict: bool,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        let inner = self.todict_slot(f, node);
        let shape = if as_dict {
            Ty::Map(Box::new(inner.clone()))
        } else {
            Ty::Array(Box::new(inner.clone()))
        };
        self.unify(v, &shape, span, "a value toDict reads");
        inner
    }

    /// Purpose: This function makes every `toDict` slot nothing filled a str,
    /// once the passes have settled.
    ///
    /// Inputs: none.
    ///
    /// Outputs: true when it filled one, so the passes go on from that.
    pub(super) fn default_todict_values(&mut self) -> bool {
        let mut filled = false;
        for slot in self.todict_slots.clone() {
            if matches!(self.store.shallow(&slot), Ty::Var(_)) {
                let _ = self.store.join(&slot, &Ty::Str);
                filled = true;
            }
        }
        filled
    }
}

/// Purpose: This function says which shape a method asks of a `toDict` value
/// whose type is not settled: a method only dicts have makes it a dict, one
/// only arrays have an array, and one strings share, such as `len`, neither.
///
/// Inputs:
/// - `name`: the method.
///
/// Outputs: `Some(true)` for a dict, `Some(false)` for an array, or `None`.
pub(super) fn todict_method_shape(name: &str) -> Option<bool> {
    let on_str = crate::string::STR_METHODS.contains(&name);
    let on_dict = crate::dict::DICT_METHODS.contains(&name) || name == "merge";
    let on_array = crate::array::ARRAY_METHODS.contains(&name);
    match (on_dict, on_array, on_str) {
        (true, false, false) => Some(true),
        (false, true, false) => Some(false),
        _ => None,
    }
}
