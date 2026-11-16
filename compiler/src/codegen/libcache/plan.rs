//! Which std library instances a build takes from the cache, which it copies
//! into every module that calls them, and which it compiles with the program.
//!
//! An instance is one library function at one set of argument types, the unit
//! MIR compiles. It can be compiled once per machine only when its code is the
//! same in every program, so it is *cacheable* when:
//!
//! - it is the library's (`Func::library`), named by its instance label;
//! - every local's type and its return type are built in: numbers, bools,
//!   chars, strings, bytes, nil, and optionals, arrays and dicts of those. A
//!   class, a function value, a future, an error, a prompt or a grammar is
//!   the program's: `ShOutput`'s type id and destructor depend on the
//!   program's other classes. The one class allowed is `RuntimeError`, in a
//!   local that is not a parameter, since `raise "text"` builds one; library
//!   code builds it through `jade.new_error`, which the program defines;
//! - it reads no global, reads or writes no field, asks no class's id, starts
//!   no task, and calls nothing through a pointer;
//! - every operation it performs takes built-in types, and none is one only
//!   the program can do: an error's message, tasks, the model, a Rust package;
//! - it calls only cacheable instances. That is a fixpoint over the call
//!   graph: an instance that calls one that is not cacheable is not either.
//!
//! A cacheable instance small enough that inlining matters is *copied*: it
//! gets no object, and every module that calls it, the program's or another
//! library object's, compiles a private copy LLVM can inline, as support code
//! is. Every other cacheable instance gets an *object* of its own, in the
//! cache, under a symbol taken from its label. The rest compile with the
//! program as before.
//!
//! *The size rule.* An instance is copied when it does nothing but
//! arithmetic and comparisons on numbers, bools and chars, branches, copies,
//! optionals of those, hooks that are one instruction or one C math call
//! (`sys::is_one_op`), and calls to instances that are copied too, and when
//! it and everything it calls come to at most [`SMALL`] statements and
//! terminators, each callee counted once. That is `math`'s wrappers, such as
//! `sqrt`, `abs`, `min`, `floor` and `pi`, and `clamp`, which is `min` of a
//! `max`. In 2.0.8, which compiled all library code with the program, LLVM
//! inlined 45 of the 46 instances this rule copies in every example that
//! called them, and kept out of line most of the instances that call a string
//! or file hook, whose bodies are calls themselves. With nothing copied, a
//! loop of `math` calls ran six times slower; see `codegen/README.md` for the
//! measurement.

use sha2::{Digest, Sha256};

use crate::mir::{self, BinOp, Const, Op, Operand, Place, Rvalue, Stmt, Ty};

/// The most statements and terminators a copied instance has, counting
/// each instance it calls once.
pub const SMALL: usize = 20;

/// The class `raise "text"` builds, which library code may hold.
const RUNTIME_ERROR: &str = "RuntimeError";

/// Purpose: This enum says how one function of a program is compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// In the program's module, as in a build without the cache.
    Program,
    /// In an object of its own, compiled once per machine and cached.
    Object,
    /// Small: a private copy in every module that calls it.
    Copied,
}

/// Purpose: This struct holds how each function of a program is compiled.
#[derive(Debug, Clone)]
pub struct LibPlan {
    kinds: Vec<Kind>,
    /// Each cacheable instance's symbol, by function id.
    symbols: Vec<Option<String>>,
}

impl LibPlan {
    /// Purpose: This function compiles every function with the program, for
    /// a build without the cache and for `--emit ir`.
    ///
    /// Inputs:
    /// - `prog`: the program.
    ///
    /// Outputs: the plan.
    pub fn whole(prog: &mir::Program) -> Self {
        LibPlan {
            kinds: vec![Kind::Program; prog.funcs.len()],
            symbols: vec![None; prog.funcs.len()],
        }
    }

    /// Purpose: This function works out which instances are cached, copied
    /// and compiled with the program.
    ///
    /// Inputs:
    /// - `prog`: the program.
    ///
    /// Outputs: the plan.
    pub fn new(prog: &mir::Program) -> Self {
        let mut ok: Vec<bool> = prog.funcs.iter().map(cacheable_alone).collect();
        refine(prog, &mut ok);
        let mut copied: Vec<bool> =
            prog.funcs.iter().zip(&ok).map(|(f, ok)| *ok && small_alone(f)).collect();
        loop {
            refine(prog, &mut copied);
            let too_big: Vec<usize> = (0..copied.len())
                .filter(|i| copied[*i] && inlined_size(prog, *i) > SMALL)
                .collect();
            if too_big.is_empty() {
                break;
            }
            for i in too_big {
                copied[i] = false;
            }
        }
        let kinds = ok
            .iter()
            .zip(&copied)
            .map(|(ok, copied)| match (ok, copied) {
                (false, _) => Kind::Program,
                (true, true) => Kind::Copied,
                (true, false) => Kind::Object,
            })
            .collect();
        let symbols =
            prog.funcs.iter().zip(&ok).map(|(f, ok)| ok.then(|| symbol(&f.name))).collect();
        LibPlan { kinds, symbols }
    }

    /// Purpose: This function returns how a function is compiled.
    pub fn kind(&self, id: usize) -> Kind {
        self.kinds.get(id).copied().unwrap_or(Kind::Program)
    }

    /// Purpose: This function returns a cacheable instance's symbol.
    pub fn symbol(&self, id: usize) -> Option<&str> {
        self.symbols.get(id).and_then(|s| s.as_deref())
    }

    /// Purpose: This function lists the instances that get an object.
    pub fn objects(&self) -> Vec<usize> {
        (0..self.kinds.len()).filter(|i| self.kinds[*i] == Kind::Object).collect()
    }

    /// Purpose: This function compiles some instances with the program after
    /// all, and with them every cacheable instance that calls one.
    ///
    /// Inputs:
    /// - `prog`: the program.
    /// - `ids`: the instances whose modules turned out to depend on the
    ///   program.
    ///
    /// Outputs: none.
    pub fn demote(&mut self, prog: &mir::Program, ids: &[usize]) {
        let mut ok: Vec<bool> = self.kinds.iter().map(|k| *k != Kind::Program).collect();
        for id in ids {
            if let Some(o) = ok.get_mut(*id) {
                *o = false;
            }
        }
        refine(prog, &mut ok);
        for (i, ok) in ok.iter().enumerate() {
            if !ok {
                self.kinds[i] = Kind::Program;
                self.symbols[i] = None;
            }
        }
    }
}

/// Purpose: This function returns the symbol a cacheable instance's code
/// has: `jade.lib.` and 16 hex digits of its label's SHA-256. The library's
/// namespace ids are fixed, so a label, and its symbol, is the same in every
/// program.
///
/// Inputs:
/// - `label`: the instance's label, such as `join$4294966271(str, str)`.
///
/// Outputs: the symbol.
pub fn symbol(label: &str) -> String {
    let hash = Sha256::digest(label.as_bytes());
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("jade.lib.{hex}")
}

/// Purpose: This function drops, until nothing changes, every cacheable
/// instance that calls one that is not.
///
/// Inputs:
/// - `prog`: the program.
/// - `ok`: which instances are cacheable so far, by function id.
///
/// Outputs: none.
fn refine(prog: &mir::Program, ok: &mut [bool]) {
    loop {
        let mut changed = false;
        for (i, f) in prog.funcs.iter().enumerate() {
            if ok[i] && callees(f).iter().any(|c| !ok.get(*c).copied().unwrap_or(false)) {
                ok[i] = false;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Purpose: This function lists the functions a function calls directly.
///
/// Inputs:
/// - `f`: the function.
///
/// Outputs: their ids, each once.
pub fn callees(f: &mir::Func) -> Vec<usize> {
    let mut out = Vec::new();
    for b in &f.blocks {
        for s in &b.stmts {
            let rv = match s {
                Stmt::Assign(_, rv) | Stmt::Eval(rv) => rv,
                _ => continue,
            };
            if let Rvalue::Call(id, _) = rv
                && !out.contains(&(id.0 as usize))
            {
                out.push(id.0 as usize);
            }
        }
    }
    out
}

/// Purpose: This function says whether a type is built in, so its layout and
/// code are the same in every program.
pub fn portable(t: &Ty) -> bool {
    match t {
        Ty::Int
        | Ty::Float
        | Ty::Sized(_)
        | Ty::Bool
        | Ty::Char
        | Ty::Str
        | Ty::Bytes
        | Ty::Nil => true,
        Ty::Opt(inner) | Ty::Array(inner) | Ty::Map(inner) => portable(inner),
        _ => false,
    }
}

/// Purpose: This function says whether a function could be cached, looking
/// at it alone: everything but what it calls.
///
/// Inputs:
/// - `f`: the function.
///
/// Outputs: true when it is the library's and touches only built-in types and
/// operations every program compiles the same way.
fn cacheable_alone(f: &mir::Func) -> bool {
    if !f.library || !portable(&f.ret) {
        return false;
    }
    let locals_ok = f.locals.iter().enumerate().all(|(i, l)| {
        portable(&l.ty) || ((i as u32) >= f.params && l.ty == Ty::Struct(RUNTIME_ERROR.to_string()))
    });
    locals_ok
        && f.blocks.iter().all(|b| {
            b.stmts.iter().all(stmt_ok)
                && match &b.term {
                    mir::Term::If(c, ..) => operand_ok(c),
                    mir::Term::Switch { value, .. } => operand_ok(value),
                    mir::Term::Return(op) => operand_ok(op),
                    mir::Term::Goto(_) | mir::Term::Unreachable | mir::Term::Unwind => true,
                }
        })
}

/// Purpose: This function says whether a statement is the same in every
/// program.
fn stmt_ok(s: &Stmt) -> bool {
    match s {
        Stmt::Assign(Place::Local(_), rv) | Stmt::Eval(rv) => rvalue_ok(rv),
        Stmt::Assign(Place::Global(_), _) => false,
        Stmt::SetField { .. } => false,
        Stmt::DictSet { dict, key, value } => {
            matches!(dict, Place::Local(_)) && operand_ok(key) && operand_ok(value)
        }
        Stmt::Drop(p) => matches!(p, Place::Local(_)),
        Stmt::Raise(op) => operand_ok(op),
    }
}

/// Purpose: This function says whether an operand is the same in every
/// program: not a global, and no constant of the program's types.
fn operand_ok(op: &Operand) -> bool {
    match op {
        Operand::Copy(Place::Global(_)) => false,
        Operand::Copy(Place::Local(_)) | Operand::Move(_) => true,
        Operand::Const(Const::Nil(t) | Const::Static(t)) => portable(t),
        Operand::Const(_) => true,
    }
}

/// Purpose: This function says whether an rvalue is the same in every
/// program.
fn rvalue_ok(rv: &Rvalue) -> bool {
    let all = |ops: &[Operand]| ops.iter().all(operand_ok);
    match rv {
        Rvalue::Use(a) | Rvalue::Un(_, a) | Rvalue::Unwrap(a) | Rvalue::IsNil(a) => operand_ok(a),
        Rvalue::IntToFloat(a) => operand_ok(a),
        Rvalue::Bin(op, a, b) => {
            let typed = match op {
                BinOp::Cmp(_, t) => portable(t),
                BinOp::Arith(..) | BinOp::Bit(..) | BinOp::Concat => true,
            };
            typed && operand_ok(a) && operand_ok(b)
        }
        Rvalue::Call(_, args) => all(args),
        Rvalue::Op(op, args) => op_ok(op) && all(args),
        Rvalue::Array(t, els) => portable(t) && all(els),
        Rvalue::Dict(t, pairs) => {
            portable(t) && pairs.iter().all(|(k, v)| operand_ok(k) && operand_ok(v))
        }
        Rvalue::Struct(name, fields) => name == RUNTIME_ERROR && fields.len() == 1 && all(fields),
        Rvalue::Wrap(t, a) => portable(t) && operand_ok(a),
        Rvalue::ErrPending => true,
        Rvalue::Field { .. }
        | Rvalue::TypeId(_)
        | Rvalue::IsA(..)
        | Rvalue::Spawn(..)
        | Rvalue::FnAddr(_)
        | Rvalue::CallIndirect(..)
        | Rvalue::TakeErr => false,
    }
}

/// Purpose: This function says whether a support operation compiles the same
/// way in every program.
fn op_ok(op: &Op) -> bool {
    match op {
        Op::ToStr(t)
        | Op::ToInt(t)
        | Op::ToFloat(t)
        | Op::ToBool(t)
        | Op::ToChar(t)
        | Op::ToDict(t)
        | Op::ArrayGet(t)
        | Op::ArraySet(t)
        | Op::ArrayPush(t)
        | Op::ArrayPop(t)
        | Op::ArrayContains(t)
        | Op::ArraySort(t)
        | Op::ArrayReverse(t)
        | Op::ArraySorted(t)
        | Op::ArrayReversed(t)
        | Op::ArrayJoin(t)
        | Op::DictGet(t)
        | Op::DictGetOpt(t)
        | Op::DictHas(t)
        | Op::DictKeys(t)
        | Op::DictValues(t)
        | Op::DictMerge(t) => portable(t),
        Op::Convert { to, from } => portable(to) && portable(from),
        Op::Sys(h) => portable(&h.result()) && h.params().iter().all(portable),
        Op::Print
        | Op::Input
        | Op::StrCat
        | Op::CharToStr
        | Op::StrLen
        | Op::StrIndex
        | Op::StrChars
        | Op::Str(_)
        | Op::ArrayLen
        | Op::DictLen
        | Op::Fail(_)
        | Op::BytesGet
        | Op::BytesSet
        | Op::BytesLen
        | Op::BytesDecode
        | Op::BytesSlice
        | Op::StrEncode
        | Op::BytesZeros
        | Op::BytesFromInts
        | Op::BytesConcat
        | Op::BytesInts => true,
        Op::ErrorMessage
        | Op::Await(_)
        | Op::Join(_)
        | Op::JoinSettled(..)
        | Op::Wait
        | Op::WaitArray(_)
        | Op::Ready
        | Op::Cancel
        | Op::Cancelled
        | Op::MaxTasks
        | Op::SetMaxTasks
        | Op::Infer(_)
        | Op::Native(_) => false,
    }
}

/// Purpose: This function says whether a cacheable instance could be copied
/// into its callers, looking at it alone: every type a number, bool, char or
/// nil, or an optional of one; nothing but arithmetic, comparisons, copies,
/// branches, one-instruction hooks and calls, which [`LibPlan::new`] then
/// requires to be copied too; and at most [`SMALL`] statements and
/// terminators.
fn small_alone(f: &mir::Func) -> bool {
    let scalar = |t: &Ty| {
        let base = |t: &Ty| matches!(t, Ty::Int | Ty::Float | Ty::Sized(_) | Ty::Bool | Ty::Char);
        base(t) || *t == Ty::Nil || matches!(t, Ty::Opt(inner) if base(inner))
    };
    if !scalar(&f.ret) || !f.locals.iter().all(|l| scalar(&l.ty)) {
        return false;
    }
    size(f) <= SMALL
        && f.blocks.iter().all(|b| {
            b.stmts.iter().all(|s| match s {
                Stmt::Assign(_, rv) | Stmt::Eval(rv) => match rv {
                    Rvalue::Use(_)
                    | Rvalue::Un(..)
                    | Rvalue::Unwrap(_)
                    | Rvalue::IsNil(_)
                    | Rvalue::Wrap(..)
                    | Rvalue::IntToFloat(_)
                    | Rvalue::ErrPending
                    | Rvalue::Call(..) => true,
                    Rvalue::Bin(BinOp::Concat, ..) => false,
                    Rvalue::Bin(BinOp::Cmp(_, t), ..) => scalar(t),
                    Rvalue::Bin(..) => true,
                    Rvalue::Op(Op::Sys(h), _) => crate::codegen::sys::is_one_op(*h),
                    _ => false,
                },
                Stmt::Drop(_) => true,
                _ => false,
            })
        })
}

/// Purpose: This function counts a function's statements and terminators.
fn size(f: &mir::Func) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}

/// Purpose: This function counts what a function grows to once every
/// function it calls, and every function those call, is inlined into it:
/// its own size and each callee's once.
///
/// Inputs:
/// - `prog`: the program.
/// - `id`: the function.
///
/// Outputs: the total.
fn inlined_size(prog: &mir::Program, id: usize) -> usize {
    let mut seen = vec![id];
    let mut todo = vec![id];
    let mut total = 0;
    while let Some(i) = todo.pop() {
        let Some(f) = prog.funcs.get(i) else { continue };
        total += size(f);
        for c in callees(f) {
            if !seen.contains(&c) {
                seen.push(c);
                todo.push(c);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_is_sixteen_hex_digits_of_the_labels_hash() {
        let s = symbol("join$4294966271(str, str)");
        assert!(s.starts_with("jade.lib."));
        assert_eq!(s.len(), "jade.lib.".len() + 16);
        assert_ne!(s, symbol("join$4294966271(str?, str)"));
        assert_eq!(s, symbol("join$4294966271(str, str)"));
    }

    #[test]
    fn only_built_in_types_are_portable() {
        assert!(portable(&Ty::Map(Box::new(Ty::Array(Box::new(Ty::opt(Ty::Str)))))));
        assert!(!portable(&Ty::Struct("ShOutput".into())));
        assert!(!portable(&Ty::Array(Box::new(Ty::Struct("Card".into())))));
        assert!(!portable(&Ty::Future(Box::new(Ty::Int))));
        assert!(!portable(&Ty::Error));
    }
}
