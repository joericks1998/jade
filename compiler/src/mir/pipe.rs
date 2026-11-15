//! Lowering a pipe, `source |> f |> g(a)`, which calls its stages once per
//! piece of its source as each piece arrives. See `design/v2-pipes.md`.
//!
//! A pipe lowers one of three ways, by what its source is:
//!
//! - *One piece,* any value the checker did not type as a stream: the stages
//!   are calls, made once, and the pipe's value is what the last one gives.
//! - *A stream that has arrived,* such as a variable holding one: a loop calls
//!   the stages on each value and collects what they give back.
//! - *A call to a generator:* the stages become a function of their own, and a
//!   copy of the generator calls it at each `yield`, so each value goes through
//!   the stages before the generator makes the next. The copy takes what the
//!   stages need from the caller, its variables and the stage arguments worked
//!   out once, as extra parameters, so no closure is needed. A prompt's reply
//!   arrives this way too: `src/infer/rewrite.rs` makes `?p |> f` a pipe over
//!   the prelude's `reply_pieces` generator.

use std::collections::{HashMap, HashSet};

use super::lower::{FnDef, Fx, FxSink, Lowerer, Sink, addr, inst_label, internal};
use super::*;
use crate::frontend::ast::{Expr, ExprKind, FStrPart};
use crate::frontend::error::{JadeError, Span};
use crate::typechecker::infer::{InstKey, Target, display_type_name};

type Result<T> = std::result::Result<T, JadeError>;

/// Purpose: This struct holds a pipe's source when it is a direct call to a
/// generator: the instance called and the call's parts.
struct GenCall<'a> {
    key: InstKey,
    def: FnDef<'a>,
    callee: &'a Expr,
    args: &'a [Expr],
    kwargs: &'a [(String, Expr)],
}

impl<'a, 'l> Fx<'a, 'l> {
    /// Purpose: This function lowers a pipe.
    ///
    /// Inputs:
    /// - `e`: the pipe node, whose address names the piece's slot.
    /// - `et`: the pipe's type.
    /// - `source`, `hoisted`, `piece`, `body`: its parts.
    ///
    /// Outputs: the pipe's value and type.
    pub(super) fn pipe(
        &mut self,
        e: &'a Expr,
        et: &Ty,
        source: &'a Expr,
        hoisted: &'a [(String, Expr)],
        piece: &'a str,
        body: &'a Expr,
    ) -> Result<(Operand, Ty)> {
        let span = e.span;
        let streamed = self.l.streams.contains(&(self.key.clone(), addr(source)));
        if streamed && let Some(call) = self.generator_call(source) {
            self.scopes.push(HashMap::new());
            let r = self.live_pipe(e, et, call, hoisted, piece, body);
            self.scopes.pop();
            return r.map(|r| (r, et.clone()));
        }
        let (src, st) = self.expr(source)?;
        self.scopes.push(HashMap::new());
        let r = self.bind_hoisted(hoisted).and_then(|()| {
            if streamed {
                self.held_pipe(e, et, src, &st, piece, body)
            } else {
                let (pp, pt) = self.declare(piece, addr(e) + 1, span)?;
                self.store(pp, &pt, src, &st, span)?;
                let (v, t) = self.expr(body)?;
                self.coerce(v, &t, et, span)
            }
        });
        self.scopes.pop();
        Ok((r?, et.clone()))
    }

    /// Purpose: This function lowers a pipe over a call to a generator: the
    /// copy of the generator whose `yield` calls the stages, made the first
    /// time, and a call to it.
    ///
    /// Inputs:
    /// - `e`, `et`: the pipe and its type, a stream of what the stages give.
    /// - `call`: the generator call.
    /// - `hoisted`, `piece`, `body`: the pipe's parts.
    ///
    /// Outputs: the stream of what the stages gave.
    fn live_pipe(
        &mut self,
        e: &'a Expr,
        et: &Ty,
        call: GenCall<'a>,
        hoisted: &'a [(String, Expr)],
        piece: &'a str,
        body: &'a Expr,
    ) -> Result<Operand> {
        let span = e.span;
        let GenCall { key, def, callee, args, kwargs } = call;
        // The generator's own arguments first, as a call would pass them, then
        // the stages' arguments, before the generator makes its first value.
        let vals = self.instance_vals(&key, def, callee, args, kwargs, span)?;
        let mut argv = self.instance_args(&key, def, vals, span)?;
        self.bind_hoisted(hoisted)?;

        // What the stages read from this function: its variables, by name, in
        // the order they first appear, which is the same each time the pipe is
        // lowered. Globals and functions are reachable from any function.
        let mut names = Vec::new();
        free_names(body, &mut names);
        let mut seen = HashSet::new();
        let mut captured: Vec<(String, Local, Ty)> = Vec::new();
        for n in names {
            if n == piece || !seen.insert(n.clone()) {
                continue;
            }
            if let Some((Place::Local(l), t)) = self.lookup(&n) {
                captured.push((n, l, t));
            }
        }
        let piece_ty = self
            .slots
            .get(&(addr(e) + 1))
            .cloned()
            .ok_or_else(|| internal("a pipe's piece with no type", span))?;
        let Ty::Array(out) = et else {
            return Err(internal(format!("a pipe over a generator giving {et}"), span));
        };

        let site = (self.key.clone(), addr(e));
        let made = self.l.pipes.borrow().get(&site).copied();
        let copy = match made {
            Some((_, copy)) => copy,
            None => {
                // Named for the pipe as written, `two |> bump`, which is what a
                // message about the copy, such as the task check's, quotes; the
                // place follows in parentheses, which messages leave off.
                let at = format!("{} at {}:{}", inst_label(&self.key), span.line, span.col);
                let mut written = vec![
                    display_type_name(inst_label(&key).split('(').next().unwrap_or_default())
                        .to_string(),
                ];
                stage_names(body, &mut written);
                let written = written.join(" |> ");
                let copy_name = format!("{written}(piped in {at})");
                let stages_name = format!("|> stages of {written}(in {at})");
                let stages = self.l.reserve(stages_name.clone());
                let copy = self.l.reserve(copy_name.clone());
                // Recorded before either is lowered, so a generator that pipes
                // itself finds the copy already being made.
                self.l.pipes.borrow_mut().insert(site, (stages, copy));
                let named: Vec<(String, Ty)> =
                    captured.iter().map(|(n, _, t)| (n.clone(), t.clone())).collect();
                let f = self.l.stages_fn(
                    &self.key,
                    stages_name,
                    (piece, &piece_ty),
                    &named,
                    body,
                    out,
                    span,
                )?;
                self.l.fill(stages, f);
                let g = self.l.lower_instance_with(
                    &key,
                    Some(Sink {
                        name: copy_name,
                        stages,
                        piece: piece_ty,
                        captured: named.into_iter().map(|(_, t)| t).collect(),
                        out: (**out).clone(),
                    }),
                )?;
                self.l.fill(copy, g);
                copy
            }
        };
        argv.extend(captured.iter().map(|(_, l, _)| Operand::Copy(Place::Local(*l))));
        Ok(self.assign_temp(et.clone(), Rvalue::Call(copy, argv)))
    }

    /// Purpose: This function lowers a pipe over a stream that has already
    /// arrived: a loop that calls the stages on each value in order.
    ///
    /// Inputs:
    /// - `e`, `et`: the pipe and its type, a stream of what the stages give.
    /// - `src`, `st`: the stream and its type.
    /// - `piece`, `body`: the pipe's parts.
    ///
    /// Outputs: the stream of what the stages gave.
    fn held_pipe(
        &mut self,
        e: &'a Expr,
        et: &Ty,
        src: Operand,
        st: &Ty,
        piece: &str,
        body: &'a Expr,
    ) -> Result<Operand> {
        let span = e.span;
        let (Ty::Array(el), Ty::Array(out_el)) = (st, et) else {
            return Err(internal(format!("a pipe over {st} giving {et}"), span));
        };
        let (el, out_el) = ((**el).clone(), (**out_el).clone());
        // The stream is kept in a local of its own so the loop owns it for as
        // long as it runs, as a `for` loop does.
        let held = self.new_local(st.clone(), None);
        let src = self.consume(src);
        self.emit(Stmt::Assign(Place::Local(held), Rvalue::Use(src)));
        let out = self.temp(et.clone());
        self.emit(Stmt::Assign(Place::Local(out), Rvalue::Array(out_el.clone(), Vec::new())));
        let idx = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(Place::Local(idx), Rvalue::Use(Operand::Const(Const::Int(0)))));
        let len = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(
            Place::Local(len),
            Rvalue::Op(Op::ArrayLen, vec![Operand::Copy(Place::Local(held))]),
        ));

        let head = self.new_block();
        let each = self.new_block();
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
        self.terminate(Term::If(Operand::Copy(Place::Local(more)), each, exit));

        self.switch_to(each);
        let (pp, pt) = self.declare(piece, addr(e) + 1, span)?;
        let mark = self.temps.len();
        let v = self.assign_temp(
            el.clone(),
            Rvalue::Op(
                Op::ArrayGet(el.clone()),
                vec![Operand::Copy(Place::Local(held)), Operand::Copy(Place::Local(idx))],
            ),
        );
        self.store(pp, &pt, v, &el, span)?;
        let (r, rt) = self.expr(body)?;
        let r = self.coerce(r, &rt, &out_el, span)?;
        let r = self.consume(r);
        self.emit(Stmt::Eval(Rvalue::Op(
            Op::ArrayPush(out_el),
            vec![Operand::Copy(Place::Local(out)), r],
        )));
        self.drop_temps(mark);
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
        self.fresh.insert(out);
        Ok(Operand::Copy(Place::Local(out)))
    }

    /// Purpose: This function works out a pipe's hoisted stage arguments, once,
    /// into variables of the names its stages use.
    ///
    /// Inputs:
    /// - `hoisted`: the arguments, by name.
    ///
    /// Outputs: `Ok(())` or an error.
    fn bind_hoisted(&mut self, hoisted: &'a [(String, Expr)]) -> Result<()> {
        for (name, v) in hoisted {
            let (o, t) = self.expr(v)?;
            let (p, pt) = self.declare(name, addr(v) + 1, v.span)?;
            self.store(p, &pt, o, &t, v.span)?;
        }
        Ok(())
    }

    /// Purpose: This function finds a pipe source that is a direct call to a
    /// generator, which the pipe can run one value at a time.
    ///
    /// Inputs:
    /// - `source`: the pipe's source.
    ///
    /// Outputs: the call, or `None` for anything else, such as a call through
    /// a function value, whose stream arrives whole.
    fn generator_call(&self, source: &'a Expr) -> Option<GenCall<'a>> {
        let ExprKind::Call { callee, args, kwargs } = &source.kind else {
            return None;
        };
        let Some(Target::Instance(k)) = self.targets.get(&addr(source)) else {
            return None;
        };
        let def = self.l.def_of(&k.func)?;
        if def.is_async || !super::ops::body_yields(def.body) {
            return None;
        }
        Some(GenCall { key: k.clone(), def, callee, args, kwargs })
    }

    /// Purpose: This function lowers a `yield` in a generator's copy made for
    /// a pipe: the value goes through the stages, and what they give back is
    /// collected.
    ///
    /// A raise in a stage leaves the generator at once. The generator's own
    /// `try` blocks guard its own work, and the stages are the caller's code,
    /// so the raise goes out of the copy, to where the pipe is written.
    ///
    /// Inputs:
    /// - `value`: the yielded value.
    /// - `sink`: the stages and what they capture.
    /// - `s`, `el`: the collected stream and its element type.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())` or an error.
    pub(super) fn yield_to_stages(
        &mut self,
        value: &'a Expr,
        sink: &FxSink,
        s: Local,
        el: &Ty,
        span: Span,
    ) -> Result<()> {
        let (v, t) = self.expr(value)?;
        let v = self.coerce(v, &t, &sink.piece, span)?;
        let mut argv = vec![v];
        argv.extend(sink.captured.iter().map(|l| Operand::Copy(Place::Local(*l))));
        let handlers = std::mem::take(&mut self.handlers);
        let r = self.assign_temp(el.clone(), Rvalue::Call(sink.stages, argv));
        self.handlers = handlers;
        let r = self.consume(r);
        self.emit(Stmt::Eval(Rvalue::Op(
            Op::ArrayPush(el.clone()),
            vec![Operand::Copy(Place::Local(s)), r],
        )));
        Ok(())
    }
}

impl<'a> Lowerer<'a> {
    /// Purpose: This function lowers a pipe's stages into a function of their
    /// own, which a generator's copy calls once per value it yields.
    ///
    /// The stages are read with the answers of the instance the pipe is in,
    /// since that is where the checker checked them.
    ///
    /// Inputs:
    /// - `caller`: the instance the pipe is in.
    /// - `name`: the function's name.
    /// - `piece`: the piece's name and type, the first parameter.
    /// - `captured`: the caller's variables the stages read, by name, the
    ///   parameters after it.
    /// - `body`: the stages applied to the piece.
    /// - `ret`: what the stages give back.
    /// - `span`: where.
    ///
    /// Outputs: the function.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stages_fn(
        &self,
        caller: &InstKey,
        name: String,
        piece: (&str, &Ty),
        captured: &[(String, Ty)],
        body: &'a Expr,
        ret: &Ty,
        span: Span,
    ) -> Result<Func> {
        let mut fx = self.new_fx(caller, ret.clone());
        fx.is_main = false;
        fx.ret = ret.clone();
        fx.span = span;
        let l = fx.new_local(piece.1.clone(), Some(piece.0.to_string()));
        fx.scopes[0].insert(piece.0.to_string(), l);
        for (n, t) in captured {
            let l = fx.new_local(t.clone(), Some(n.clone()));
            fx.scopes[0].insert(n.clone(), l);
        }
        let nparams = fx.locals.len() as u32;
        let (v, t) = fx.expr(body)?;
        let v = fx.coerce(v, &t, ret, span)?;
        let v = fx.consume(v);
        fx.terminate(Term::Return(v));
        Ok(Func {
            name,
            params: nparams,
            locals: fx.locals,
            ret: ret.clone(),
            blocks: fx.blocks,
            library: false,
        })
    }
}

/// Purpose: This function lists a pipe's stages as written, first stage
/// first, for the name of the functions made for it: `bump` for `two |> bump`.
///
/// Inputs:
/// - `body`: the stages applied to the piece, `g(f(piece))`.
/// - `out`: the names so far.
///
/// Outputs: none.
fn stage_names(body: &Expr, out: &mut Vec<String>) {
    let ExprKind::Call { callee, args, .. } = &body.kind else {
        return;
    };
    if let Some(inner) = args.first() {
        stage_names(inner, out);
    }
    out.push(match &callee.kind {
        ExprKind::Identifier(n) => display_type_name(n).to_string(),
        ExprKind::FieldAccess { object, field } => match &object.kind {
            ExprKind::Identifier(o) => format!("{}.{field}", display_type_name(o)),
            _ => field.clone(),
        },
        _ => "a function".to_string(),
    });
}

/// Purpose: This function lists the names an expression reads, in the order
/// they appear, leaving out the names a pipe inside it binds for itself.
///
/// Inputs:
/// - `e`: the expression.
/// - `out`: the names so far.
///
/// Outputs: none.
fn free_names(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Identifier(n) => out.push(n.clone()),
        ExprKind::Integer(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => {}
        ExprKind::Is { value, .. } => free_names(value, out),
        ExprKind::Call { callee, args, kwargs } => {
            free_names(callee, out);
            for a in args {
                free_names(a, out);
            }
            for (_, v) in kwargs {
                free_names(v, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            free_names(left, out);
            free_names(right, out);
        }
        ExprKind::UnaryOp { operand, .. } => free_names(operand, out),
        ExprKind::StructLiteral { base, fields, .. } => {
            if let Some(b) = base {
                free_names(b, out);
            }
            for (_, v, _) in fields {
                free_names(v, out);
            }
        }
        ExprKind::FieldAccess { object, .. } => free_names(object, out),
        ExprKind::Index { object, index } => {
            free_names(object, out);
            free_names(index, out);
        }
        ExprKind::Array { elements } => {
            for x in elements {
                free_names(x, out);
            }
        }
        ExprKind::FStr { parts } => {
            for p in parts {
                if let FStrPart::Expr(x) = p {
                    free_names(x, out);
                }
            }
        }
        ExprKind::PromptDeref { expr, grammar_expr, .. } => {
            free_names(expr, out);
            if let Some(g) = grammar_expr {
                free_names(g, out);
            }
        }
        ExprKind::Dict { entries } => {
            for (k, v) in entries {
                free_names(k, out);
                free_names(v, out);
            }
        }
        ExprKind::Await { expr } => free_names(expr, out),
        ExprKind::PromptLiteral { body } => free_names(body, out),
        ExprKind::Pipe { source, hoisted, piece, body } => {
            free_names(source, out);
            for (_, v) in hoisted {
                free_names(v, out);
            }
            let mut inner = Vec::new();
            free_names(body, &mut inner);
            out.extend(
                inner.into_iter().filter(|n| n != piece && !hoisted.iter().any(|(h, _)| h == n)),
            );
        }
        // Never reaches MIR: lowering it reports an internal error.
        ExprKind::RawPipe { value, stage } => {
            free_names(value, out);
            free_names(stage, out);
        }
    }
}
