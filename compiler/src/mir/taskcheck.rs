//! Rejects shared mutation across task boundaries, over MIR.
//!
//! Jade tasks run on worker threads and share one heap: a task is handed the
//! very objects its spawner passed, not copies, and nothing locks a collection.
//! Two tasks pushing to one array is a data race. There are three ways to close
//! that: lock every collection, a cost every program pays; deep-copy task
//! arguments, which silently changes what `async` means; or refuse to compile
//! the programs that would race. This pass is the third. Nothing is locked,
//! nothing is copied, and the programs that survive were already correct.
//!
//! It is v1's rule, which ran over bytecode in `compiler/taskcheck.rs` until the
//! bytecode went in phase 6 of `design/v2-static-values.md`. MIR suits it
//! better: every call names the instance it runs, every write to a global or
//! through a reference is its own statement, and the types say which values
//! live on the heap and so can be shared at all.
//!
//! ## What is rejected
//!
//! For any function a task runs, transitively through what it calls:
//!
//!  * *writing a global*, or writing into the collection or object a global
//!    holds;
//!  * *writing through a value that arrived as a parameter*: setting an index,
//!    a key or a field of it, or calling a method that changes it in place,
//!    such as `push`.
//!
//! And for the spawner, while a task it started is still running:
//!
//!  * *assigning a global the task reads*;
//!  * *changing a collection it handed the task*.
//!
//! The window closes at the `await` or `join` that finishes the task.
//!
//! ## What is allowed
//!
//! Reading globals, rebinding a local or a parameter, and changing anything the
//! task built itself: a new array, dict or object is shared with nobody, unless
//! it was built holding something that is.
//!
//! ## Precision
//!
//! Each heap local carries two marks, *from a parameter* and *from a global*,
//! and a value built from or read out of a marked value carries its marks.
//! A call whose callee is not known, through a function value of a joined type,
//! returns a value marked as its arguments are, so nothing is laundered
//! through it. False positives are possible and false negatives are not, which
//! is the right bias: a false positive is an error the author can see and work
//! around, and a false negative is a race nobody sees.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::{Func, FuncId, Op, Operand, Place, Program, Rvalue, Stmt, Term, is_heap};
use crate::frontend::error::{JadeError, Span};

/// Purpose: This struct holds where a value may have come from.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Marks {
    /// It may be, or be part of, something a parameter brought in.
    param: bool,
    /// It may be, or be part of, something a global holds.
    global: bool,
}

impl Marks {
    /// Purpose: This function joins two sets of marks.
    fn or(self, o: Marks) -> Marks {
        Marks { param: self.param || o.param, global: self.global || o.global }
    }

    /// Purpose: This function says whether a value is shared at all.
    fn any(self) -> bool {
        self.param || self.global
    }
}

/// Purpose: This struct holds what a function does to state it does not own,
/// anywhere in its call tree.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Effects {
    /// It writes a global, or into what a global holds.
    writes_global: bool,
    /// It writes into something a parameter brought in.
    mutates_param: bool,
    /// Its result may be something a parameter brought in.
    returns_param: bool,
    /// Its result may be something a global holds.
    returns_global: bool,
    /// The globals it reads, by display name.
    reads: BTreeSet<String>,
}

impl Effects {
    /// Purpose: This function says whether a task may run this function.
    fn is_clean(&self) -> bool {
        !self.writes_global && !self.mutates_param
    }
}

/// Purpose: This struct holds the analysis of one function: its effects, and
/// the first place each kind of effect happens, for the message.
#[derive(Clone, Default)]
struct Analysis {
    effects: Effects,
    /// What it does and where, first first.
    sites: Vec<(String, Span)>,
}

/// Purpose: This function checks a program for tasks that race on shared
/// state.
///
/// Inputs:
/// - `p`: the program.
///
/// Outputs: `Ok(())`, or the first race found, naming the operation to change.
pub fn check(p: &Program) -> Result<(), JadeError> {
    let n = p.funcs.len();
    // Effects only ever grow, so iterating to a fixed point terminates; the
    // bound guards the reasoning, not a real program.
    let mut an = vec![Analysis::default(); n];
    for _ in 0..n + 2 {
        let mut changed = false;
        for i in 0..n {
            let next = analyze(p, &p.funcs[i], &an);
            if next.effects != an[i].effects {
                changed = true;
            }
            an[i] = next;
        }
        if !changed {
            break;
        }
    }
    for f in &p.funcs {
        spawner(p, f, &an)?;
    }
    Ok(())
}

/// Purpose: This function gives a function's name as its source writes it.
fn fn_name(f: &Func) -> String {
    let base = f.name.split('(').next().unwrap_or(&f.name);
    crate::compiler::infer::display_type_name(base).to_string()
}

/// Purpose: This function gives a global's name as its source writes it.
fn global_name(p: &Program, g: super::GlobalId) -> String {
    crate::compiler::infer::display_type_name(&p.globals[g.0 as usize].name).to_string()
}

/// Purpose: This function names a field of a struct, for messages.
fn field_name(p: &Program, strukt: &str, index: u32) -> String {
    p.structs
        .iter()
        .find(|s| s.name == strukt)
        .and_then(|s| s.fields.get(index as usize))
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "a field".to_string())
}

/// Purpose: This function names an operation that changes a collection in
/// place, for messages, or says it is not one.
///
/// Inputs:
/// - `op`: the operation.
///
/// Outputs: the method's name, `[]` for an index write, or `None`.
fn mutation(op: &Op) -> Option<&'static str> {
    Some(match op {
        Op::ArraySet(_) | Op::BytesSet => "[]",
        Op::ArrayPush(_) => "push",
        Op::ArrayPop(_) => "pop",
        Op::ArraySort(_) => "sort",
        Op::ArrayReverse(_) => "reverse",
        _ => return None,
    })
}

/// Purpose: This function says whether an operation hands back part of its
/// first operand rather than something new.
fn reads_into(op: &Op) -> bool {
    matches!(op, Op::ArrayGet(_) | Op::ArrayPop(_) | Op::DictGet(_) | Op::DictGetOpt(_))
}

/// Purpose: This struct holds the marks of one function's locals.
struct Taint<'a> {
    p: &'a Program,
    f: &'a Func,
    locals: Vec<Marks>,
    /// The function each local holds, when it holds a known one.
    fns: HashMap<u32, FuncId>,
    reads: BTreeSet<String>,
}

impl<'a> Taint<'a> {
    /// Purpose: This function gives an operand's marks, noting a global read.
    fn of(&mut self, op: &Operand) -> Marks {
        match op {
            Operand::Copy(Place::Local(l)) | Operand::Move(l) => self.locals[l.0 as usize],
            Operand::Copy(Place::Global(g)) => {
                self.reads.insert(global_name(self.p, *g));
                let heap = is_heap(&self.p.globals[g.0 as usize].ty);
                Marks { param: false, global: heap }
            }
            Operand::Const(_) => Marks::default(),
        }
    }

    /// Purpose: This function gives the function an operand holds, when known.
    fn func(&self, op: &Operand) -> Option<FuncId> {
        match op {
            Operand::Copy(Place::Local(l)) | Operand::Move(l) => self.fns.get(&l.0).copied(),
            _ => None,
        }
    }

    /// Purpose: This function marks a local, only ever adding marks.
    ///
    /// Outputs: whether its marks grew.
    fn mark(&mut self, l: u32, m: Marks) -> bool {
        let heap = is_heap(&self.f.locals[l as usize].ty);
        let m = if heap { m } else { Marks::default() };
        let old = self.locals[l as usize];
        let new = old.or(m);
        self.locals[l as usize] = new;
        new != old
    }
}

/// Purpose: This function works out one function's effects, given what is
/// known so far about the functions it calls.
///
/// Inputs:
/// - `p`: the program.
/// - `f`: the function.
/// - `an`: every function's analysis so far.
///
/// Outputs: the function's analysis.
fn analyze(p: &Program, f: &Func, an: &[Analysis]) -> Analysis {
    let mut t = Taint {
        p,
        f,
        locals: vec![Marks::default(); f.locals.len()],
        fns: HashMap::new(),
        reads: BTreeSet::new(),
    };
    for l in 0..f.params {
        t.mark(l, Marks { param: true, global: false });
    }

    // Marks only grow, so passes over the blocks until none grows converge.
    let mut out = Analysis::default();
    for _ in 0..16 {
        let mut grew = false;
        let mut e = Effects::default();
        let mut sites: Vec<(String, Span)> = Vec::new();
        for b in &f.blocks {
            for (i, s) in b.stmts.iter().enumerate() {
                let span = b.spans.get(i).copied().unwrap_or(Span { line: 0, col: 0 });
                grew |= step(&mut t, s, span, an, &mut e, &mut sites);
            }
            if let Term::Return(op) = &b.term {
                let m = t.of(op);
                e.returns_param |= m.param;
                e.returns_global |= m.global;
            }
        }
        e.reads.extend(t.reads.iter().cloned());
        out = Analysis { effects: e, sites };
        if !grew {
            break;
        }
    }
    out
}

/// Purpose: This function applies one statement to a function's marks and
/// effects.
///
/// Inputs:
/// - `t`: the marks.
/// - `s`: the statement, and `span`, where it is.
/// - `an`: every function's analysis so far.
/// - `e`, `sites`: the effects and their sites, added to.
///
/// Outputs: whether any local's marks grew.
fn step(
    t: &mut Taint<'_>,
    s: &Stmt,
    span: Span,
    an: &[Analysis],
    e: &mut Effects,
    sites: &mut Vec<(String, Span)>,
) -> bool {
    let mut site = |e: &mut Effects, global: bool, param: bool, what: String| {
        if global {
            e.writes_global = true;
        }
        if param {
            e.mutates_param = true;
        }
        if global || param {
            sites.push((what, span));
        }
    };
    match s {
        Stmt::Assign(place, rv) => {
            let m = rvalue(t, rv, an, e, &mut site);
            match place {
                Place::Local(l) => {
                    if let Rvalue::FnAddr(fid) = rv {
                        t.fns.insert(l.0, *fid);
                    } else if let Rvalue::Use(op) = rv
                        && let Some(fid) = t.func(op)
                    {
                        t.fns.insert(l.0, fid);
                    }
                    return t.mark(l.0, m);
                }
                Place::Global(g) => {
                    let name = global_name(t.p, *g);
                    site(e, true, false, format!("writes to the global `{name}`"));
                }
            }
        }
        Stmt::Eval(rv) => {
            rvalue(t, rv, an, e, &mut site);
        }
        Stmt::SetField { object, strukt, index, .. } => {
            let m = t.of(object);
            let field = field_name(t.p, strukt, *index);
            site(
                e,
                m.global,
                m.param,
                format!("assigns to the field `{field}` of a shared object"),
            );
        }
        Stmt::DictSet { dict, .. } => match dict {
            Place::Global(g) => {
                let name = global_name(t.p, *g);
                site(e, true, false, format!("assigns into the global `{name}`"));
            }
            Place::Local(l) => {
                let m = t.locals[l.0 as usize];
                site(e, m.global, m.param, "assigns into a shared collection".to_string());
            }
        },
        Stmt::Drop(_) | Stmt::Raise(_) => {}
    }
    false
}

/// Purpose: This function gives an rvalue's marks, recording what it does.
///
/// Inputs:
/// - `t`: the marks.
/// - `rv`: the rvalue.
/// - `an`: every function's analysis so far.
/// - `e`: the effects, added to.
/// - `site`: records an effect and where.
///
/// Outputs: the marks of the value it computes.
fn rvalue(
    t: &mut Taint<'_>,
    rv: &Rvalue,
    an: &[Analysis],
    e: &mut Effects,
    site: &mut impl FnMut(&mut Effects, bool, bool, String),
) -> Marks {
    let all = |t: &mut Taint<'_>, ops: &[Operand]| {
        ops.iter().fold(Marks::default(), |m, o| m.or(t.of(o)))
    };
    match rv {
        Rvalue::Use(op) | Rvalue::Wrap(_, op) | Rvalue::Unwrap(op) => t.of(op),
        Rvalue::Field { object, .. } => t.of(object),
        // Something new, carrying whatever it was built from.
        Rvalue::Array(_, items) | Rvalue::Struct(_, items) => all(t, items),
        Rvalue::Dict(_, pairs) => {
            pairs.iter().fold(Marks::default(), |m, (k, v)| m.or(t.of(k)).or(t.of(v)))
        }
        Rvalue::Op(op, args) => {
            let first = args.first().map(|a| t.of(a)).unwrap_or_default();
            for a in args.iter().skip(1) {
                t.of(a);
            }
            if let Some(name) = mutation(op) {
                let what = if name == "[]" {
                    if first.global && !first.param && is_global(&args[0]) {
                        "assigns into the global collection".to_string()
                    } else {
                        "assigns into a shared collection".to_string()
                    }
                } else {
                    format!("calls `{name}()` on a shared collection")
                };
                site(e, first.global, first.param, what);
            }
            if reads_into(op) { first } else { Marks::default() }
        }
        Rvalue::Call(fid, args) => call(t, *fid, args, an, e, site),
        Rvalue::CallIndirect(callee, args) => match t.func(callee) {
            Some(fid) => call(t, fid, args, an, e, site),
            // Nothing is known of the callee, so its result may be any argument.
            None => all(t, args),
        },
        Rvalue::Spawn(_, args) => {
            all(t, args);
            Marks::default()
        }
        Rvalue::Bin(_, a, b) => {
            t.of(a);
            t.of(b);
            Marks::default()
        }
        Rvalue::Un(_, a)
        | Rvalue::IsNil(a)
        | Rvalue::IntToFloat(a)
        | Rvalue::TypeId(a)
        | Rvalue::IsA(a, _) => {
            t.of(a);
            Marks::default()
        }
        Rvalue::FnAddr(_) | Rvalue::ErrPending | Rvalue::TakeErr => Marks::default(),
    }
}

/// Purpose: This function says whether an operand reads a global directly.
fn is_global(op: &Operand) -> bool {
    matches!(op, Operand::Copy(Place::Global(_)))
}

/// Purpose: This function applies a call to a known function: the callee's
/// effects become the caller's, where the caller hands it something shared.
///
/// Inputs:
/// - `t`: the caller's marks.
/// - `fid`: the callee, and `args`, what it is passed.
/// - `an`: every function's analysis so far.
/// - `e`: the caller's effects, added to.
/// - `site`: records an effect and where.
///
/// Outputs: the marks of the call's result.
fn call(
    t: &mut Taint<'_>,
    fid: FuncId,
    args: &[Operand],
    an: &[Analysis],
    e: &mut Effects,
    site: &mut impl FnMut(&mut Effects, bool, bool, String),
) -> Marks {
    let passed = args.iter().fold(Marks::default(), |m, o| m.or(t.of(o)));
    let callee = &an[fid.0 as usize].effects;
    let name = fn_name(&t.p.funcs[fid.0 as usize]);
    e.reads.extend(callee.reads.iter().cloned());
    if callee.writes_global {
        site(e, true, false, format!("calls `{name}`, which writes a global"));
    }
    // A callee that changes its parameter changes whatever it was handed.
    if callee.mutates_param && passed.any() {
        site(
            e,
            passed.global,
            passed.param,
            format!("passes a shared value to `{name}`, which mutates it"),
        );
    }
    let mut m = Marks::default();
    if callee.returns_param {
        m = m.or(passed);
    }
    if callee.returns_global {
        m.global = true;
    }
    m
}

/// Purpose: This function checks what one function does while tasks it
/// started are still running, and what it hands the tasks it starts.
///
/// Inputs:
/// - `p`: the program.
/// - `f`: the function.
/// - `an`: every function's analysis.
///
/// Outputs: `Ok(())`, or the first race found.
fn spawner(p: &Program, f: &Func, an: &[Analysis]) -> Result<(), JadeError> {
    let violation = |task: FuncId, what: String, span: Span| JadeError::SharedMutation {
        task: fn_name(&p.funcs[task.0 as usize]),
        what,
        span,
    };
    // Tasks started and not yet finished, and which place holds each future.
    let mut live: Vec<FuncId> = Vec::new();
    let mut futures: HashMap<Place, FuncId> = HashMap::new();
    // What a running task was handed, and the task.
    let mut handed: HashSet<Place> = HashSet::new();
    let mut handed_to: Option<FuncId> = None;
    let mut fns: HashMap<u32, FuncId> = HashMap::new();

    let place_of = |op: &Operand| match op {
        Operand::Copy(pl) => Some(*pl),
        Operand::Move(l) => Some(Place::Local(*l)),
        Operand::Const(_) => None,
    };
    let finish = |live: &mut Vec<FuncId>,
                  futures: &HashMap<Place, FuncId>,
                  handed: &mut HashSet<Place>,
                  ops: &[Operand]| {
        for op in ops {
            if let Some(task) = place_of(op).and_then(|pl| futures.get(&pl)) {
                live.retain(|x| x != task);
            }
        }
        if live.is_empty() {
            handed.clear();
        }
    };

    for b in &f.blocks {
        for (i, s) in b.stmts.iter().enumerate() {
            let span = b.spans.get(i).copied().unwrap_or(Span { line: 0, col: 0 });
            // The spawner's side: a write while a task is running.
            if !live.is_empty() {
                if let Stmt::Assign(Place::Global(g), _) = s {
                    let name = global_name(p, *g);
                    if let Some(&task) =
                        live.iter().find(|t| an[t.0 as usize].effects.reads.contains(&name))
                    {
                        return Err(violation(
                            task,
                            format!(
                                "reads the global `{name}`, which is assigned here while the \
                                 task is still running"
                            ),
                            span,
                        ));
                    }
                }
                let target = match s {
                    Stmt::SetField { object, .. } => place_of(object).map(|pl| (pl, None)),
                    Stmt::DictSet { dict, .. } => Some((*dict, None)),
                    Stmt::Assign(_, Rvalue::Op(op, args)) | Stmt::Eval(Rvalue::Op(op, args)) => {
                        mutation(op)
                            .and_then(|m| Some((place_of(args.first()?)?, Some(m))))
                            .map(|(pl, m)| (pl, m.filter(|m| *m != "[]")))
                    }
                    _ => None,
                };
                if let Some((pl, method)) = target
                    && handed.contains(&pl)
                    && let Some(task) = handed_to
                {
                    let what = match method {
                        Some(m) => format!(
                            "is handed a collection the spawner then calls `{m}()` on, here, \
                             while the task is still running"
                        ),
                        None => "is handed a collection the spawner then assigns into, here, \
                                 while the task is still running"
                            .to_string(),
                    };
                    return Err(violation(task, what, span));
                }
            }

            match s {
                Stmt::Assign(dest, rv) => {
                    match rv {
                        Rvalue::Spawn(task, args) => {
                            // The task, and any function handed to it, must be
                            // clean: a callback is the other way a task reaches
                            // shared state.
                            let mut candidates = vec![*task];
                            for a in args {
                                if let Operand::Copy(Place::Local(l)) | Operand::Move(l) = a
                                    && let Some(fid) = fns.get(&l.0)
                                {
                                    candidates.push(*fid);
                                }
                            }
                            if let Some(&bad) =
                                candidates.iter().find(|c| !an[c.0 as usize].effects.is_clean())
                            {
                                let (what, at) =
                                    an[bad.0 as usize].sites.first().cloned().unwrap_or_else(
                                        || ("mutates shared state".to_string(), span),
                                    );
                                return Err(violation(bad, what, at));
                            }
                            if !live.contains(task) {
                                live.push(*task);
                            }
                            futures.insert(*dest, *task);
                            for a in args {
                                if let Some(pl) = place_of(a) {
                                    handed.insert(pl);
                                }
                            }
                            handed_to = Some(*task);
                        }
                        Rvalue::Op(Op::Await(_), ops)
                        | Rvalue::Op(Op::Join(_), ops)
                        | Rvalue::Op(Op::JoinSettled(_, _), ops) => {
                            finish(&mut live, &futures, &mut handed, ops);
                        }
                        Rvalue::FnAddr(fid) => {
                            if let Place::Local(l) = dest {
                                fns.insert(l.0, *fid);
                            }
                        }
                        Rvalue::Use(op) => {
                            if let Some(pl) = place_of(op) {
                                // A future or a handed value keeps its meaning
                                // through a copy.
                                if let Some(task) = futures.get(&pl).copied() {
                                    futures.insert(*dest, task);
                                }
                                if handed.contains(&pl) {
                                    handed.insert(*dest);
                                }
                                if let (Place::Local(d), Place::Local(src)) = (dest, pl)
                                    && let Some(fid) = fns.get(&src.0).copied()
                                {
                                    fns.insert(d.0, fid);
                                }
                            }
                        }
                        other => {
                            // Part of a handed value is handed too.
                            let src = match other {
                                Rvalue::Field { object, .. } => Some(object),
                                Rvalue::Op(op, args) if reads_into(op) => args.first(),
                                _ => None,
                            };
                            if src.and_then(place_of).is_some_and(|pl| handed.contains(&pl)) {
                                handed.insert(*dest);
                            }
                        }
                    }
                }
                Stmt::Eval(Rvalue::Op(Op::Await(_), ops))
                | Stmt::Eval(Rvalue::Op(Op::Join(_), ops))
                | Stmt::Eval(Rvalue::Op(Op::JoinSettled(_, _), ops)) => {
                    finish(&mut live, &futures, &mut handed, ops);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
