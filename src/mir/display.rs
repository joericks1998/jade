//! MIR as text, for `jade build --backend=static --emit mir` and for tests.
//!
//! The format is for reading, not for parsing back:
//!
//! ```text
//! fn main() -> nil
//!   let _0: int  // x
//!   bb0:
//!     _0 = 1
//!     eval print(tostr.int(_0), "\n")
//!     return nil
//! ```

use std::fmt::{self, Display, Formatter, Write};

use super::*;

impl Display for Program {
    /// Purpose: This function writes a whole program: its structs, globals and
    /// functions.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, s) in self.structs.iter().enumerate() {
            let fields: Vec<String> = s.fields.iter().map(|(n, t)| format!("{n}: {t}")).collect();
            writeln!(f, "struct {} #{i} {{ {} }}", s.name, fields.join(", "))?;
        }
        for (i, g) in self.globals.iter().enumerate() {
            writeln!(f, "global g{i}: {}  // {}", g.ty, g.name)?;
        }
        for (i, func) in self.funcs.iter().enumerate() {
            let marker = if FuncId(i as u32) == self.main { " (main)" } else { "" };
            writeln!(f)?;
            write!(f, "{}", FuncText { func, id: i, marker })?;
        }
        Ok(())
    }
}

/// Purpose: This struct pairs a function with its id for printing.
struct FuncText<'a> {
    func: &'a Func,
    id: usize,
    marker: &'a str,
}

impl Display for FuncText<'_> {
    /// Purpose: This function writes one function.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let func = self.func;
        let params: Vec<String> =
            (0..func.params as usize).map(|i| format!("_{i}: {}", func.locals[i].ty)).collect();
        writeln!(
            f,
            "fn f{} {}({}) -> {}{}",
            self.id,
            func.name,
            params.join(", "),
            func.ret,
            self.marker
        )?;
        for (i, l) in func.locals.iter().enumerate().skip(func.params as usize) {
            match &l.name {
                Some(n) => writeln!(f, "  let _{i}: {}  // {n}", l.ty)?,
                None => writeln!(f, "  let _{i}: {}", l.ty)?,
            }
        }
        for (i, b) in func.blocks.iter().enumerate() {
            writeln!(f, "  bb{i}:")?;
            for s in &b.stmts {
                writeln!(f, "    {s}")?;
            }
            writeln!(f, "    {}", b.term)?;
        }
        Ok(())
    }
}

impl Display for Place {
    /// Purpose: This function writes a place as `_n` or `gn`.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Place::Local(l) => write!(f, "_{}", l.0),
            Place::Global(g) => write!(f, "g{}", g.0),
        }
    }
}

impl Display for Operand {
    /// Purpose: This function writes an operand.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Copy(p) => write!(f, "{p}"),
            Operand::Move(l) => write!(f, "move _{}", l.0),
            Operand::Const(c) => write!(f, "{c}"),
        }
    }
}

impl Display for Const {
    /// Purpose: This function writes a constant the way source would.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Const::Int(i) => write!(f, "{i}"),
            Const::Float(x) => write!(f, "{x:?}"),
            Const::Bool(b) => write!(f, "{b}"),
            Const::Char(c) => write!(f, "{c:?}"),
            Const::Str(s) => write!(f, "{s:?}"),
            Const::Nil(t) => write!(f, "nil: {t}"),
            Const::Static(t) => write!(f, "static {t}"),
        }
    }
}

/// Purpose: This function joins operands with commas.
///
/// Inputs:
/// - `ops`: the operands.
///
/// Outputs: the joined text.
fn list(ops: &[Operand]) -> String {
    let mut s = String::new();
    for (i, o) in ops.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{o}");
    }
    s
}

impl Display for Stmt {
    /// Purpose: This function writes one statement.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Assign(p, rv) => write!(f, "{p} = {rv}"),
            Stmt::Eval(rv) => write!(f, "eval {rv}"),
            Stmt::SetField { object, strukt, index, value } => {
                write!(f, "{object}.{strukt}#{index} = {value}")
            }
            Stmt::DictSet { dict, key, value } => write!(f, "{dict}[{key}] = {value}"),
            Stmt::Drop(p) => write!(f, "drop {p}"),
            Stmt::Raise(o) => write!(f, "raise {o}"),
        }
    }
}

impl Display for Rvalue {
    /// Purpose: This function writes one rvalue.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(o) => write!(f, "{o}"),
            Rvalue::Bin(op, a, b) => write!(f, "{} {a}, {b}", bin_name(op)),
            Rvalue::Un(op, a) => write!(f, "{op:?} {a}"),
            Rvalue::Call(id, args) => write!(f, "call f{}({})", id.0, list(args)),
            Rvalue::Op(op, args) => write!(f, "{}({})", op_name(op), list(args)),
            Rvalue::Array(t, els) => write!(f, "array.{t} [{}]", list(els)),
            Rvalue::Dict(t, entries) => {
                let parts: Vec<String> = entries.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                write!(f, "dict.{t} {{{}}}", parts.join(", "))
            }
            Rvalue::Struct(n, fields) => write!(f, "{n} {{{}}}", list(fields)),
            Rvalue::Field { object, strukt, index } => write!(f, "{object}.{strukt}#{index}"),
            Rvalue::Wrap(t, o) => write!(f, "wrap.{t} {o}"),
            Rvalue::Unwrap(o) => write!(f, "unwrap {o}"),
            Rvalue::IsNil(o) => write!(f, "isnil {o}"),
            Rvalue::IntToFloat(o) => write!(f, "itof {o}"),
            Rvalue::TypeId(o) => write!(f, "typeid {o}"),
            Rvalue::IsA(o, ids) => write!(f, "isa {o} {ids:?}"),
            Rvalue::FnAddr(id) => write!(f, "fnaddr f{}", id.0),
            Rvalue::Spawn(id, args) => write!(f, "spawn f{}({})", id.0, list(args)),
            Rvalue::CallIndirect(c, args) => write!(f, "callindirect {c}({})", list(args)),
            Rvalue::ErrPending => write!(f, "errpending"),
            Rvalue::TakeErr => write!(f, "takeerr"),
        }
    }
}

/// Purpose: This function names a binary operator.
///
/// Inputs:
/// - `op`: the operator.
///
/// Outputs: its short name, such as `add.int` or `lt.str`.
fn bin_name(op: &BinOp) -> String {
    match op {
        BinOp::Arith(a, n) => format!("{a:?}.{n:?}").to_lowercase(),
        BinOp::Bit(b) => format!("{b:?}").to_lowercase(),
        BinOp::Cmp(c, t) => format!("{}.{t}", format!("{c:?}").to_lowercase()),
        BinOp::Concat => "concat".to_string(),
    }
}

/// Purpose: This function names a support operation.
///
/// Inputs:
/// - `op`: the operation.
///
/// Outputs: its short name, with its type where it has one.
fn op_name(op: &Op) -> String {
    let typed = |n: &str, t: &Ty| format!("{n}.{t}");
    match op {
        Op::ToStr(t) => typed("tostr", t),
        Op::ToInt(t) => typed("toint", t),
        Op::ToFloat(t) => typed("tofloat", t),
        Op::ToBool(t) => typed("tobool", t),
        Op::ToChar(t) => typed("tochar", t),
        Op::ArrayGet(t) => typed("array.get", t),
        Op::ArraySet(t) => typed("array.set", t),
        Op::ArrayPush(t) => typed("array.push", t),
        Op::ArrayPop(t) => typed("array.pop", t),
        Op::ArrayContains(t) => typed("array.contains", t),
        Op::ArraySort(t) => typed("array.sort", t),
        Op::ArrayReverse(t) => typed("array.reverse", t),
        Op::ArraySorted(t) => typed("array.sorted", t),
        Op::ArrayReversed(t) => typed("array.reversed", t),
        Op::ArrayJoin(t) => typed("array.join", t),
        Op::DictGet(t) => typed("dict.get", t),
        Op::DictGetOpt(t) => typed("dict.getopt", t),
        Op::DictHas(t) => typed("dict.has", t),
        Op::DictKeys(t) => typed("dict.keys", t),
        Op::DictValues(t) => typed("dict.values", t),
        Op::DictMerge(t) => typed("dict.merge", t),
        Op::Str(m) => format!("str.{m:?}").to_lowercase(),
        Op::Math(m) => format!("math.{m:?}").to_lowercase(),
        Op::Fs(m) => format!("fs.{m:?}").to_lowercase(),
        Op::Sh(m) => format!("sh.{m:?}").to_lowercase(),
        Op::Env(m) => format!("env.{m:?}").to_lowercase(),
        Op::Path(m) => format!("path.{m:?}").to_lowercase(),
        Op::Random(m) => format!("random.{m:?}").to_lowercase(),
        Op::Json(m) => format!("json.{m:?}").to_lowercase(),
        Op::Http(m, unix) => {
            format!("{}.{m:?}", if *unix { "uhttp" } else { "http" }).to_lowercase()
        }
        Op::UhttpStream(t) => typed("uhttp.stream", t),
        Op::Native(c) => format!("native {} -> {}", c.display, c.ret),
        Op::Fail(m) => format!("fail {m:?}"),
        Op::Await(t) => typed("await", t),
        Op::Join(t) => typed("join", t),
        Op::JoinSettled(t, s) => format!("join.settled.{t} {s}"),
        Op::WaitArray(t) => typed("wait.array", t),
        other => format!("{other:?}").to_lowercase(),
    }
}

impl Display for Term {
    /// Purpose: This function writes a terminator.
    ///
    /// Inputs:
    /// - `f`: the formatter.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Term::Goto(b) => write!(f, "goto bb{}", b.0),
            Term::If(c, t, e) => write!(f, "if {c} then bb{} else bb{}", t.0, e.0),
            Term::Switch { value, cases, default } => {
                let parts: Vec<String> =
                    cases.iter().map(|(v, b)| format!("{v} => bb{}", b.0)).collect();
                write!(f, "switch {value} [{}] else bb{}", parts.join(", "), default.0)
            }
            Term::Return(o) => write!(f, "return {o}"),
            Term::Unreachable => write!(f, "unreachable"),
            Term::Unwind => write!(f, "unwind"),
        }
    }
}
