//! Lowering the checked program into MIR.
//!
//! The checker's answer (`compiler::infer::Typed`) says, for every instance it
//! reached, the type of every expression and slot and the target of every call,
//! keyed by the address of the syntax node. This walks each instance's body
//! once with those answers in hand and writes MIR: locals for variables and
//! temporaries, blocks for control flow, and an explicit conversion wherever a
//! value meets a slot of a wider type, such as an `int` stored in an `int?`.
//!
//! Anything the static backend does not compile yet stops here with
//! `JadeError::NotYetStatic`, naming the feature. The phases that add them are
//! listed in `design/v2-static-values.md`.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::compiler::infer::{FnKey, InstKey, Target, Typed};
use crate::compiler::tir::{TExpr, TExprKind, TFStrPart, TProgram, TStmt};
use crate::frontend::ast::{BinOpKind, UnaryOpKind};
use crate::frontend::error::{JadeError, Span};

type Result<T> = std::result::Result<T, JadeError>;

/// Purpose: This struct holds one function definition as the lowering reads it.
#[derive(Clone, Copy)]
pub(super) struct FnDef<'a> {
    pub params: &'a [(String, Option<TExpr>)],
    pub body: &'a [TStmt],
    pub is_async: bool,
    pub span: Span,
}

/// Purpose: This struct holds what every function's lowering shares.
pub(super) struct Lowerer<'a> {
    top: &'a [TStmt],
    fns: HashMap<&'a str, FnDef<'a>>,
    methods: HashMap<(&'a str, &'a str), FnDef<'a>>,
    func_ids: HashMap<InstKey, FuncId>,
    /// Each instance's parameter and return types.
    sigs: HashMap<InstKey, (Vec<Ty>, Ty)>,
    exprs: HashMap<InstKey, HashMap<usize, Ty>>,
    targets: HashMap<InstKey, HashMap<usize, Target>>,
    slots: HashMap<InstKey, HashMap<usize, Ty>>,
    pub(super) globals: HashMap<String, (GlobalId, Ty)>,
    pub(super) struct_ids: HashMap<String, u32>,
    pub(super) structs: Vec<StructDef>,
    pub(super) packages: HashSet<String>,
}

/// Purpose: This function lowers a whole checked program into MIR.
///
/// Inputs:
/// - `program`: the merged program the checker checked.
/// - `typed`: the checker's answer for it.
///
/// Outputs: the MIR program, or the first feature the static backend cannot
/// compile yet.
pub fn lower_program(program: &TProgram, typed: &Typed) -> Result<Program> {
    let mut l = Lowerer::new(&program.stmts, typed);
    l.collect(&program.stmts);

    let mut keys: Vec<&(InstKey, Vec<Ty>, Ty)> = typed.instances.iter().collect();
    keys.sort_by_key(|(k, _, _)| (k.func != FnKey::Main, inst_label(k)));
    for (i, (k, params, ret)) in keys.iter().enumerate() {
        l.func_ids.insert(k.clone(), FuncId(i as u32));
        l.sigs.insert(k.clone(), (params.iter().map(norm).collect(), norm(ret)));
    }

    let mut funcs = Vec::with_capacity(keys.len());
    for (k, _, _) in &keys {
        funcs.push(l.lower_instance(k)?);
    }
    let mut globals: Vec<(GlobalId, Global)> = l
        .globals
        .iter()
        .map(|(n, (id, t))| (*id, Global { name: n.clone(), ty: t.clone() }))
        .collect();
    globals.sort_by_key(|(id, _)| id.0);
    Ok(Program {
        funcs,
        globals: globals.into_iter().map(|(_, g)| g).collect(),
        structs: l.structs,
        main: FuncId(0),
    })
}

/// Purpose: This function names an instance readably and uniquely.
///
/// Inputs:
/// - `k`: the instance.
///
/// Outputs: a label such as `add(int, int)` or `Point.norm(Point)`.
fn inst_label(k: &InstKey) -> String {
    let base = match &k.func {
        FnKey::Main => "main".to_string(),
        FnKey::Named(n) => n.clone(),
        FnKey::Method(t, n) => format!("{t}.{n}"),
    };
    let args: Vec<String> = k.args.iter().map(|a| norm(a).to_string()).collect();
    format!("{base}({})", args.join(", "))
}

/// Purpose: This function replaces any slot the checker left empty with `nil`.
///
/// Inputs:
/// - `t`: a resolved type.
///
/// Outputs: the same type with no `Var` in it.
pub(super) fn norm(t: &Ty) -> Ty {
    match t {
        Ty::Var(_) => Ty::Nil,
        Ty::Opt(a) => Ty::opt(norm(a)),
        Ty::Array(a) => Ty::Array(Box::new(norm(a))),
        Ty::Map(a) => Ty::Map(Box::new(norm(a))),
        Ty::Future(a) => Ty::Future(Box::new(norm(a))),
        Ty::Stream(a) => Ty::Stream(Box::new(norm(a))),
        other => other.clone(),
    }
}

/// Purpose: This function gives a syntax node its identity, as the checker does.
///
/// Inputs:
/// - `x`: any node.
///
/// Outputs: its address.
fn addr<T>(x: &T) -> usize {
    x as *const T as usize
}

/// Purpose: This function makes the error for a feature not compiled yet.
///
/// Inputs:
/// - `what`: the feature, as a noun phrase.
/// - `span`: where it is.
///
/// Outputs: the error.
pub(super) fn not_yet(what: impl Into<String>, span: Span) -> JadeError {
    JadeError::NotYetStatic { what: what.into(), span }
}

/// Purpose: This function makes the error for a checker answer that is missing.
///
/// Inputs:
/// - `what`: what was missing.
/// - `span`: where.
///
/// Outputs: the error, worded as a compiler bug.
pub(super) fn internal(what: impl std::fmt::Display, span: Span) -> JadeError {
    JadeError::TypeError {
        message: format!("internal compiler error in the static backend: {what}"),
        span,
    }
}

impl<'a> Lowerer<'a> {
    /// Purpose: This function makes the shared state, regrouping the checker's
    /// answers by instance.
    ///
    /// Inputs:
    /// - `top`: the program's statements.
    /// - `typed`: the checker's answer.
    ///
    /// Outputs: the lowerer.
    fn new(top: &'a [TStmt], typed: &'a Typed) -> Self {
        let mut exprs: HashMap<InstKey, HashMap<usize, Ty>> = HashMap::new();
        for ((k, a), t) in &typed.types {
            exprs.entry(k.clone()).or_default().insert(*a, norm(t));
        }
        let mut targets: HashMap<InstKey, HashMap<usize, Target>> = HashMap::new();
        for ((k, a), t) in &typed.targets {
            targets.entry(k.clone()).or_default().insert(*a, t.clone());
        }
        let mut slots: HashMap<InstKey, HashMap<usize, Ty>> = HashMap::new();
        for ((k, a), t) in &typed.slots {
            slots.entry(k.clone()).or_default().insert(*a, norm(t));
        }

        let mut names: Vec<&String> = typed.structs.keys().collect();
        names.sort();
        let mut structs = Vec::new();
        let mut struct_ids = HashMap::new();
        for n in names {
            let info = &typed.structs[n];
            struct_ids.insert(n.clone(), structs.len() as u32);
            structs.push(StructDef {
                name: n.clone(),
                fields: info.fields.iter().map(|(f, t)| (f.clone(), norm(t))).collect(),
                ancestors: info.ancestors.clone(),
            });
        }

        let mut gnames: Vec<&String> = typed.globals.keys().collect();
        gnames.sort();
        let globals = gnames
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), (GlobalId(i as u32), norm(&typed.globals[n]))))
            .collect();

        Lowerer {
            top,
            fns: HashMap::new(),
            methods: HashMap::new(),
            func_ids: HashMap::new(),
            sigs: HashMap::new(),
            exprs,
            targets,
            slots,
            globals,
            struct_ids,
            structs,
            packages: crate::builtins::all_packages()
                .iter()
                .map(|p| p.global_name.to_string())
                .collect(),
        }
    }

    /// Purpose: This function records every function and method definition.
    ///
    /// Inputs:
    /// - `stmts`: the program's statements.
    ///
    /// Outputs: none.
    fn collect(&mut self, stmts: &'a [TStmt]) {
        for s in stmts {
            match s {
                TStmt::FnDef { name, params, body, span, .. }
                | TStmt::AsyncFnDef { name, params, body, span, .. } => {
                    let is_async = matches!(s, TStmt::AsyncFnDef { .. });
                    self.fns.insert(name, FnDef { params, body, is_async, span: *span });
                }
                TStmt::ExtendBlock { type_name, methods, .. } => {
                    for m in methods {
                        if let TStmt::FnDef { name, params, body, span, .. }
                        | TStmt::AsyncFnDef { name, params, body, span, .. } = m
                        {
                            let is_async = matches!(m, TStmt::AsyncFnDef { .. });
                            self.methods.insert(
                                (type_name.as_str(), name.as_str()),
                                FnDef { params, body, is_async, span: *span },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Purpose: This function finds the definition an instance checks.
    ///
    /// Inputs:
    /// - `f`: the code.
    ///
    /// Outputs: the definition, or `None` for the top level and closures.
    pub(super) fn def_of(&self, f: &FnKey) -> Option<FnDef<'a>> {
        match f {
            FnKey::Named(n) => self.fns.get(n.as_str()).copied(),
            FnKey::Method(t, n) => self.methods.get(&(t.as_str(), n.as_str())).copied(),
            _ => None,
        }
    }

    /// Purpose: This function returns a function's id and signature.
    ///
    /// Inputs:
    /// - `k`: the instance.
    /// - `span`: where it is called, for errors.
    ///
    /// Outputs: the id, parameter types and return type.
    pub(super) fn sig(&self, k: &InstKey, span: Span) -> Result<(FuncId, Vec<Ty>, Ty)> {
        let id = *self
            .func_ids
            .get(k)
            .ok_or_else(|| internal("a call to an unchecked instance", span))?;
        let (p, r) = self.sigs[k].clone();
        Ok((id, p, r))
    }

    /// Purpose: This function returns a struct's layout.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: its definition, if it has one.
    pub(super) fn strukt(&self, name: &str) -> Option<&StructDef> {
        self.struct_ids.get(name).map(|i| &self.structs[*i as usize])
    }

    /// Purpose: This function finds a field's index in a struct's layout.
    ///
    /// Inputs:
    /// - `s`: the struct.
    /// - `field`: the field.
    ///
    /// Outputs: the index and the field's type.
    pub(super) fn field(&self, s: &str, field: &str) -> Option<(u32, Ty)> {
        let def = self.strukt(s)?;
        def.fields.iter().position(|(n, _)| n == field).map(|i| (i as u32, def.fields[i].1.clone()))
    }

    /// Purpose: This function says whether a value of struct `child` can be
    /// read as a `parent`: its fields must start with the parent's, in order.
    ///
    /// Inputs:
    /// - `child`, `parent`: the two structs.
    ///
    /// Outputs: true when the parent's layout is a prefix of the child's.
    fn prefix_layout(&self, child: &str, parent: &str) -> bool {
        match (self.strukt(child), self.strukt(parent)) {
            (Some(c), Some(p)) => {
                p.fields.len() <= c.fields.len()
                    && p.fields.iter().zip(&c.fields).all(|(a, b)| a.0 == b.0)
            }
            _ => false,
        }
    }

    /// Purpose: This function finds the nearest struct two structs share.
    ///
    /// Inputs:
    /// - `a`, `b`: two struct names.
    ///
    /// Outputs: the nearest common ancestor, if any.
    fn common_ancestor(&self, a: &str, b: &str) -> Option<String> {
        let line = |n: &str| -> Vec<String> {
            let mut v = vec![n.to_string()];
            if let Some(s) = self.strukt(n) {
                v.extend(s.ancestors.iter().cloned());
            }
            v
        };
        let lb = line(b);
        line(a).into_iter().find(|x| lb.contains(x))
    }

    /// Purpose: This function lowers one instance into a function.
    ///
    /// Inputs:
    /// - `k`: the instance.
    ///
    /// Outputs: the function.
    fn lower_instance(&self, k: &InstKey) -> Result<Func> {
        let (params, ret) = self.sigs[k].clone();
        let empty = HashMap::new();
        let empty_t = HashMap::new();
        let mut fx = Fx {
            l: self,
            types: self.exprs.get(k).unwrap_or(&empty),
            targets: self.targets.get(k).unwrap_or(&empty_t),
            slots: self.slots.get(k).unwrap_or(&empty),
            is_main: k.func == FnKey::Main,
            ret: if k.func == FnKey::Main { Ty::Nil } else { ret },
            locals: Vec::new(),
            blocks: vec![Block { stmts: Vec::new(), term: Term::Unreachable }],
            done: vec![false],
            cur: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            temps: Vec::new(),
            fresh: HashSet::new(),
        };
        let (name, body) = match &k.func {
            FnKey::Main => ("main".to_string(), self.top),
            other => {
                let def = self.def_of(other).ok_or_else(|| {
                    internal(
                        format!("no definition for {}", inst_label(k)),
                        Span { line: 0, col: 0 },
                    )
                })?;
                if def.is_async {
                    return Err(not_yet("async functions", def.span));
                }
                if super::ops::body_yields(def.body) {
                    return Err(not_yet("functions that yield", def.span));
                }
                for ((p, _), t) in def.params.iter().zip(&params) {
                    let l = fx.new_local(t.clone(), Some(p.clone()));
                    fx.scopes[0].insert(p.clone(), l);
                }
                (inst_label(k), def.body)
            }
        };
        let nparams = fx.locals.len() as u32;
        fx.lower_body(body)?;
        Ok(Func { name, params: nparams, locals: fx.locals, ret: fx.ret, blocks: fx.blocks })
    }
}

/// Purpose: This struct holds the state of lowering one function.
pub(super) struct Fx<'a, 'l> {
    pub(super) l: &'l Lowerer<'a>,
    types: &'l HashMap<usize, Ty>,
    pub(super) targets: &'l HashMap<usize, Target>,
    slots: &'l HashMap<usize, Ty>,
    is_main: bool,
    ret: Ty,
    locals: Vec<LocalDecl>,
    blocks: Vec<Block>,
    /// Whether each block has its terminator.
    done: Vec<bool>,
    cur: BlockId,
    /// Variables in scope, innermost last.
    scopes: Vec<HashMap<String, Local>>,
    /// Each enclosing loop's continue and break targets, innermost last.
    loops: Vec<(BlockId, BlockId)>,
    /// Heap temporaries of the statement being lowered, dropped when it ends.
    temps: Vec<Local>,
    /// Temporaries whose reference has not been handed over yet.
    fresh: HashSet<Local>,
}

impl<'a, 'l> Fx<'a, 'l> {
    // ── building blocks ──────────────────────────────────────────────────

    /// Purpose: This function makes a new local.
    ///
    /// Inputs:
    /// - `ty`: its type.
    /// - `name`: the variable's name, or `None` for a temporary.
    ///
    /// Outputs: the local.
    pub(super) fn new_local(&mut self, ty: Ty, name: Option<String>) -> Local {
        let l = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { ty, name });
        l
    }

    /// Purpose: This function makes a temporary for the current statement.
    ///
    /// Inputs:
    /// - `ty`: its type.
    ///
    /// Outputs: the local. A heap temporary is dropped when the statement ends.
    pub(super) fn temp(&mut self, ty: Ty) -> Local {
        let l = self.new_local(ty.clone(), None);
        if is_heap(&ty) {
            self.temps.push(l);
        }
        l
    }

    /// Purpose: This function computes an rvalue into a new temporary.
    ///
    /// Inputs:
    /// - `ty`: the value's type.
    /// - `rv`: the computation.
    ///
    /// Outputs: an operand naming the temporary.
    pub(super) fn assign_temp(&mut self, ty: Ty, rv: Rvalue) -> Operand {
        let t = self.temp(ty);
        self.emit(Stmt::Assign(Place::Local(t), rv));
        self.fresh.insert(t);
        Operand::Copy(Place::Local(t))
    }

    /// Purpose: This function turns an operand into one that hands over its
    /// reference when it is a temporary nothing else uses.
    ///
    /// Inputs:
    /// - `op`: the operand.
    ///
    /// Outputs: `Move` for a fresh temporary, the operand unchanged otherwise.
    pub(super) fn consume(&mut self, op: Operand) -> Operand {
        if let Operand::Copy(Place::Local(t)) = &op
            && self.fresh.remove(t)
        {
            return Operand::Move(*t);
        }
        op
    }

    /// Purpose: This function appends a statement to the current block.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: none. Code after a terminator goes into a fresh block nothing
    /// reaches.
    pub(super) fn emit(&mut self, s: Stmt) {
        if self.done[self.cur.0 as usize] {
            let b = self.new_block();
            self.cur = b;
        }
        self.blocks[self.cur.0 as usize].stmts.push(s);
    }

    /// Purpose: This function makes a new empty block.
    ///
    /// Inputs: none.
    ///
    /// Outputs: its id.
    pub(super) fn new_block(&mut self) -> BlockId {
        self.blocks.push(Block { stmts: Vec::new(), term: Term::Unreachable });
        self.done.push(false);
        BlockId(self.blocks.len() as u32 - 1)
    }

    /// Purpose: This function ends the current block.
    ///
    /// Inputs:
    /// - `t`: its terminator.
    ///
    /// Outputs: none.
    pub(super) fn terminate(&mut self, t: Term) {
        let i = self.cur.0 as usize;
        if !self.done[i] {
            self.blocks[i].term = t;
            self.done[i] = true;
        }
    }

    /// Purpose: This function continues lowering in another block.
    ///
    /// Inputs:
    /// - `b`: the block.
    ///
    /// Outputs: none.
    pub(super) fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    /// Purpose: This function drops the temporaries made since a mark.
    ///
    /// Inputs:
    /// - `mark`: the length of the temporary list when the statement began.
    ///
    /// Outputs: none.
    fn drop_temps(&mut self, mark: usize) {
        let ts: Vec<Local> = self.temps.drain(mark..).collect();
        for t in ts {
            self.fresh.remove(&t);
            self.emit(Stmt::Drop(Place::Local(t)));
        }
    }

    /// Purpose: This function returns an expression's checked type.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: its type.
    pub(super) fn ty(&self, e: &TExpr) -> Result<Ty> {
        self.types
            .get(&addr(e))
            .cloned()
            .ok_or_else(|| internal("an expression with no type", e.span))
    }

    /// Purpose: This function finds a variable: a local in scope, then a global.
    ///
    /// Inputs:
    /// - `name`: the variable.
    ///
    /// Outputs: its place and type, or `None`.
    pub(super) fn lookup(&self, name: &str) -> Option<(Place, Ty)> {
        for s in self.scopes.iter().rev() {
            if let Some(l) = s.get(name) {
                return Some((Place::Local(*l), self.locals[l.0 as usize].ty.clone()));
            }
        }
        self.l.globals.get(name).map(|(g, t)| (Place::Global(*g), t.clone()))
    }

    /// Purpose: This function declares a variable in the innermost scope.
    ///
    /// Inputs:
    /// - `name`: the variable.
    /// - `node`: the address of the statement that made its slot.
    /// - `span`: where, for errors.
    ///
    /// Outputs: its place and type.
    fn declare(&mut self, name: &str, node: usize, span: Span) -> Result<(Place, Ty)> {
        let ty = self
            .slots
            .get(&node)
            .cloned()
            .ok_or_else(|| internal("a variable with no type", span))?;
        let l = self.new_local(ty.clone(), Some(name.to_string()));
        self.scopes.last_mut().expect("scope").insert(name.to_string(), l);
        Ok((Place::Local(l), ty))
    }

    // ── conversions ──────────────────────────────────────────────────────

    /// Purpose: This function converts a value to a type it fits in.
    ///
    /// Inputs:
    /// - `op`: the value.
    /// - `from`: its type.
    /// - `to`: the type wanted.
    /// - `span`: where, for errors.
    ///
    /// Outputs: an operand of type `to`.
    pub(super) fn coerce(
        &mut self,
        op: Operand,
        from: &Ty,
        to: &Ty,
        span: Span,
    ) -> Result<Operand> {
        if from == to {
            return Ok(op);
        }
        match (from, to) {
            (Ty::Nil, Ty::Opt(_)) => Ok(Operand::Const(Const::Nil(to.clone()))),
            (Ty::Opt(a), Ty::Opt(b)) => match (&**a, &**b) {
                (Ty::Struct(x), Ty::Struct(y)) => {
                    self.check_upcast(x, y, span)?;
                    Ok(op)
                }
                _ => Err(internal(format!("cannot convert {from} to {to}"), span)),
            },
            (_, Ty::Opt(inner)) => {
                let v = self.coerce(op, from, inner, span)?;
                Ok(self.assign_temp(to.clone(), Rvalue::Wrap(to.clone(), v)))
            }
            (Ty::Opt(inner), _) => {
                let v = self.assign_temp((**inner).clone(), Rvalue::Unwrap(op));
                self.coerce(v, inner, to, span)
            }
            (Ty::Struct(x), Ty::Struct(y)) => {
                self.check_upcast(x, y, span)?;
                Ok(op)
            }
            _ => Err(internal(format!("cannot convert {from} to {to}"), span)),
        }
    }

    /// Purpose: This function refuses reading a child struct through a parent
    /// whose fields it does not start with, which needs a field table per type.
    ///
    /// Inputs:
    /// - `child`, `parent`: the two structs; a downcast is always allowed.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())`, or the not-yet error.
    fn check_upcast(&self, child: &str, parent: &str, span: Span) -> Result<()> {
        let is_ancestor =
            self.l.strukt(child).is_some_and(|c| c.ancestors.iter().any(|a| a == parent));
        if is_ancestor && !self.l.prefix_layout(child, parent) {
            return Err(not_yet(
                format!(
                    "a {} held as its second parent {}",
                    crate::compiler::infer::display_type_name(child),
                    crate::compiler::infer::display_type_name(parent)
                ),
                span,
            ));
        }
        Ok(())
    }

    /// Purpose: This function finds the one type two compared values both fit in.
    ///
    /// Inputs:
    /// - `a`, `b`: the two types.
    ///
    /// Outputs: the wider type.
    pub(super) fn common(&self, a: &Ty, b: &Ty) -> Ty {
        match (a, b) {
            _ if a == b => a.clone(),
            (Ty::Nil, t) | (t, Ty::Nil) => Ty::opt(t.clone()),
            (Ty::Opt(x), y) | (y, Ty::Opt(x)) => Ty::opt(self.common(x, y)),
            (Ty::Struct(x), Ty::Struct(y)) => {
                Ty::Struct(self.l.common_ancestor(x, y).unwrap_or_else(|| x.clone()))
            }
            _ => a.clone(),
        }
    }

    // ── bodies and statements ────────────────────────────────────────────

    /// Purpose: This function lowers a function's whole body, including the
    /// return at its end.
    ///
    /// Inputs:
    /// - `body`: the statements.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn lower_body(&mut self, body: &'a [TStmt]) -> Result<()> {
        // A function's trailing bare expression is its return value.
        if !self.is_main
            && let Some(TStmt::Expr(e)) = body.last()
        {
            for s in &body[..body.len() - 1] {
                self.stmt(s)?;
            }
            let mark = self.temps.len();
            let (v, t) = self.expr(e)?;
            let ret = self.ret.clone();
            let v = self.coerce(v, &t, &ret, e.span)?;
            let v = self.consume(v);
            self.terminate(Term::Return(v));
            self.temps.truncate(mark);
            return Ok(());
        }
        for s in body {
            self.stmt(s)?;
        }
        // Falling off the end returns nil, where the return type can hold it.
        match &self.ret {
            Ty::Nil | Ty::Opt(_) => {
                let r = self.ret.clone();
                self.terminate(Term::Return(Operand::Const(Const::Nil(r))));
            }
            _ => self.terminate(Term::Unreachable),
        }
        Ok(())
    }

    /// Purpose: This function lowers a block of statements in a new scope.
    ///
    /// Inputs:
    /// - `body`: the statements.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn block(&mut self, body: &'a [TStmt]) -> Result<()> {
        self.scopes.push(HashMap::new());
        for s in body {
            self.stmt(s)?;
        }
        let scope = self.scopes.pop().expect("scope");
        // Free what the block's variables hold once it ends normally.
        let mut ls: Vec<Local> = scope.into_values().collect();
        ls.sort_by_key(|l| l.0);
        for l in ls {
            if is_heap(&self.locals[l.0 as usize].ty) {
                self.emit(Stmt::Drop(Place::Local(l)));
            }
        }
        Ok(())
    }

    /// Purpose: This function lowers one statement and drops its temporaries.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn stmt(&mut self, s: &'a TStmt) -> Result<()> {
        let mark = self.temps.len();
        self.stmt_inner(s)?;
        self.drop_temps(mark);
        Ok(())
    }

    /// Purpose: This function stores a value into a place, converting it to the
    /// place's type.
    ///
    /// Inputs:
    /// - `place`, `pty`: the place and its type.
    /// - `v`, `vty`: the value and its type.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())` or an error.
    fn store(&mut self, place: Place, pty: &Ty, v: Operand, vty: &Ty, span: Span) -> Result<()> {
        let v = self.coerce(v, vty, pty, span)?;
        let v = self.consume(v);
        self.emit(Stmt::Assign(place, Rvalue::Use(v)));
        Ok(())
    }

    /// Purpose: This function lowers one statement.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn stmt_inner(&mut self, s: &'a TStmt) -> Result<()> {
        match s {
            TStmt::Let { name, value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, pt) = if self.is_main && self.scopes.len() == 1 {
                    let (g, t) = self
                        .l
                        .globals
                        .get(name)
                        .cloned()
                        .ok_or_else(|| internal("an unknown global", *span))?;
                    (Place::Global(g), t)
                } else {
                    self.declare(name, addr(s), *span)?
                };
                self.store(place, &pt, v, &vt, *span)
            }
            TStmt::Assign { name, value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, pt) = match self.lookup(name) {
                    Some(p) => p,
                    None => self.declare(name, addr(s), *span)?,
                };
                self.store(place, &pt, v, &vt, *span)
            }
            TStmt::FnDef { decorators, span, .. } | TStmt::AsyncFnDef { decorators, span, .. } => {
                if !decorators.is_empty() {
                    return Err(not_yet("decorators", *span));
                }
                Ok(())
            }
            TStmt::StructDef { .. }
            | TStmt::ExtendBlock { .. }
            | TStmt::Use { .. }
            | TStmt::FromUse { .. } => Ok(()),
            TStmt::Return { value, span } => {
                let ret = self.ret.clone();
                let v = match value {
                    Some(e) => {
                        let (v, t) = self.expr(e)?;
                        self.coerce(v, &t, &ret, *span)?
                    }
                    None => {
                        self.coerce(Operand::Const(Const::Nil(Ty::Nil)), &Ty::Nil, &ret, *span)?
                    }
                };
                let v = self.consume(v);
                self.terminate(Term::Return(v));
                Ok(())
            }
            TStmt::If { condition, then_body, else_body, .. } => {
                let mark = self.temps.len();
                let (c, _) = self.expr(condition)?;
                self.drop_temps(mark);
                let bt = self.new_block();
                let be = self.new_block();
                let end = if else_body.is_some() { self.new_block() } else { be };
                self.terminate(Term::If(c, bt, be));
                self.switch_to(bt);
                self.block(then_body)?;
                self.terminate(Term::Goto(end));
                if let Some(e) = else_body {
                    self.switch_to(be);
                    self.block(e)?;
                    self.terminate(Term::Goto(end));
                }
                self.switch_to(end);
                Ok(())
            }
            TStmt::While { condition, body, .. } => {
                let head = self.new_block();
                let bb = self.new_block();
                let exit = self.new_block();
                self.terminate(Term::Goto(head));
                self.switch_to(head);
                let mark = self.temps.len();
                let (c, _) = self.expr(condition)?;
                self.drop_temps(mark);
                self.terminate(Term::If(c, bb, exit));
                self.switch_to(bb);
                self.loops.push((head, exit));
                self.block(body)?;
                self.loops.pop();
                self.terminate(Term::Goto(head));
                self.switch_to(exit);
                Ok(())
            }
            TStmt::For { var, iterable, body, span } => {
                self.for_loop(s, var, iterable, body, *span)
            }
            TStmt::Break { span } => {
                let (_, brk) =
                    *self.loops.last().ok_or_else(|| internal("break outside a loop", *span))?;
                self.terminate(Term::Goto(brk));
                Ok(())
            }
            TStmt::Continue { span } => {
                let (cont, _) =
                    *self.loops.last().ok_or_else(|| internal("continue outside a loop", *span))?;
                self.terminate(Term::Goto(cont));
                Ok(())
            }
            TStmt::FieldAssign { object, field, value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, ot) =
                    self.lookup(object).ok_or_else(|| internal("an unknown variable", *span))?;
                let Ty::Struct(sn) = &ot else {
                    return Err(internal(format!("a field set on {ot}"), *span));
                };
                let (index, ft) =
                    self.l.field(sn, field).ok_or_else(|| internal("an unknown field", *span))?;
                if ft == Ty::Prompt {
                    return Err(not_yet("prompt fields", *span));
                }
                let v = self.coerce(v, &vt, &ft, *span)?;
                let v = self.consume(v);
                self.emit(Stmt::SetField {
                    object: Operand::Copy(place),
                    strukt: sn.clone(),
                    index,
                    value: v,
                });
                Ok(())
            }
            TStmt::IndexAssign { name, index, value, span } => {
                let (i, it) = self.expr(index)?;
                let (v, vt) = self.expr(value)?;
                let (place, tt) =
                    self.lookup(name).ok_or_else(|| internal("an unknown variable", *span))?;
                match &tt {
                    Ty::Array(e) => {
                        let v = self.coerce(v, &vt, e, *span)?;
                        let v = self.consume(v);
                        self.emit(Stmt::Eval(Rvalue::Op(
                            Op::ArraySet((**e).clone()),
                            vec![Operand::Copy(place), i, v],
                        )));
                        Ok(())
                    }
                    Ty::Map(e) => {
                        let k = self.coerce(i, &it, &Ty::Str, *span)?;
                        let k = self.consume(k);
                        let v = self.coerce(v, &vt, e, *span)?;
                        let v = self.consume(v);
                        self.emit(Stmt::DictSet { dict: place, key: k, value: v });
                        Ok(())
                    }
                    Ty::Bytes => Err(not_yet("bytes", *span)),
                    other => Err(internal(format!("an index assignment into {other}"), *span)),
                }
            }
            TStmt::PromptDecl { span, .. } => Err(not_yet("prompts", *span)),
            TStmt::Yield { span, .. } => Err(not_yet("yield", *span)),
            TStmt::Raise { span, .. } => Err(not_yet("raise", *span)),
            TStmt::TryCatch { span, .. } => Err(not_yet("try and catch", *span)),
            TStmt::Expr(e) => {
                self.expr(e)?;
                Ok(())
            }
        }
    }

    /// Purpose: This function lowers a `for` loop over an array, a string or a
    /// dict's keys.
    ///
    /// Inputs:
    /// - `s`: the statement, whose address names the loop variable's slot.
    /// - `var`, `iterable`, `body`: its parts.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())` or an error.
    fn for_loop(
        &mut self,
        s: &'a TStmt,
        var: &str,
        iterable: &'a TExpr,
        body: &'a [TStmt],
        span: Span,
    ) -> Result<()> {
        let (it, itt) = self.expr(iterable)?;
        // What is walked is an array, kept in a local of its own so the loop
        // owns it for as long as it runs.
        let (arr, elem) = match &itt {
            Ty::Array(e) => (it, (**e).clone()),
            Ty::Str => {
                let a = self
                    .assign_temp(Ty::Array(Box::new(Ty::Char)), Rvalue::Op(Op::StrChars, vec![it]));
                (a, Ty::Char)
            }
            Ty::Map(v) => {
                let a = self.assign_temp(
                    Ty::Array(Box::new(Ty::Str)),
                    Rvalue::Op(Op::DictKeys((**v).clone()), vec![it]),
                );
                (a, Ty::Str)
            }
            Ty::Stream(_) => return Err(not_yet("streams", span)),
            other => return Err(not_yet(format!("a for loop over {other}"), span)),
        };
        let aty = Ty::Array(Box::new(elem.clone()));
        let held = self.new_local(aty.clone(), None);
        let arr = self.consume(arr);
        self.emit(Stmt::Assign(Place::Local(held), Rvalue::Use(arr)));
        let idx = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(Place::Local(idx), Rvalue::Use(Operand::Const(Const::Int(0)))));
        // The length is read once, before the first pass, as v1 did.
        let len = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(
            Place::Local(len),
            Rvalue::Op(Op::ArrayLen, vec![Operand::Copy(Place::Local(held))]),
        ));

        let head = self.new_block();
        let bb = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();
        self.terminate(Term::Goto(head));
        self.switch_to(head);
        let more = self.new_local(Ty::Bool, None);
        self.emit(Stmt::Assign(
            Place::Local(more),
            Rvalue::Bin(
                BinOp::Cmp(CmpOp::Lt, Ty::Int),
                Operand::Copy(Place::Local(idx)),
                Operand::Copy(Place::Local(len)),
            ),
        ));
        self.terminate(Term::If(Operand::Copy(Place::Local(more)), bb, exit));

        self.switch_to(bb);
        self.scopes.push(HashMap::new());
        let (vp, vty) = self.declare(var, addr(s) + 1, span)?;
        let mark = self.temps.len();
        let el = self.assign_temp(
            elem.clone(),
            Rvalue::Op(
                Op::ArrayGet(elem.clone()),
                vec![Operand::Copy(Place::Local(held)), Operand::Copy(Place::Local(idx))],
            ),
        );
        self.store(vp, &vty, el, &elem, span)?;
        self.drop_temps(mark);
        self.loops.push((step, exit));
        for st in body {
            self.stmt(st)?;
        }
        self.loops.pop();
        self.scopes.pop();
        self.terminate(Term::Goto(step));

        self.switch_to(step);
        self.emit(Stmt::Assign(
            Place::Local(idx),
            Rvalue::Bin(
                BinOp::Arith(ArithOp::Add, NumTy::Int),
                Operand::Copy(Place::Local(idx)),
                Operand::Const(Const::Int(1)),
            ),
        ));
        self.terminate(Term::Goto(head));

        self.switch_to(exit);
        self.emit(Stmt::Drop(Place::Local(held)));
        if is_heap(&vty) {
            self.emit(Stmt::Drop(vp));
        }
        Ok(())
    }

    // ── expressions ──────────────────────────────────────────────────────

    /// Purpose: This function lowers an expression.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: an operand holding its value, and its type.
    pub(super) fn expr(&mut self, e: &'a TExpr) -> Result<(Operand, Ty)> {
        let span = e.span;
        let et = self.ty(e)?;
        let op = match &e.kind {
            TExprKind::Integer(i) => Operand::Const(Const::Int(*i)),
            TExprKind::Float(x) => Operand::Const(Const::Float(*x)),
            TExprKind::Bool(b) => Operand::Const(Const::Bool(*b)),
            TExprKind::Str(s) => Operand::Const(Const::Str(s.clone())),
            TExprKind::Identifier(name) => {
                if matches!(name.as_str(), "nil" | "None" | "null") {
                    Operand::Const(Const::Nil(et.clone()))
                } else if let Some((place, pt)) = self.lookup(name) {
                    // A narrowed read: the variable is `T?` and known present here.
                    self.coerce(Operand::Copy(place), &pt, &et, span)?
                } else {
                    return Err(not_yet("functions used as values", span));
                }
            }
            TExprKind::Call { callee, args, kwargs } => {
                return self.call(e, &et, callee, args, kwargs);
            }
            TExprKind::BinOp { op, left, right } => self.binop(op, left, right, &et, span)?,
            TExprKind::UnaryOp { op, operand } => {
                let (v, _) = self.expr(operand)?;
                let u = match op {
                    UnaryOpKind::Not => UnOp::Not,
                    UnaryOpKind::BitNot => UnOp::BitNot,
                    UnaryOpKind::Neg => {
                        UnOp::Neg(if et == Ty::Float { NumTy::Float } else { NumTy::Int })
                    }
                };
                self.assign_temp(et.clone(), Rvalue::Un(u, v))
            }
            TExprKind::StructLiteral { type_name, base, fields } => {
                self.struct_literal(type_name, base.as_deref(), fields, span)?
            }
            TExprKind::FieldAccess { object, field } => {
                let (o, ot) = self.expr(object)?;
                self.read_field(o, &ot, field, &et, span)?
            }
            TExprKind::Index { object, index } => {
                let (o, ot) = self.expr(object)?;
                match &ot {
                    Ty::Array(el) => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(
                            et.clone(),
                            Rvalue::Op(Op::ArrayGet((**el).clone()), vec![o, i]),
                        )
                    }
                    Ty::Str => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(Ty::Char, Rvalue::Op(Op::StrIndex, vec![o, i]))
                    }
                    Ty::Map(v) => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(
                            et.clone(),
                            Rvalue::Op(Op::DictGet((**v).clone()), vec![o, i]),
                        )
                    }
                    Ty::Struct(_) => match &index.kind {
                        TExprKind::Str(field) => self.read_field(o, &ot, field, &et, span)?,
                        _ => return Err(internal("a struct indexed by a non-literal", span)),
                    },
                    Ty::Bytes => return Err(not_yet("bytes", span)),
                    Ty::Json => return Err(not_yet("json values", span)),
                    Ty::Stream(_) => return Err(not_yet("streams", span)),
                    other => return Err(internal(format!("an index into {other}"), span)),
                }
            }
            TExprKind::Array { elements } => {
                let Ty::Array(el) = &et else {
                    return Err(internal("an array literal that is not an array", span));
                };
                let mut ops = Vec::with_capacity(elements.len());
                for x in elements {
                    let (v, t) = self.expr(x)?;
                    let v = self.coerce(v, &t, el, x.span)?;
                    ops.push(self.consume(v));
                }
                self.assign_temp(et.clone(), Rvalue::Array((**el).clone(), ops))
            }
            TExprKind::Dict { entries } => {
                let Ty::Map(vt) = &et else {
                    return Err(internal("a dict literal that is not a dict", span));
                };
                let mut ops = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let (kv, kt) = self.expr(k)?;
                    let kv = self.coerce(kv, &kt, &Ty::Str, k.span)?;
                    let kv = self.consume(kv);
                    let (vv, vty) = self.expr(v)?;
                    let vv = self.coerce(vv, &vty, vt, v.span)?;
                    let vv = self.consume(vv);
                    ops.push((kv, vv));
                }
                self.assign_temp(et.clone(), Rvalue::Dict((**vt).clone(), ops))
            }
            TExprKind::FStr { parts } => {
                let mut ops = Vec::new();
                for p in parts {
                    match p {
                        TFStrPart::Literal(s) => {
                            if !s.is_empty() {
                                ops.push(Operand::Const(Const::Str(s.clone())));
                            }
                        }
                        TFStrPart::Expr(x) => {
                            let (v, t) = self.expr(x)?;
                            ops.push(self.text_of(v, &t));
                        }
                    }
                }
                match ops.len() {
                    0 => Operand::Const(Const::Str(String::new())),
                    _ => self.assign_temp(Ty::Str, Rvalue::Op(Op::StrCat, ops)),
                }
            }
            TExprKind::Await { .. } => return Err(not_yet("await", span)),
            TExprKind::PromptDeref { .. } | TExprKind::PromptLiteral { .. } => {
                return Err(not_yet("prompts", span));
            }
        };
        Ok((op, et))
    }

    /// Purpose: This function gives the text of a value, as `print` shows it.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a string operand; a string is itself.
    pub(super) fn text_of(&mut self, v: Operand, t: &Ty) -> Operand {
        match t {
            Ty::Str => v,
            Ty::Char => self.assign_temp(Ty::Str, Rvalue::Op(Op::CharToStr, vec![v])),
            other => self.assign_temp(Ty::Str, Rvalue::Op(Op::ToStr(other.clone()), vec![v])),
        }
    }

    /// Purpose: This function reads a field off a struct value.
    ///
    /// Inputs:
    /// - `o`, `ot`: the object and its static type.
    /// - `field`: the field.
    /// - `et`: the expression's type.
    /// - `span`: where.
    ///
    /// Outputs: the field's value.
    fn read_field(
        &mut self,
        o: Operand,
        ot: &Ty,
        field: &str,
        et: &Ty,
        span: Span,
    ) -> Result<Operand> {
        match ot {
            Ty::Struct(sn) => match self.l.field(sn, field) {
                Some((index, ft)) => {
                    if ft == Ty::Prompt {
                        return Err(not_yet("prompt fields", span));
                    }
                    let v = self.assign_temp(
                        ft.clone(),
                        Rvalue::Field { object: o, strukt: sn.clone(), index },
                    );
                    self.coerce(v, &ft, et, span)
                }
                None => Err(not_yet("bound methods used as values", span)),
            },
            Ty::Error => Err(not_yet("caught errors", span)),
            other => Err(not_yet(format!("methods of {other} used as values"), span)),
        }
    }

    /// Purpose: This function lowers a struct literal.
    ///
    /// Inputs:
    /// - `type_name`: the struct.
    /// - `base`: the `...base` value, if any.
    /// - `fields`: the fields the literal names, or all of them with no base.
    /// - `span`: where.
    ///
    /// Outputs: the new struct.
    fn struct_literal(
        &mut self,
        type_name: &str,
        base: Option<&'a TExpr>,
        fields: &'a [(String, TExpr, bool)],
        span: Span,
    ) -> Result<Operand> {
        let def =
            self.l.strukt(type_name).ok_or_else(|| internal("an unknown struct", span))?.clone();
        let mut given: HashMap<&str, Operand> = HashMap::new();
        for (name, value, is_prompt) in fields {
            if *is_prompt {
                return Err(not_yet("prompt fields", value.span));
            }
            let (v, t) = self.expr(value)?;
            let ft = self
                .l
                .field(type_name, name)
                .map(|(_, t)| t)
                .ok_or_else(|| internal("an unknown field", span))?;
            let v = self.coerce(v, &t, &ft, value.span)?;
            given.insert(name.as_str(), v);
        }
        let base = match base {
            Some(b) => Some(self.expr(b)?),
            None => None,
        };
        let mut ops = Vec::with_capacity(def.fields.len());
        for (fname, fty) in &def.fields {
            if *fty == Ty::Prompt {
                return Err(not_yet("prompt fields", span));
            }
            let v = match given.remove(fname.as_str()) {
                Some(v) => self.consume(v),
                None => {
                    let (bo, bt) = base
                        .clone()
                        .ok_or_else(|| internal(format!("no value for field '{fname}'"), span))?;
                    let Ty::Struct(bn) = &bt else {
                        return Err(internal("a copy-with base that is not a struct", span));
                    };
                    let (bi, bft) = self
                        .l
                        .field(bn, fname)
                        .ok_or_else(|| internal("a base missing a field", span))?;
                    let v = self.assign_temp(
                        bft.clone(),
                        Rvalue::Field { object: bo, strukt: bn.clone(), index: bi },
                    );
                    let v = self.coerce(v, &bft, fty, span)?;
                    self.consume(v)
                }
            };
            ops.push(v);
        }
        Ok(self.assign_temp(
            Ty::Struct(type_name.to_string()),
            Rvalue::Struct(type_name.to_string(), ops),
        ))
    }

    /// Purpose: This function lowers a binary operator.
    ///
    /// Inputs:
    /// - `op`: the operator.
    /// - `left`, `right`: the operands.
    /// - `et`: the result type.
    /// - `span`: where.
    ///
    /// Outputs: the result.
    fn binop(
        &mut self,
        op: &BinOpKind,
        left: &'a TExpr,
        right: &'a TExpr,
        et: &Ty,
        span: Span,
    ) -> Result<Operand> {
        if matches!(op, BinOpKind::And | BinOpKind::Or) {
            return self.short_circuit(matches!(op, BinOpKind::And), left, right);
        }
        let nil_l = is_nil(left);
        let nil_r = is_nil(right);
        if matches!(op, BinOpKind::Eq | BinOpKind::Ne) && (nil_l || nil_r) {
            let other = if nil_l { right } else { left };
            let (v, t) = self.expr(other)?;
            let is = match t {
                Ty::Nil => Operand::Const(Const::Bool(true)),
                Ty::Opt(_) => self.assign_temp(Ty::Bool, Rvalue::IsNil(v)),
                Ty::Json | Ty::Error => {
                    return Err(not_yet(format!("comparing {t} with nil"), span));
                }
                _ => Operand::Const(Const::Bool(false)),
            };
            return Ok(if matches!(op, BinOpKind::Ne) {
                self.assign_temp(Ty::Bool, Rvalue::Un(UnOp::Not, is))
            } else {
                is
            });
        }
        let (l, lt) = self.expr(left)?;
        let (r, rt) = self.expr(right)?;
        let arith = |o: &BinOpKind| match o {
            BinOpKind::Add => Some(ArithOp::Add),
            BinOpKind::Sub => Some(ArithOp::Sub),
            BinOpKind::Mul => Some(ArithOp::Mul),
            BinOpKind::Div => Some(ArithOp::Div),
            BinOpKind::Mod => Some(ArithOp::Rem),
            _ => None,
        };
        let cmp = |o: &BinOpKind| match o {
            BinOpKind::Eq => Some(CmpOp::Eq),
            BinOpKind::Ne => Some(CmpOp::Ne),
            BinOpKind::Lt => Some(CmpOp::Lt),
            BinOpKind::Le => Some(CmpOp::Le),
            BinOpKind::Gt => Some(CmpOp::Gt),
            BinOpKind::Ge => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(a) = arith(op) {
            return Ok(match et {
                Ty::Str => {
                    let l = self.char_as_str(l, &lt);
                    let r = self.char_as_str(r, &rt);
                    self.assign_temp(Ty::Str, Rvalue::Bin(BinOp::Concat, l, r))
                }
                Ty::Float => {
                    let l = self.as_float(l, &lt);
                    let r = self.as_float(r, &rt);
                    self.assign_temp(Ty::Float, Rvalue::Bin(BinOp::Arith(a, NumTy::Float), l, r))
                }
                _ => self.assign_temp(Ty::Int, Rvalue::Bin(BinOp::Arith(a, NumTy::Int), l, r)),
            });
        }
        if let Some(c) = cmp(op) {
            let ordered = !matches!(c, CmpOp::Eq | CmpOp::Ne);
            // A char compares with a string as the one-character string it spells.
            if matches!((&lt, &rt), (Ty::Char, Ty::Str) | (Ty::Str, Ty::Char)) {
                let l = self.char_as_str(l, &lt);
                let r = self.char_as_str(r, &rt);
                return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, Ty::Str), l, r)));
            }
            if ordered && matches!((&lt, &rt), (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int)) {
                let l = self.as_float(l, &lt);
                let r = self.as_float(r, &rt);
                return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, Ty::Float), l, r)));
            }
            let ct = self.common(&lt, &rt);
            let l = self.coerce(l, &lt, &ct, span)?;
            let r = self.coerce(r, &rt, &ct, span)?;
            return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, ct), l, r)));
        }
        let bit = match op {
            BinOpKind::BitAnd => Some(BitOp::And),
            BinOpKind::BitOr => Some(BitOp::Or),
            BinOpKind::BitXor => Some(BitOp::Xor),
            BinOpKind::Shl => Some(BitOp::Shl),
            BinOpKind::Shr => Some(BitOp::Shr),
            _ => None,
        };
        if let Some(b) = bit {
            return Ok(self.assign_temp(Ty::Int, Rvalue::Bin(BinOp::Bit(b), l, r)));
        }
        // `in` and `not in`.
        let found = match &rt {
            Ty::Array(el) => {
                let l = self.coerce(l, &lt, el, span)?;
                self.assign_temp(
                    Ty::Bool,
                    Rvalue::Op(Op::ArrayContains((**el).clone()), vec![r, l]),
                )
            }
            Ty::Map(v) => {
                self.assign_temp(Ty::Bool, Rvalue::Op(Op::DictHas((**v).clone()), vec![r, l]))
            }
            Ty::Str => {
                let l = self.char_as_str(l, &lt);
                self.assign_temp(Ty::Bool, Rvalue::Op(Op::Str(StrMethod::Contains), vec![r, l]))
            }
            other => return Err(not_yet(format!("'in' on {other}"), span)),
        };
        Ok(if matches!(op, BinOpKind::NotIn) {
            self.assign_temp(Ty::Bool, Rvalue::Un(UnOp::Not, found))
        } else {
            found
        })
    }

    /// Purpose: This function turns a char into the string spelling it.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a string operand; anything but a char is returned unchanged.
    pub(super) fn char_as_str(&mut self, v: Operand, t: &Ty) -> Operand {
        if *t == Ty::Char {
            self.assign_temp(Ty::Str, Rvalue::Op(Op::CharToStr, vec![v]))
        } else {
            v
        }
    }

    /// Purpose: This function turns an int into a float.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a float operand.
    fn as_float(&mut self, v: Operand, t: &Ty) -> Operand {
        match (t, &v) {
            (Ty::Int, Operand::Const(Const::Int(i))) => Operand::Const(Const::Float(*i as f64)),
            (Ty::Int, _) => self.assign_temp(Ty::Float, Rvalue::IntToFloat(v)),
            _ => v,
        }
    }

    /// Purpose: This function lowers `&&` and `||`, which skip the right side
    /// when the left already decides.
    ///
    /// Inputs:
    /// - `and`: true for `&&`.
    /// - `left`, `right`: the operands.
    ///
    /// Outputs: the result.
    fn short_circuit(&mut self, and: bool, left: &'a TExpr, right: &'a TExpr) -> Result<Operand> {
        let (l, _) = self.expr(left)?;
        let res = self.new_local(Ty::Bool, None);
        self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(l)));
        let rhs = self.new_block();
        let end = self.new_block();
        let cond = Operand::Copy(Place::Local(res));
        self.terminate(if and { Term::If(cond, rhs, end) } else { Term::If(cond, end, rhs) });
        self.switch_to(rhs);
        let (r, _) = self.expr(right)?;
        self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(r)));
        self.terminate(Term::Goto(end));
        self.switch_to(end);
        Ok(Operand::Copy(Place::Local(res)))
    }

    // ── calls ────────────────────────────────────────────────────────────

    /// Purpose: This function lowers a call, by what the checker resolved it to.
    ///
    /// Inputs:
    /// - `e`: the call node.
    /// - `et`: its type.
    /// - `callee`, `args`, `kwargs`: its parts.
    ///
    /// Outputs: the result and its type.
    fn call(
        &mut self,
        e: &'a TExpr,
        et: &Ty,
        callee: &'a TExpr,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
    ) -> Result<(Operand, Ty)> {
        let span = e.span;
        let target = self.targets.get(&addr(e)).cloned();
        match target {
            Some(Target::Instance(k)) => {
                let def = self
                    .l
                    .def_of(&k.func)
                    .ok_or_else(|| internal("a call to a function with no definition", span))?;
                let mut vals: Vec<Option<(Operand, Ty, Span)>> = vec![None; def.params.len()];
                let mut next = 0;
                if let FnKey::Method(..) = &k.func {
                    let TExprKind::FieldAccess { object, .. } = &callee.kind else {
                        return Err(not_yet("bound methods used as values", span));
                    };
                    let (o, t) = self.expr(object)?;
                    vals[0] = Some((o, t, object.span));
                    next = 1;
                }
                for a in args {
                    let (v, t) = self.expr(a)?;
                    if next >= vals.len() {
                        return Err(internal("too many arguments", span));
                    }
                    vals[next] = Some((v, t, a.span));
                    next += 1;
                }
                for (name, v) in kwargs {
                    let i = def
                        .params
                        .iter()
                        .position(|(p, _)| p == name)
                        .ok_or_else(|| internal("an unknown keyword argument", span))?;
                    let (o, t) = self.expr(v)?;
                    vals[i] = Some((o, t, v.span));
                }
                let r = self.call_instance(&k, def, vals, span)?;
                Ok((r, et.clone()))
            }
            Some(Target::Dispatch(kinds)) => {
                let TExprKind::FieldAccess { object, .. } = &callee.kind else {
                    return Err(internal("a dispatched call with no receiver", span));
                };
                let (recv, rt) = self.expr(object)?;
                let mut argv = Vec::new();
                for a in args {
                    let (v, t) = self.expr(a)?;
                    argv.push((v, t, a.span));
                }
                if !kwargs.is_empty() {
                    return Err(not_yet("keyword arguments to an inherited method", span));
                }
                let res = self.temp(et.clone());
                let tid = self.assign_temp(Ty::Int, Rvalue::TypeId(recv.clone()));
                let end = self.new_block();
                let default = self.new_block();
                let mut cases = Vec::new();
                let mut bodies = Vec::new();
                for (kind, k) in &kinds {
                    let b = self.new_block();
                    let id = *self
                        .l
                        .struct_ids
                        .get(kind)
                        .ok_or_else(|| internal("an unknown struct", span))?;
                    cases.push((id as i64, b));
                    bodies.push((b, k.clone()));
                }
                self.terminate(Term::Switch { value: tid, cases, default });
                for (b, k) in bodies {
                    self.switch_to(b);
                    let def = self
                        .l
                        .def_of(&k.func)
                        .ok_or_else(|| internal("a method with no body", span))?;
                    let mut vals: Vec<Option<(Operand, Ty, Span)>> = vec![None; def.params.len()];
                    vals[0] = Some((recv.clone(), rt.clone(), span));
                    for (i, a) in argv.iter().enumerate() {
                        if i + 1 < vals.len() {
                            vals[i + 1] = Some(a.clone());
                        }
                    }
                    let (_, _, ret) = self.l.sig(&k, span)?;
                    let r = self.call_instance(&k, def, vals, span)?;
                    let r = self.coerce(r, &ret, et, span)?;
                    let r = self.consume(r);
                    self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(r)));
                    self.terminate(Term::Goto(end));
                }
                self.switch_to(default);
                self.terminate(Term::Unreachable);
                self.switch_to(end);
                self.fresh.insert(res);
                Ok((Operand::Copy(Place::Local(res)), et.clone()))
            }
            Some(Target::Builtin(name)) => self.builtin(e, et, &name, callee, args, kwargs),
            None => match &callee.kind {
                TExprKind::Identifier(n) if n.starts_with("__native$") => {
                    Err(not_yet("C libraries", span))
                }
                _ => Err(not_yet("calls through function values", span)),
            },
        }
    }

    /// Purpose: This function calls one instance with its arguments bound,
    /// filling defaults and converting each to its parameter's type.
    ///
    /// Inputs:
    /// - `k`: the instance.
    /// - `def`: its definition.
    /// - `vals`: each parameter's argument, if the call gave one.
    /// - `span`: where.
    ///
    /// Outputs: the call's result.
    fn call_instance(
        &mut self,
        k: &InstKey,
        def: FnDef<'a>,
        vals: Vec<Option<(Operand, Ty, Span)>>,
        span: Span,
    ) -> Result<Operand> {
        let (id, params, ret) = self.l.sig(k, span)?;
        let mut argv = Vec::with_capacity(params.len());
        for (i, (v, pt)) in vals.into_iter().zip(&params).enumerate() {
            let (o, t, sp) = match v {
                Some(x) => x,
                None => {
                    let d = def.params[i]
                        .1
                        .as_ref()
                        .ok_or_else(|| internal("a missing argument", span))?;
                    let (o, t) = self.default_arg(d)?;
                    (o, t, d.span)
                }
            };
            argv.push(self.coerce(o, &t, pt, sp)?);
        }
        Ok(self.assign_temp(ret, Rvalue::Call(id, argv)))
    }

    /// Purpose: This function lowers a parameter's default value at a call.
    ///
    /// Inputs:
    /// - `d`: the default, always a literal.
    ///
    /// Outputs: its value and type.
    fn default_arg(&mut self, d: &'a TExpr) -> Result<(Operand, Ty)> {
        if self.types.contains_key(&addr(d)) {
            return self.expr(d);
        }
        Ok(match &d.kind {
            TExprKind::Integer(i) => (Operand::Const(Const::Int(*i)), Ty::Int),
            TExprKind::Float(x) => (Operand::Const(Const::Float(*x)), Ty::Float),
            TExprKind::Bool(b) => (Operand::Const(Const::Bool(*b)), Ty::Bool),
            TExprKind::Str(s) => (Operand::Const(Const::Str(s.clone())), Ty::Str),
            _ => (Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil),
        })
    }
}

/// Purpose: This function says whether an expression is the literal `nil`.
///
/// Inputs:
/// - `e`: the expression.
///
/// Outputs: true for `nil`, `None` or `null`.
fn is_nil(e: &TExpr) -> bool {
    matches!(&e.kind, TExprKind::Identifier(n) if n == "nil" || n == "None" || n == "null")
}
