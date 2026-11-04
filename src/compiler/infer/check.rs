//! Checking statements and expressions inside one instance.
//!
//! An instance is checked with its parameters bound to the argument types it
//! was made for. Every `let` gets a slot, cached by the node's address so the
//! next pass reuses it. Reading a variable gives its slot's type, and assigning
//! one joins into it, so a variable has one type for its whole life. The only
//! way a variable's type differs from its slot's is narrowing: inside
//! `if x != nil { … }`, `x` is read as its non-optional type.

use std::collections::HashMap;

use super::ty::{FuncRef, Ty};
use super::{Checker, ClosureInfo, FnKey, InstKey, Target, addr, is_nil_literal};
use crate::compiler::tir::{TExpr, TExprKind, TFStrPart, TStmt};
use crate::frontend::ast::{BinOpKind, UnaryOpKind};
use crate::frontend::error::Span;

/// Purpose: This struct holds the state of checking one instance's body.
pub(super) struct Frame {
    pub key: InstKey,
    /// Slots made in this instance, by node address.
    pub slots: HashMap<usize, Ty>,
    /// Local variables, innermost scope last.
    pub scopes: Vec<HashMap<String, Ty>>,
    /// Narrowed reads, innermost last: a name read as a narrower type here.
    pub narrowed: Vec<HashMap<String, Ty>>,
    pub ret: Ty,
    pub elem: Option<Ty>,
    pub is_main: bool,
}

impl<'a> Checker<'a> {
    /// Purpose: This function checks one instance's body.
    ///
    /// Inputs:
    /// - `key`: the instance.
    ///
    /// Outputs: none. Types land in slots; errors land in `self.error`.
    pub(super) fn check_instance(&mut self, key: &InstKey) {
        let inst = self.instances.get_mut(key).expect("instance exists");
        let slots = std::mem::take(&mut inst.slots);
        let (params, ret, elem) = (inst.params.clone(), inst.ret.clone(), inst.elem.clone());
        let mut frame = Frame {
            key: key.clone(),
            slots,
            scopes: vec![HashMap::new()],
            narrowed: vec![HashMap::new()],
            ret,
            elem,
            is_main: key.func == FnKey::Main,
        };
        let body: &'a [TStmt] = match &key.func {
            FnKey::Main => self.top,
            FnKey::Named(n) => {
                let info = self.fns[n].clone();
                for ((p, _), t) in info.params.iter().zip(&params) {
                    frame.scopes[0].insert(p.clone(), t.clone());
                }
                info.body
            }
            FnKey::Method(t, n) => {
                let info = self.methods[&(t.clone(), n.clone())].clone();
                for ((p, _), t) in info.params.iter().zip(&params) {
                    frame.scopes[0].insert(p.clone(), t.clone());
                }
                info.body
            }
            FnKey::Closure(a) => {
                let info = self.closures[a].clone();
                for (p, t) in info.params.iter().zip(&params) {
                    frame.scopes[0].insert(p.clone(), t.clone());
                }
                info.body
            }
        };
        // A function's trailing bare expression is its return value:
        // `fn double(x) { x * 2 }` returns `x * 2`.
        let trailing = match body.last() {
            Some(TStmt::Expr(e)) if !frame.is_main && frame.elem.is_none() => Some(e),
            _ => None,
        };
        match trailing {
            Some(e) => {
                self.check_block(&body[..body.len() - 1], &mut frame, false);
                if self.error.is_none() {
                    let t = self.check_expr(e, &mut frame);
                    let r = frame.ret.clone();
                    self.unify(&r, &t, e.span, "this function's return values");
                }
            }
            None => {
                self.check_block(body, &mut frame, false);
                if !frame.is_main && frame.elem.is_none() && falls_off(body) {
                    let r = frame.ret.clone();
                    let _ = self.store.join(&r, &Ty::Nil);
                }
            }
        }
        if let Some(inst) = self.instances.get_mut(key) {
            inst.slots = frame.slots;
        }
    }

    /// Purpose: This function returns the slot cached for a node in this
    /// instance, making it the first time.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `node`: the node's address.
    ///
    /// Outputs: the slot.
    pub(super) fn slot(&mut self, f: &mut Frame, node: usize) -> Ty {
        if let Some(t) = f.slots.get(&node) {
            return t.clone();
        }
        let t = self.store.fresh();
        f.slots.insert(node, t.clone());
        t
    }

    /// Purpose: This function joins two types, reporting a mismatch in the
    /// words of what was being done.
    ///
    /// Inputs:
    /// - `a`, `b`: the types.
    /// - `span`: where.
    /// - `what`: a phrase saying what needed them to match.
    ///
    /// Outputs: the joined type, or `a` after reporting.
    pub(super) fn unify(&mut self, a: &Ty, b: &Ty, span: Span, what: &str) -> Ty {
        match self.store.join(a, b) {
            Ok(t) => t,
            Err(m) if m.recursive => {
                self.report(
                    span,
                    format!(
                        "a value cannot contain itself ({what}); point back at it through a \
                         struct field instead"
                    ),
                );
                a.clone()
            }
            Err(m) => {
                let (l, r) = (self.store.resolve(&m.left), self.store.resolve(&m.right));
                let hint = mismatch_hint(&l, &r);
                self.report(span, format!("type mismatch in {what}: {l} and {r} do not mix{hint}"));
                a.clone()
            }
        }
    }

    /// Purpose: This function checks a block in a new scope.
    ///
    /// Inputs:
    /// - `body`: the statements.
    /// - `f`: the frame.
    /// - `scoped`: whether the block opens a scope; the top level of a body
    ///   already has one.
    ///
    /// Outputs: none.
    fn check_block(&mut self, body: &'a [TStmt], f: &mut Frame, scoped: bool) {
        if scoped {
            f.scopes.push(HashMap::new());
            f.narrowed.push(HashMap::new());
        }
        for s in body {
            self.check_stmt(s, f);
            if self.error.is_some() {
                break;
            }
        }
        if scoped {
            f.scopes.pop();
            f.narrowed.pop();
        }
    }

    /// Purpose: This function finds a variable's slot: a local, then a global.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `name`: the variable.
    ///
    /// Outputs: the slot, or `None` when no variable has that name.
    fn lookup_var(&self, f: &Frame, name: &str) -> Option<Ty> {
        for scope in f.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        self.globals.get(name).cloned()
    }

    /// Purpose: This function checks one statement.
    ///
    /// Inputs:
    /// - `s`: the statement.
    /// - `f`: the frame.
    ///
    /// Outputs: none.
    fn check_stmt(&mut self, s: &'a TStmt, f: &mut Frame) {
        match s {
            TStmt::Let { name, value, span } => {
                let t = self.check_expr(value, f);
                let slot = if f.is_main && f.scopes.len() == 1 {
                    self.globals[name].clone()
                } else {
                    let slot = self.slot(f, addr(s));
                    f.scopes.last_mut().expect("scope").insert(name.clone(), slot.clone());
                    slot
                };
                self.clear_narrowing(f, name);
                self.unify(&slot, &t, *span, &format!("'{name}'"));
            }
            TStmt::Assign { name, value, span } => {
                let t = self.check_expr(value, f);
                let slot = match self.lookup_var(f, name) {
                    Some(slot) => slot,
                    None => {
                        let slot = self.slot(f, addr(s));
                        f.scopes.last_mut().expect("scope").insert(name.clone(), slot.clone());
                        slot
                    }
                };
                self.clear_narrowing(f, name);
                self.unify(&slot, &t, *span, &format!("assigning to '{name}'"));
            }
            TStmt::PromptDecl { name, body, span } => {
                let t = self.check_expr(body, f);
                self.expect_text(&t, *span, "a prompt's text");
                let slot = if f.is_main && f.scopes.len() == 1 {
                    self.globals[name].clone()
                } else {
                    let slot = self.slot(f, addr(s));
                    f.scopes.last_mut().expect("scope").insert(name.clone(), slot.clone());
                    slot
                };
                self.unify(&slot, &Ty::Prompt, *span, &format!("'{name}'"));
            }
            TStmt::FnDef { name, decorators, span, .. }
            | TStmt::AsyncFnDef { name, decorators, span, .. } => {
                if !f.is_main || f.scopes.len() > 1 {
                    self.report(
                        *span,
                        format!(
                            "'{name}' is defined inside a block; define functions at the top level \
                             of a file"
                        ),
                    );
                    return;
                }
                if !decorators.is_empty() {
                    // `@d fn g` makes `g` mean `d(g)`: the global holds what the
                    // decorators return, applied in the order they are written.
                    let mut cur = Ty::Func(FuncRef::Named(name.clone()));
                    for (dec, args) in decorators.iter().rev() {
                        let mut tys = vec![cur];
                        for (_, a) in args {
                            tys.push(self.check_expr(a, f));
                        }
                        cur = self.call_name_with(dec, tys, *span, f, addr(s));
                    }
                    let slot = self.globals[name].clone();
                    self.unify(&slot, &cur, *span, &format!("decorated '{name}'"));
                }
            }
            TStmt::Return { value, span } => {
                let t = match value {
                    Some(v) => self.check_expr(v, f),
                    None => Ty::Nil,
                };
                if f.is_main {
                    self.report(*span, "'return' outside a function".to_string());
                    return;
                }
                let r = f.ret.clone();
                self.unify(&r, &t, *span, "this function's return values");
            }
            TStmt::Yield { value, span } => {
                let t = self.check_expr(value, f);
                match f.elem.clone() {
                    Some(e) => {
                        self.unify(&e, &t, *span, "this function's yielded values");
                    }
                    None => self.report(*span, "'yield' outside a function".to_string()),
                }
            }
            TStmt::If { condition, then_body, else_body, span } => {
                let c = self.check_expr(condition, f);
                self.unify(&c, &Ty::Bool, *span, "an if condition");
                let (then_narrow, else_narrow) = self.narrowing_of(condition, f);
                self.check_narrowed_block(then_body, f, then_narrow);
                match else_body {
                    Some(e) => self.check_narrowed_block(e, f, else_narrow),
                    // `if x == nil { return }` leaves `x` known to be there for
                    // the rest of the block.
                    None if exits(then_body) => {
                        let narrowed = f.narrowed.last_mut().expect("scope");
                        narrowed.extend(else_narrow);
                    }
                    None => {}
                }
            }
            TStmt::While { condition, body, span } => {
                let c = self.check_expr(condition, f);
                self.unify(&c, &Ty::Bool, *span, "a while condition");
                let (narrow, _) = self.narrowing_of(condition, f);
                self.check_narrowed_block(body, f, narrow);
            }
            TStmt::For { var, iterable, body, span } => {
                let it = self.check_expr(iterable, f);
                let elem = self.element_of(&it, *span, f, addr(s));
                f.scopes.push(HashMap::new());
                f.narrowed.push(HashMap::new());
                let slot = self.slot(f, addr(s) + 1);
                self.unify(&slot, &elem, *span, "the loop variable");
                f.scopes.last_mut().expect("scope").insert(var.clone(), slot);
                self.check_block(body, f, false);
                f.scopes.pop();
                f.narrowed.pop();
            }
            TStmt::Break { .. } | TStmt::Continue { .. } => {}
            TStmt::StructDef { .. } | TStmt::ExtendBlock { .. } => {}
            TStmt::FieldAssign { object, field, value, span } => {
                let v = self.check_expr(value, f);
                let Some(obj) = self.lookup_var(f, object) else {
                    self.report(*span, format!("undefined variable '{object}'"));
                    return;
                };
                let ot = self.store.shallow(&obj);
                match ot {
                    Ty::Struct(sn) => match self.field_slot(&sn, field) {
                        Some(slot) => {
                            let slot = if self.structs[&sn].prompt_fields.contains(field) {
                                self.expect_text(&v, *span, "a prompt field");
                                return;
                            } else {
                                slot
                            };
                            self.unify(&slot, &v, *span, &format!("field '{field}'"));
                        }
                        None => self.report(
                            *span,
                            format!(
                                "struct '{}' has no field '{field}'",
                                super::ty::display_name(&sn)
                            ),
                        ),
                    },
                    Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(
                            *span,
                            format!(
                                "cannot set field '{field}' on {other}; only a struct has fields{}",
                                if matches!(other, Ty::Map(_)) {
                                    format!(", so write {object}[\"{field}\"] = … for a dict")
                                } else {
                                    String::new()
                                }
                            ),
                        );
                    }
                }
            }
            TStmt::IndexAssign { name, index, value, span } => {
                let i = self.check_expr(index, f);
                let v = self.check_expr(value, f);
                let Some(target) = self.lookup_var(f, name) else {
                    self.report(*span, format!("undefined variable '{name}'"));
                    return;
                };
                let tt = self.store.shallow(&target);
                match tt {
                    Ty::Array(e) => {
                        self.unify(&i, &Ty::Int, *span, "an array index");
                        self.unify(&e, &v, *span, &format!("an element of '{name}'"));
                    }
                    Ty::Map(e) => {
                        self.unify(&i, &Ty::Str, *span, "a dict key");
                        self.unify(&e, &v, *span, &format!("a value of '{name}'"));
                    }
                    Ty::Bytes => {
                        self.unify(&i, &Ty::Int, *span, "a bytes index");
                        self.unify(&v, &Ty::Int, *span, "an octet written into bytes");
                    }
                    Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(*span, format!("cannot assign by index into {other}"));
                    }
                }
            }
            TStmt::Use { .. } | TStmt::FromUse { .. } => {}
            TStmt::Raise { value, span } => {
                let t = self.check_expr(value, f);
                match self.store.shallow(&t) {
                    Ty::Struct(_) | Ty::Str | Ty::Error | Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(
                            *span,
                            format!(
                                "cannot raise {other}; raise a struct, or a string, which becomes a \
                                 RuntimeError"
                            ),
                        );
                    }
                }
            }
            TStmt::TryCatch { body, arms, span } => {
                self.check_block(body, f, true);
                for arm in arms {
                    f.scopes.push(HashMap::new());
                    f.narrowed.push(HashMap::new());
                    let bound = match &arm.catch_type {
                        Some(t) => {
                            if !self.structs.contains_key(t) {
                                self.report(*span, format!("unknown type '{t}' in catch"));
                            }
                            Ty::Struct(t.clone())
                        }
                        None => Ty::Error,
                    };
                    f.scopes.last_mut().expect("scope").insert(arm.binding.clone(), bound);
                    self.check_block(&arm.body, f, false);
                    f.scopes.pop();
                    f.narrowed.pop();
                }
            }
            TStmt::Expr(e) => {
                self.check_expr(e, f);
            }
        }
    }

    /// Purpose: This function checks a block with some names read as narrower types.
    ///
    /// Inputs:
    /// - `body`: the block.
    /// - `f`: the frame.
    /// - `narrow`: names and the types they are read as inside the block.
    ///
    /// Outputs: none.
    fn check_narrowed_block(
        &mut self,
        body: &'a [TStmt],
        f: &mut Frame,
        narrow: HashMap<String, Ty>,
    ) {
        f.scopes.push(HashMap::new());
        f.narrowed.push(narrow);
        self.check_block(body, f, false);
        f.scopes.pop();
        f.narrowed.pop();
    }

    /// Purpose: This function stops narrowing a name once it is assigned.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `name`: the variable.
    ///
    /// Outputs: none.
    fn clear_narrowing(&self, f: &mut Frame, name: &str) {
        for n in f.narrowed.iter_mut() {
            n.remove(name);
        }
    }

    /// Purpose: This function works out what a condition narrows in each branch.
    ///
    /// Inputs:
    /// - `cond`: the condition.
    /// - `f`: the frame.
    ///
    /// Outputs: the names narrowed when the condition is true, and when it is false.
    fn narrowing_of(
        &mut self,
        cond: &TExpr,
        f: &Frame,
    ) -> (HashMap<String, Ty>, HashMap<String, Ty>) {
        let mut yes = HashMap::new();
        let mut no = HashMap::new();
        match &cond.kind {
            TExprKind::BinOp { op: op @ (BinOpKind::Ne | BinOpKind::Eq), left, right } => {
                let name = match (&left.kind, &right.kind) {
                    (TExprKind::Identifier(n), _) if is_nil_literal(right) => Some(n),
                    (_, TExprKind::Identifier(n)) if is_nil_literal(left) => Some(n),
                    _ => None,
                };
                if let Some(n) = name
                    && let Some(slot) = self.read_var(f, n)
                    && let Ty::Opt(inner) = self.store.shallow(&slot)
                {
                    if matches!(op, BinOpKind::Ne) {
                        yes.insert(n.clone(), *inner);
                    } else {
                        no.insert(n.clone(), *inner);
                    }
                }
            }
            TExprKind::BinOp { op: BinOpKind::And, left, right } => {
                let (l, _) = self.narrowing_of(left, f);
                let (r, _) = self.narrowing_of(right, f);
                yes.extend(l);
                yes.extend(r);
            }
            _ => {}
        }
        (yes, no)
    }

    /// Purpose: This function reads a variable, honoring narrowing.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `name`: the variable.
    ///
    /// Outputs: its type here, or `None` when there is no such variable.
    fn read_var(&self, f: &Frame, name: &str) -> Option<Ty> {
        for n in f.narrowed.iter().rev() {
            if let Some(t) = n.get(name) {
                return Some(t.clone());
            }
        }
        self.lookup_var(f, name)
    }

    /// Purpose: This function says what a `for` loop over a value binds.
    ///
    /// Inputs:
    /// - `it`: the iterated value's type.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the loop's address, for a slot when the type is not known yet.
    ///
    /// Outputs: the loop variable's type.
    fn element_of(&mut self, it: &Ty, span: Span, f: &mut Frame, node: usize) -> Ty {
        match self.store.shallow(it) {
            Ty::Array(e) | Ty::Stream(e) => *e,
            Ty::Map(_) => Ty::Str,
            Ty::Str => Ty::Char,
            Ty::Bytes => Ty::Int,
            Ty::Json => Ty::Json,
            Ty::Var(_) => self.slot(f, node + 2),
            other => {
                let other = self.store.resolve(&other);
                self.report(span, format!("cannot loop over {other}"));
                self.slot(f, node + 2)
            }
        }
    }

    /// Purpose: This function requires a value to be text: a string, or for a
    /// prompt field a prompt.
    ///
    /// Inputs:
    /// - `t`: the value's type.
    /// - `span`: where.
    /// - `what`: what needed text.
    ///
    /// Outputs: none.
    fn expect_text(&mut self, t: &Ty, span: Span, what: &str) {
        match self.store.shallow(t) {
            Ty::Str | Ty::Prompt => {}
            Ty::Var(_) => {
                self.unify(t, &Ty::Str, span, what);
            }
            other => {
                let other = self.store.resolve(&other);
                self.report(span, format!("{what} must be a str, got {other}"));
            }
        }
    }

    /// Purpose: This function checks an expression and records its type.
    ///
    /// Inputs:
    /// - `e`: the expression.
    /// - `f`: the frame.
    ///
    /// Outputs: its type.
    pub(super) fn check_expr(&mut self, e: &'a TExpr, f: &mut Frame) -> Ty {
        let t = self.check_expr_inner(e, f);
        if self.final_pass {
            let r = self.store.resolve(&t);
            self.out.types.insert((f.key.clone(), addr(e)), r);
        }
        t
    }

    /// Purpose: This function works out an expression's type.
    ///
    /// Inputs:
    /// - `e`: the expression.
    /// - `f`: the frame.
    ///
    /// Outputs: its type.
    fn check_expr_inner(&mut self, e: &'a TExpr, f: &mut Frame) -> Ty {
        let span = e.span;
        match &e.kind {
            TExprKind::Integer(_) => Ty::Int,
            TExprKind::Float(_) => Ty::Float,
            TExprKind::Bool(_) => Ty::Bool,
            TExprKind::Str(_) => Ty::Str,
            TExprKind::Identifier(name) => self.check_identifier(name, span, f, addr(e)),
            TExprKind::Call { callee, args, kwargs } => self.check_call(e, callee, args, kwargs, f),
            TExprKind::BinOp { op, left, right } => self.check_binop(op, left, right, span, f),
            TExprKind::UnaryOp { op, operand } => {
                let t = self.check_expr(operand, f);
                match op {
                    UnaryOpKind::Not => self.unify(&t, &Ty::Bool, span, "'!'"),
                    UnaryOpKind::BitNot => self.unify(&t, &Ty::Int, span, "'~'"),
                    UnaryOpKind::Neg => match self.store.shallow(&t) {
                        Ty::Int | Ty::Float | Ty::Var(_) => t,
                        other => {
                            let other = self.store.resolve(&other);
                            self.report(span, format!("cannot negate {other}"));
                            t
                        }
                    },
                }
            }
            TExprKind::StructLiteral { type_name, base, fields } => {
                self.check_struct_literal(type_name, base.as_deref(), fields, span, f)
            }
            TExprKind::FieldAccess { object, field } => {
                self.check_field(object, field, span, f, addr(e))
            }
            TExprKind::Index { object, index } => self.check_index(object, index, span, f, addr(e)),
            TExprKind::Array { elements } => {
                let elem = self.slot(f, addr(e));
                for el in elements {
                    let t = self.check_expr(el, f);
                    self.unify(&elem, &t, el.span, "the elements of an array");
                }
                Ty::Array(Box::new(elem))
            }
            TExprKind::Dict { entries } => {
                let val = self.slot(f, addr(e));
                for (k, v) in entries {
                    let kt = self.check_expr(k, f);
                    self.unify(&kt, &Ty::Str, k.span, "a dict key");
                    let vt = self.check_expr(v, f);
                    let msg = "the values of a dict (a dict holds one kind of value; use a struct \
                               for named fields of different kinds)";
                    self.unify(&val, &vt, v.span, msg);
                }
                Ty::Map(Box::new(val))
            }
            TExprKind::FStr { parts } => {
                for p in parts {
                    if let TFStrPart::Expr(x) = p {
                        self.check_expr(x, f);
                    }
                }
                Ty::Str
            }
            TExprKind::PromptLiteral { body } => {
                let t = self.check_expr(body, f);
                self.expect_text(&t, span, "a prompt's text");
                Ty::Prompt
            }
            TExprKind::PromptDeref { expr, output_type, grammar_expr } => {
                let t = self.check_expr(expr, f);
                match self.store.shallow(&t) {
                    Ty::Prompt | Ty::Str | Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(span, format!("'?' dereferences a prompt, got {other}"));
                    }
                }
                if let Some(g) = grammar_expr {
                    let gt = self.check_expr(g, f);
                    self.unify(&gt, &Ty::Grammar, span, "a grammar constraint");
                }
                match output_type.as_deref() {
                    None => Ty::Str,
                    Some(t) => self.named_type(t, span),
                }
            }
            TExprKind::Closure { params, body, .. } => {
                let a = addr(e);
                self.closures.entry(a).or_insert(ClosureInfo { params, body });
                self.store.arities.insert(FuncRef::Closure(a), (params.len(), false));
                Ty::Func(FuncRef::Closure(a))
            }
            TExprKind::Await { expr } => {
                let t = self.check_expr(expr, f);
                match self.store.shallow(&t) {
                    Ty::Future(inner) => *inner,
                    Ty::Var(_) => self.slot(f, addr(e)),
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(span, format!("'await' needs a future, got {other}"));
                        self.slot(f, addr(e))
                    }
                }
            }
        }
    }

    /// Purpose: This function gives the type a type name in `?p |> T` names.
    ///
    /// Inputs:
    /// - `name`: the type name.
    /// - `span`: where.
    ///
    /// Outputs: the type.
    fn named_type(&mut self, name: &str, span: Span) -> Ty {
        match name {
            "int" => Ty::Int,
            "float" => Ty::Float,
            "bool" => Ty::Bool,
            "str" => Ty::Str,
            "char" => Ty::Char,
            "nil" | "null" => Ty::Nil,
            other if self.structs.contains_key(other) => {
                self.constructed.insert(other.to_string());
                Ty::Struct(other.to_string())
            }
            other => {
                self.report(span, format!("unknown type '{other}'"));
                Ty::Nil
            }
        }
    }

    /// Purpose: This function types a bare name.
    ///
    /// Inputs:
    /// - `name`: the identifier.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: its address.
    ///
    /// Outputs: the name's type.
    fn check_identifier(&mut self, name: &str, span: Span, f: &mut Frame, node: usize) -> Ty {
        if matches!(name, "nil" | "None" | "null") {
            return Ty::Nil;
        }
        if let Some(t) = self.read_var(f, name) {
            return t;
        }
        if self.fns.contains_key(name) {
            return Ty::Func(FuncRef::Named(name.to_string()));
        }
        if self.packages.contains(name) {
            self.report(
                span,
                format!("'{name}' is a package; call one of its functions, such as {name}.<fn>(…)"),
            );
            return self.slot(f, node);
        }
        if super::builtins::is_builtin_value(name) {
            return Ty::Func(FuncRef::Builtin(name.to_string()));
        }
        if self.structs.contains_key(name) {
            self.report(
                span,
                format!("'{}' is a struct type, not a value", super::ty::display_name(name)),
            );
            return self.slot(f, node);
        }
        self.report(span, format!("undefined variable '{}'", super::ty::display_name(name)));
        self.slot(f, node)
    }

    /// Purpose: This function types a call expression.
    ///
    /// Inputs:
    /// - `e`: the call node.
    /// - `callee`, `args`, `kwargs`: its parts.
    /// - `f`: the frame.
    ///
    /// Outputs: the call's type.
    fn check_call(
        &mut self,
        e: &'a TExpr,
        callee: &'a TExpr,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
        f: &mut Frame,
    ) -> Ty {
        let span = e.span;
        let node = addr(e);
        // A package function: `math.floor(x)`.
        if let TExprKind::FieldAccess { object, field } = &callee.kind
            && let TExprKind::Identifier(pkg) = &object.kind
            && self.read_var(f, pkg).is_none()
            && !self.fns.contains_key(pkg.as_str())
            && (self.packages.contains(pkg.as_str()) || pkg == "Grammar")
        {
            return self.package_call(pkg, field, args, kwargs, span, f, node);
        }
        // A method call: `obj.m(args)`.
        if let TExprKind::FieldAccess { object, field } = &callee.kind {
            let recv = self.check_expr(object, f);
            let mut tys = Vec::new();
            for a in args {
                tys.push(self.check_expr(a, f));
            }
            for (_, v) in kwargs {
                self.check_expr(v, f);
            }
            return self.method_call(&recv, field, tys, args, span, f, node);
        }
        // A call by name.
        if let TExprKind::Identifier(name) = &callee.kind
            && self.read_var(f, name).is_none()
        {
            if name == "join" {
                return self.check_join(args, kwargs, span, f, node);
            }
            if name == "wait" {
                return self.check_wait(args, span, f, node);
            }
            if let Some(info) = self.fns.get(name).cloned() {
                if self.decorated.contains_key(name) {
                    // A decorated name means what its decorators returned.
                    let slot = self.globals[name].clone();
                    let mut tys = Vec::new();
                    for a in args {
                        tys.push(self.check_expr(a, f));
                    }
                    return self.call_value(&slot, tys, span, f, node);
                }
                let tys = self.bind_args(&info.name, info.params, args, kwargs, span, f);
                let Some(tys) = tys else { return self.slot(f, node) };
                return match self.instantiate(FnKey::Named(name.clone()), tys, span) {
                    Some(k) => {
                        self.record_target(f, node, Target::Instance(k.clone()));
                        self.instance_result(&k)
                    }
                    None => self.slot(f, node),
                };
            }
            if super::builtins::is_builtin_value(name)
                || name == "print"
                || name == "write"
                || name == "input"
            {
                let mut tys = Vec::new();
                for a in args {
                    tys.push(self.check_expr(a, f));
                }
                for (_, v) in kwargs {
                    self.check_expr(v, f);
                }
                self.record_target(f, node, Target::Builtin(name.clone()));
                return self.builtin_call(name, tys, args, span, f, node);
            }
            if self.structs.contains_key(name) {
                self.report(
                    span,
                    format!(
                        "'{0}' is a struct type, not a function; build one with {0} {{ … }}",
                        super::ty::display_name(name)
                    ),
                );
                return self.slot(f, node);
            }
            if name.starts_with("__native$") {
                for a in args {
                    self.check_expr(a, f);
                }
                return self.native_call(name, span, f, node);
            }
        }
        // A call through a value: a variable holding a function, or any expression.
        let ct = self.check_expr(callee, f);
        let mut tys = Vec::new();
        for a in args {
            tys.push(self.check_expr(a, f));
        }
        for (_, v) in kwargs {
            self.check_expr(v, f);
        }
        self.call_value(&ct, tys, span, f, node)
    }

    /// Purpose: This function records what a call resolved to, on the final pass.
    ///
    /// Inputs:
    /// - `f`: the frame.
    /// - `node`: the call's address.
    /// - `t`: the target.
    ///
    /// Outputs: none.
    pub(super) fn record_target(&mut self, f: &Frame, node: usize, t: Target) {
        if self.final_pass {
            self.out.targets.insert((f.key.clone(), node), t);
        }
    }

    /// Purpose: This function matches a call's arguments to a function's
    /// parameters, filling defaults and keyword arguments.
    ///
    /// Inputs:
    /// - `name`: the function, for errors.
    /// - `params`: its parameters with their defaults.
    /// - `args`, `kwargs`: the call's arguments.
    /// - `span`: where.
    /// - `f`: the frame.
    ///
    /// Outputs: one type per parameter, or `None` after reporting an arity error.
    fn bind_args(
        &mut self,
        name: &str,
        params: &'a [(String, Option<TExpr>)],
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
        span: Span,
        f: &mut Frame,
    ) -> Option<Vec<Ty>> {
        let mut out: Vec<Option<Ty>> = vec![None; params.len()];
        if args.len() > params.len() {
            self.report(
                span,
                format!(
                    "'{}' takes {} argument{}, got {}",
                    super::ty::display_name(name),
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            );
            for a in args {
                self.check_expr(a, f);
            }
            return None;
        }
        for (i, a) in args.iter().enumerate() {
            out[i] = Some(self.check_expr(a, f));
        }
        for (k, v) in kwargs {
            let t = self.check_expr(v, f);
            match params.iter().position(|(p, _)| p == k) {
                Some(i) if out[i].is_none() => out[i] = Some(t),
                Some(_) => {
                    self.report(v.span, format!("argument '{k}' given twice"));
                    return None;
                }
                None => {
                    self.report(
                        v.span,
                        format!("'{}' has no parameter named '{k}'", super::ty::display_name(name)),
                    );
                    return None;
                }
            }
        }
        let mut tys = Vec::with_capacity(params.len());
        for (i, (p, default)) in params.iter().enumerate() {
            match (out[i].take(), default) {
                (Some(t), _) => tys.push(t),
                (None, Some(d)) => tys.push(self.check_expr(d, f)),
                (None, None) => {
                    self.report(
                        span,
                        format!("'{}' needs an argument for '{p}'", super::ty::display_name(name)),
                    );
                    return None;
                }
            }
        }
        Some(tys)
    }

    /// Purpose: This function calls a value whose type is a function.
    ///
    /// Inputs:
    /// - `ct`: the callee's type.
    /// - `tys`: the argument types.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: the call's type.
    pub(super) fn call_value(
        &mut self,
        ct: &Ty,
        tys: Vec<Ty>,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        match self.store.shallow(ct) {
            Ty::Func(r) => match self.call_func_ref(&r, tys, span, Some((f, node))) {
                Some(t) => t,
                None => self.slot(f, node),
            },
            Ty::Fn(ft) => {
                if ft.params.len() != tys.len() {
                    self.report(
                        span,
                        format!(
                            "this function takes {} arguments, got {}",
                            ft.params.len(),
                            tys.len()
                        ),
                    );
                    return self.slot(f, node);
                }
                for (p, a) in ft.params.iter().zip(&tys) {
                    self.unify(p, a, span, "an argument to a function value");
                }
                if ft.is_async { Ty::Future(Box::new(ft.ret.clone())) } else { ft.ret.clone() }
            }
            Ty::Var(_) => self.slot(f, node),
            other => {
                let other = self.store.resolve(&other);
                self.report(span, format!("{other} is not callable"));
                self.slot(f, node)
            }
        }
    }

    /// Purpose: This function calls a function value at some argument types.
    ///
    /// Inputs:
    /// - `r`: the function.
    /// - `tys`: the argument types.
    /// - `span`: where.
    /// - `site`: the frame and call address, when the call is in a body.
    ///
    /// Outputs: the call's type, or `None` after reporting an error.
    pub(super) fn call_func_ref(
        &mut self,
        r: &FuncRef,
        tys: Vec<Ty>,
        span: Span,
        site: Option<(&mut Frame, usize)>,
    ) -> Option<Ty> {
        match r {
            FuncRef::Named(n) => {
                let info = self.fns.get(n)?.clone();
                let tys = self.fill_defaults(&info.name, info.params, tys, span)?;
                let k = self.instantiate(FnKey::Named(n.clone()), tys, span)?;
                if let Some((f, node)) = site {
                    self.record_target(f, node, Target::Instance(k.clone()));
                }
                Some(self.instance_result(&k))
            }
            FuncRef::Closure(a) => {
                let info = self.closures.get(a)?.clone();
                if info.params.len() != tys.len() {
                    self.report(
                        span,
                        format!(
                            "this closure takes {} arguments, got {}",
                            info.params.len(),
                            tys.len()
                        ),
                    );
                    return None;
                }
                let k = self.instantiate(FnKey::Closure(*a), tys, span)?;
                if let Some((f, node)) = site {
                    self.record_target(f, node, Target::Instance(k.clone()));
                }
                Some(self.instance_result(&k))
            }
            FuncRef::Builtin(n) => {
                let (f, node) = match site {
                    Some(s) => s,
                    None => return super::builtins::builtin_value_result(self, n, &tys, span),
                };
                Some(self.builtin_call(n, tys, &[], span, f, node))
            }
            FuncRef::Method { recv, name } => match site {
                Some((f, node)) => Some(self.method_call(recv, name, tys, &[], span, f, node)),
                None => {
                    let recv = (**recv).clone();
                    self.method_result_detached(&recv, name, tys, span)
                }
            },
        }
    }

    /// Purpose: This function fills a named function's defaults for a call
    /// through a value, where only positional arguments are possible.
    ///
    /// Inputs:
    /// - `name`: the function.
    /// - `params`: its parameters.
    /// - `tys`: the positional argument types.
    /// - `span`: where.
    ///
    /// Outputs: one type per parameter, or `None` after reporting.
    fn fill_defaults(
        &mut self,
        name: &str,
        params: &'a [(String, Option<TExpr>)],
        mut tys: Vec<Ty>,
        span: Span,
    ) -> Option<Vec<Ty>> {
        if tys.len() > params.len() {
            self.report(
                span,
                format!(
                    "'{}' takes {} argument{}, got {}",
                    super::ty::display_name(name),
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    tys.len()
                ),
            );
            return None;
        }
        for (p, d) in &params[tys.len()..] {
            match d {
                Some(d) => tys.push(literal_type(d)),
                None => {
                    self.report(
                        span,
                        format!("'{}' needs an argument for '{p}'", super::ty::display_name(name)),
                    );
                    return None;
                }
            }
        }
        Some(tys)
    }

    /// Purpose: This function calls a top-level function or builtin by name
    /// with types already worked out, as a decorator is applied.
    ///
    /// Inputs:
    /// - `name`: the function.
    /// - `tys`: the argument types.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: an address for slots.
    ///
    /// Outputs: the call's type.
    fn call_name_with(
        &mut self,
        name: &str,
        tys: Vec<Ty>,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        if let Some(t) = self.read_var(f, name) {
            return self.call_value(&t, tys, span, f, node);
        }
        if self.fns.contains_key(name) {
            return match self.call_func_ref(&FuncRef::Named(name.to_string()), tys, span, None) {
                Some(t) => t,
                None => self.slot(f, node),
            };
        }
        self.report(span, format!("undefined decorator '{}'", super::ty::display_name(name)));
        self.slot(f, node)
    }

    /// Purpose: This function types a binary operator.
    ///
    /// Inputs:
    /// - `op`: the operator.
    /// - `left`, `right`: the operands.
    /// - `span`: where.
    /// - `f`: the frame.
    ///
    /// Outputs: the result type.
    fn check_binop(
        &mut self,
        op: &BinOpKind,
        left: &'a TExpr,
        right: &'a TExpr,
        span: Span,
        f: &mut Frame,
    ) -> Ty {
        let l = self.check_expr(left, f);
        // `a && b` narrows `b` by `a`, so `x != nil && x.len() > 0` works.
        let r = if matches!(op, BinOpKind::And) {
            let (narrow, _) = self.narrowing_of(left, f);
            f.narrowed.push(narrow);
            let r = self.check_expr(right, f);
            f.narrowed.pop();
            r
        } else {
            self.check_expr(right, f)
        };
        let (sl, sr) = (self.store.shallow(&l), self.store.shallow(&r));
        let sym = op_symbol(op);
        match op {
            BinOpKind::Add | BinOpKind::Sub | BinOpKind::Mul | BinOpKind::Div | BinOpKind::Mod => {
                match (&sl, &sr) {
                    (Ty::Int, Ty::Int) => Ty::Int,
                    (Ty::Float, Ty::Float) | (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
                        Ty::Float
                    }
                    (Ty::Str | Ty::Char, Ty::Str | Ty::Char) if matches!(op, BinOpKind::Add) => {
                        // A char joins a string in either order, and two chars
                        // make a two-character string.
                        Ty::Str
                    }
                    (Ty::Var(_), Ty::Var(_)) => self.unify(&l, &r, span, sym),
                    (Ty::Var(_), Ty::Int | Ty::Float | Ty::Str) => {
                        self.unify(&l, &r, span, sym);
                        sr
                    }
                    (Ty::Int | Ty::Float | Ty::Str, Ty::Var(_)) => {
                        self.unify(&r, &l, span, sym);
                        sl
                    }
                    _ => {
                        let (a, b) = (self.store.resolve(&l), self.store.resolve(&r));
                        self.report(span, format!("'{sym}' cannot combine {a} and {b}"));
                        l
                    }
                }
            }
            BinOpKind::BitAnd
            | BinOpKind::BitOr
            | BinOpKind::BitXor
            | BinOpKind::Shl
            | BinOpKind::Shr => {
                self.unify(&l, &Ty::Int, span, sym);
                self.unify(&r, &Ty::Int, span, sym);
                Ty::Int
            }
            BinOpKind::And | BinOpKind::Or => {
                self.unify(&l, &Ty::Bool, span, sym);
                self.unify(&r, &Ty::Bool, span, sym);
                Ty::Bool
            }
            BinOpKind::Eq | BinOpKind::Ne => {
                let nil_side = is_nil_literal(left) || is_nil_literal(right);
                if nil_side {
                    let other = if is_nil_literal(left) { &sr } else { &sl };
                    match other {
                        Ty::Opt(_) | Ty::Nil | Ty::Var(_) | Ty::Json | Ty::Error => {}
                        t => {
                            let t = self.store.resolve(t);
                            self.report(
                                span,
                                format!(
                                    "{t} can never be nil, so '{sym} nil' is always the same answer"
                                ),
                            );
                        }
                    }
                    return Ty::Bool;
                }
                match (&sl, &sr) {
                    (Ty::Char, Ty::Str) | (Ty::Str, Ty::Char) => {}
                    _ => {
                        self.unify(
                            &l,
                            &r,
                            span,
                            &format!("'{sym}' (both sides must be the same type)"),
                        );
                    }
                }
                Ty::Bool
            }
            BinOpKind::Lt | BinOpKind::Gt | BinOpKind::Le | BinOpKind::Ge => {
                match (&sl, &sr) {
                    (Ty::Int | Ty::Float, Ty::Int | Ty::Float) => {}
                    (Ty::Str, Ty::Str) | (Ty::Char, Ty::Char) | (Ty::Bool, Ty::Bool) => {}
                    (Ty::Var(_), _) | (_, Ty::Var(_)) => {
                        self.unify(&l, &r, span, sym);
                    }
                    _ => {
                        let (a, b) = (self.store.resolve(&l), self.store.resolve(&r));
                        self.report(span, format!("'{sym}' cannot compare {a} and {b}"));
                    }
                }
                Ty::Bool
            }
            BinOpKind::In | BinOpKind::NotIn => {
                match &sr {
                    Ty::Array(e) => {
                        self.unify(e, &l, span, sym);
                    }
                    Ty::Map(_) => {
                        self.unify(&l, &Ty::Str, span, "a dict key");
                    }
                    Ty::Str => match &sl {
                        Ty::Str | Ty::Char => {}
                        _ => {
                            self.unify(&l, &Ty::Str, span, sym);
                        }
                    },
                    Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(other);
                        self.report(span, format!("'{sym}' cannot search {other}"));
                    }
                }
                Ty::Bool
            }
        }
    }

    /// Purpose: This function types a struct literal and joins its values into
    /// the struct's field slots.
    ///
    /// Inputs:
    /// - `type_name`: the struct.
    /// - `base`: the `...base` value, if any.
    /// - `fields`: the named fields.
    /// - `span`: where.
    /// - `f`: the frame.
    ///
    /// Outputs: the struct type.
    fn check_struct_literal(
        &mut self,
        type_name: &str,
        base: Option<&'a TExpr>,
        fields: &'a [(String, TExpr, bool)],
        span: Span,
        f: &mut Frame,
    ) -> Ty {
        if !self.structs.contains_key(type_name) {
            self.report(span, format!("unknown struct '{}'", super::ty::display_name(type_name)));
            return Ty::Struct(type_name.to_string());
        }
        self.constructed.insert(type_name.to_string());
        for (name, value, _) in fields {
            let t = self.check_expr(value, f);
            if self.structs[type_name].prompt_fields.contains(name) {
                self.expect_text(&t, value.span, "a prompt field");
                continue;
            }
            match self.field_slot(type_name, name) {
                Some(slot) => {
                    self.unify(&slot, &t, value.span, &format!("field '{name}'"));
                }
                None => self.report(
                    value.span,
                    format!(
                        "struct '{}' has no field '{name}'",
                        super::ty::display_name(type_name)
                    ),
                ),
            }
        }
        if let Some(b) = base {
            let bt = self.check_expr(b, f);
            if let Ty::Struct(bn) = self.store.shallow(&bt) {
                let named: Vec<&str> = fields.iter().map(|(n, _, _)| n.as_str()).collect();
                let all = self.structs[type_name].fields.clone();
                for field in all.iter().filter(|x| !named.contains(&x.as_str())) {
                    match (self.field_slot(type_name, field), self.field_slot(&bn, field)) {
                        (Some(dst), Some(src)) => {
                            self.unify(
                                &dst,
                                &src,
                                span,
                                &format!("field '{field}' copied from the base"),
                            );
                        }
                        (Some(_), None) => self.report(
                            span,
                            format!(
                                "the base value, a {}, has no field '{field}' to copy into {}",
                                super::ty::display_name(&bn),
                                super::ty::display_name(type_name)
                            ),
                        ),
                        _ => {}
                    }
                }
            } else if !matches!(self.store.shallow(&bt), Ty::Var(_)) {
                let r = self.store.resolve(&bt);
                self.report(span, format!("'...' copies fields from a struct, got {r}"));
            }
        }
        Ty::Struct(type_name.to_string())
    }

    /// Purpose: This function types `obj.field`.
    ///
    /// Inputs:
    /// - `object`: the receiver.
    /// - `field`: the field.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the access's address.
    ///
    /// Outputs: the field's type.
    fn check_field(
        &mut self,
        object: &'a TExpr,
        field: &str,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        if let TExprKind::Identifier(pkg) = &object.kind
            && self.read_var(f, pkg).is_none()
            && self.packages.contains(pkg.as_str())
        {
            return super::builtins::package_fn_value(self, pkg, field, span, f, node);
        }
        let ot = self.check_expr(object, f);
        self.field_of(&ot, field, span, f, node)
    }

    /// Purpose: This function types reading a field from a value of some type.
    ///
    /// Inputs:
    /// - `ot`: the receiver's type.
    /// - `field`: the field.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: an address for slots.
    ///
    /// Outputs: the field's type.
    pub(super) fn field_of(
        &mut self,
        ot: &Ty,
        field: &str,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        match self.store.shallow(ot) {
            Ty::Struct(sn) => {
                if self.structs.get(&sn).is_some_and(|i| i.prompt_fields.contains(field)) {
                    return Ty::Prompt;
                }
                if let Some(slot) = self.field_slot(&sn, field) {
                    return slot;
                }
                if self.find_method(&sn, field).is_some() {
                    return Ty::Func(FuncRef::Method {
                        recv: Box::new(Ty::Struct(sn)),
                        name: field.to_string(),
                    });
                }
                self.report(
                    span,
                    format!("struct '{}' has no field '{field}'", super::ty::display_name(&sn)),
                );
                self.slot(f, node)
            }
            Ty::Error => {
                if field == "message" {
                    Ty::Str
                } else {
                    self.report(
                        span,
                        format!(
                            "a caught error only has 'message'; catch the struct by its type to read \
                             '{field}'"
                        ),
                    );
                    self.slot(f, node)
                }
            }
            Ty::Var(_) => self.slot(f, node),
            Ty::Map(_) => {
                self.report(
                    span,
                    format!(
                        "a dict has no fields; read it with [\"{field}\"], or use a struct for named fields"
                    ),
                );
                self.slot(f, node)
            }
            Ty::Opt(inner) => {
                let inner = self.store.resolve(&inner);
                self.report(
                    span,
                    format!("this value may be nil; check it with 'if x != nil' before reading '{field}' from the {inner}"),
                );
                self.slot(f, node)
            }
            other => {
                let other = self.store.resolve(&other);
                if super::builtins::primitive_has_method(&other, field) {
                    return Ty::Func(FuncRef::Method {
                        recv: Box::new(other),
                        name: field.to_string(),
                    });
                }
                self.report(span, format!("{other} has no field '{field}'"));
                self.slot(f, node)
            }
        }
    }

    /// Purpose: This function types `obj[index]`.
    ///
    /// Inputs:
    /// - `object`, `index`: the parts.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the access's address.
    ///
    /// Outputs: the element's type.
    fn check_index(
        &mut self,
        object: &'a TExpr,
        index: &'a TExpr,
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        let ot = self.check_expr(object, f);
        let it = self.check_expr(index, f);
        match self.store.shallow(&ot) {
            Ty::Array(e) | Ty::Stream(e) => {
                self.unify(&it, &Ty::Int, span, "an index");
                *e
            }
            Ty::Str => {
                self.unify(&it, &Ty::Int, span, "a string index");
                Ty::Char
            }
            Ty::Bytes => {
                self.unify(&it, &Ty::Int, span, "a bytes index");
                Ty::Int
            }
            Ty::Map(v) => {
                self.unify(&it, &Ty::Str, span, "a dict key");
                *v
            }
            Ty::Json => {
                match self.store.shallow(&it) {
                    Ty::Str | Ty::Int | Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(
                            span,
                            format!("a json value is indexed by str or int, got {other}"),
                        );
                    }
                }
                Ty::Json
            }
            Ty::Struct(_) | Ty::Error => match &index.kind {
                // `r["status"]` on a struct reads the field, so a std result that
                // used to be a dict reads the same either way.
                TExprKind::Str(field) => self.field_of(&ot, field, span, f, node),
                _ => {
                    self.report(
                        span,
                        "a struct is indexed only by a field name written as a string literal"
                            .to_string(),
                    );
                    self.slot(f, node)
                }
            },
            Ty::Var(_) => self.slot(f, node),
            other => {
                let other = self.store.resolve(&other);
                self.report(span, format!("{other} cannot be indexed"));
                self.slot(f, node)
            }
        }
    }

    /// Purpose: This function finds the method a struct's value runs: its own,
    /// or the nearest ancestor's.
    ///
    /// Inputs:
    /// - `s`: the struct.
    /// - `name`: the method.
    ///
    /// Outputs: the struct whose `extend` block defines it, or `None`.
    pub(super) fn find_method(&self, s: &str, name: &str) -> Option<String> {
        if self.methods.contains_key(&(s.to_string(), name.to_string())) {
            return Some(s.to_string());
        }
        let ancestors = self.store.ancestors.get(s)?;
        ancestors
            .iter()
            .find(|a| self.methods.contains_key(&((*a).clone(), name.to_string())))
            .cloned()
    }

    /// Purpose: This function types a method call on a receiver.
    ///
    /// Inputs:
    /// - `recv`: the receiver's type.
    /// - `name`: the method.
    /// - `tys`: the argument types.
    /// - `args`: the argument expressions, when the call is written out.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: the call's type.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn method_call(
        &mut self,
        recv: &Ty,
        name: &str,
        tys: Vec<Ty>,
        args: &'a [TExpr],
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        match self.store.shallow(recv) {
            Ty::Struct(sn) => {
                // A field that holds a function: `obj.callback(x)`.
                if let Some(slot) = self.field_slot(&sn, name) {
                    return self.call_value(&slot, tys, span, f, node);
                }
                // Dispatch on every struct the receiver may actually hold.
                let mut kinds: Vec<String> =
                    self.constructed.iter().filter(|c| self.store.is_a(c, &sn)).cloned().collect();
                kinds.sort();
                if !kinds.contains(&sn) {
                    kinds.insert(0, sn.clone());
                }
                let result = self.slot(f, node);
                let mut targets = Vec::new();
                let mut found = false;
                for kind in kinds {
                    let Some(owner) = self.find_method(&kind, name) else { continue };
                    found = true;
                    let info = self.methods[&(owner.clone(), name.to_string())].clone();
                    let mut all = vec![Ty::Struct(kind.clone())];
                    all.extend(tys.iter().cloned());
                    let Some(all) = self.fill_defaults(&info.name, info.params, all, span) else {
                        return result;
                    };
                    let Some(k) =
                        self.instantiate(FnKey::Method(owner, name.to_string()), all, span)
                    else {
                        return result;
                    };
                    let r = self.instance_result(&k);
                    self.unify(
                        &result,
                        &r,
                        span,
                        &format!(
                            "the results of '{name}' across {}'s kinds",
                            super::ty::display_name(&sn)
                        ),
                    );
                    targets.push((kind, k));
                }
                if !found {
                    self.report(
                        span,
                        format!("struct '{}' has no method '{name}'", super::ty::display_name(&sn)),
                    );
                    return result;
                }
                let t = if targets.len() == 1 {
                    Target::Instance(targets.remove(0).1)
                } else {
                    Target::Dispatch(targets)
                };
                self.record_target(f, node, t);
                result
            }
            Ty::Var(_) => self.slot(f, node),
            Ty::Opt(inner) => {
                let inner = self.store.resolve(&inner);
                self.report(
                    span,
                    format!("this value may be nil; check it with 'if x != nil' before calling '{name}' on the {inner}"),
                );
                self.slot(f, node)
            }
            other => {
                self.record_target(f, node, Target::Builtin(format!("method:{name}")));
                super::builtins::primitive_method(self, &other, name, tys, args, span, f, node)
            }
        }
    }

    /// Purpose: This function types a method value called from a joined
    /// function type, outside any body.
    ///
    /// Inputs:
    /// - `recv`: the bound receiver's type.
    /// - `name`: the method.
    /// - `tys`: the argument types.
    /// - `span`: where.
    ///
    /// Outputs: the call's type, or `None`.
    fn method_result_detached(
        &mut self,
        recv: &Ty,
        name: &str,
        tys: Vec<Ty>,
        span: Span,
    ) -> Option<Ty> {
        let Ty::Struct(sn) = self.store.shallow(recv) else { return None };
        let owner = self.find_method(&sn, name)?;
        let info = self.methods[&(owner.clone(), name.to_string())].clone();
        let mut all = vec![Ty::Struct(sn)];
        all.extend(tys);
        let all = self.fill_defaults(&info.name, info.params, all, span)?;
        let k = self.instantiate(FnKey::Method(owner, name.to_string()), all, span)?;
        Some(self.instance_result(&k))
    }

    /// Purpose: This function types `join(f1, f2, …)`, which awaits several futures.
    ///
    /// Inputs:
    /// - `args`, `kwargs`: the call's arguments.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: an array of the results.
    fn check_join(
        &mut self,
        args: &'a [TExpr],
        kwargs: &'a [(String, TExpr)],
        span: Span,
        f: &mut Frame,
        node: usize,
    ) -> Ty {
        let inner = self.slot(f, node);
        for a in args {
            let t = self.check_expr(a, f);
            let fut = Ty::Future(Box::new(inner.clone()));
            self.unify(&fut, &t, a.span, "the futures given to join");
        }
        let mut settle = false;
        for (k, v) in kwargs {
            let t = self.check_expr(v, f);
            if k == "settle" {
                self.unify(&t, &Ty::Bool, v.span, "join's settle");
                settle = true;
            } else {
                self.report(v.span, format!("join has no parameter named '{k}'"));
            }
        }
        self.record_target(f, node, Target::Builtin("join".to_string()));
        if settle {
            let name = super::builtins::settled_struct(self, &inner);
            return Ty::Array(Box::new(Ty::Struct(name)));
        }
        let _ = span;
        Ty::Array(Box::new(inner))
    }

    /// Purpose: This function types `wait(futures)`, which answers the index of
    /// the first to finish. The futures may have different result types,
    /// since `wait` never reads a result.
    ///
    /// Inputs:
    /// - `args`: the call's arguments.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: `int`.
    fn check_wait(&mut self, args: &'a [TExpr], span: Span, f: &mut Frame, node: usize) -> Ty {
        self.record_target(f, node, Target::Builtin("wait".to_string()));
        if args.len() != 1 {
            self.report(
                span,
                format!("wait takes one array of futures, got {} arguments", args.len()),
            );
            return Ty::Int;
        }
        if let TExprKind::Array { elements } = &args[0].kind {
            for el in elements {
                let t = self.check_expr(el, f);
                match self.store.shallow(&t) {
                    Ty::Future(_) | Ty::Var(_) => {}
                    other => {
                        let other = self.store.resolve(&other);
                        self.report(el.span, format!("wait expects futures, got {other}"));
                    }
                }
            }
            return Ty::Int;
        }
        let t = self.check_expr(&args[0], f);
        let inner = self.slot(f, node);
        let want = Ty::Array(Box::new(Ty::Future(Box::new(inner))));
        self.unify(&want, &t, span, "wait's argument");
        Ty::Int
    }

    /// Purpose: This function types a call into a native package.
    ///
    /// Inputs:
    /// - `name`: the `__native$<pkg>$<fn>` reference.
    /// - `span`: where.
    /// - `f`: the frame.
    /// - `node`: the call's address.
    ///
    /// Outputs: the call's type.
    fn native_call(&mut self, name: &str, span: Span, f: &mut Frame, node: usize) -> Ty {
        // C bindings declare their types in jade.toml; feeding those to the
        // checker is part of the package work (design decision 11). Until then a
        // native call's result is whatever its use makes it.
        let _ = (name, span);
        self.slot(f, node)
    }
}

/// Purpose: This function says whether control can reach the end of a body.
///
/// Inputs:
/// - `body`: the statements.
///
/// Outputs: true when the body may finish without a `return` or `raise`.
fn falls_off(body: &[TStmt]) -> bool {
    match body.last() {
        None => true,
        Some(TStmt::Return { .. }) | Some(TStmt::Raise { .. }) => false,
        Some(TStmt::If { then_body, else_body: Some(e), .. }) => {
            falls_off(then_body) || falls_off(e)
        }
        Some(TStmt::TryCatch { body, arms, .. }) => {
            falls_off(body) || arms.iter().any(|a| falls_off(&a.body))
        }
        Some(TStmt::While { condition, body, .. }) => {
            // `while true { … }` with no break never falls off.
            !(matches!(condition.kind, TExprKind::Bool(true)) && !has_break(body))
        }
        _ => true,
    }
}

/// Purpose: This function says whether a block always leaves: it ends in a
/// `return`, `raise`, `break` or `continue`.
///
/// Inputs:
/// - `body`: the statements.
///
/// Outputs: true when control never reaches the end of the block.
fn exits(body: &[TStmt]) -> bool {
    match body.last() {
        Some(
            TStmt::Return { .. }
            | TStmt::Raise { .. }
            | TStmt::Break { .. }
            | TStmt::Continue { .. },
        ) => true,
        Some(TStmt::If { then_body, else_body: Some(e), .. }) => exits(then_body) && exits(e),
        _ => false,
    }
}

/// Purpose: This function says whether a loop body breaks out of its own loop.
///
/// Inputs:
/// - `body`: the loop's statements.
///
/// Outputs: true when a `break` reaches this loop.
fn has_break(body: &[TStmt]) -> bool {
    body.iter().any(|s| match s {
        TStmt::Break { .. } => true,
        TStmt::If { then_body, else_body, .. } => {
            has_break(then_body) || else_body.as_deref().is_some_and(has_break)
        }
        TStmt::TryCatch { body, arms, .. } => {
            has_break(body) || arms.iter().any(|a| has_break(&a.body))
        }
        _ => false,
    })
}

/// Purpose: This function gives a parameter default's type, which is always a literal.
///
/// Inputs:
/// - `e`: the default.
///
/// Outputs: its type.
fn literal_type(e: &TExpr) -> Ty {
    match &e.kind {
        TExprKind::Integer(_) => Ty::Int,
        TExprKind::Float(_) => Ty::Float,
        TExprKind::Bool(_) => Ty::Bool,
        TExprKind::Str(_) => Ty::Str,
        _ => Ty::Nil,
    }
}

/// Purpose: This function spells an operator for an error message.
///
/// Inputs:
/// - `op`: the operator.
///
/// Outputs: its symbol.
fn op_symbol(op: &BinOpKind) -> &'static str {
    match op {
        BinOpKind::Add => "+",
        BinOpKind::Sub => "-",
        BinOpKind::Mul => "*",
        BinOpKind::Div => "/",
        BinOpKind::Mod => "%",
        BinOpKind::BitAnd => "&",
        BinOpKind::BitOr => "|",
        BinOpKind::BitXor => "^",
        BinOpKind::Shl => "<<",
        BinOpKind::Shr => ">>",
        BinOpKind::And => "&&",
        BinOpKind::Or => "||",
        BinOpKind::Eq => "==",
        BinOpKind::Ne => "!=",
        BinOpKind::Lt => "<",
        BinOpKind::Gt => ">",
        BinOpKind::Le => "<=",
        BinOpKind::Ge => ">=",
        BinOpKind::In => "in",
        BinOpKind::NotIn => "not in",
    }
}

/// Purpose: This function adds a suggestion to a mismatch the language has an
/// answer for.
///
/// Inputs:
/// - `a`, `b`: the two types that did not join.
///
/// Outputs: a sentence starting with "; ", or nothing.
fn mismatch_hint(a: &Ty, b: &Ty) -> String {
    let structs = matches!(a, Ty::Struct(_)) && matches!(b, Ty::Struct(_));
    if structs {
        return "; give both structs a shared parent struct to keep them together".to_string();
    }
    if matches!((a, b), (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int)) {
        return "; convert one with float(x) or int(x)".to_string();
    }
    String::new()
}
