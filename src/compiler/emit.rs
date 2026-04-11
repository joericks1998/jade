use std::{collections::HashMap, rc::Rc};

use crate::{
    compiler::{
        bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
        tir::{JadeType, TExpr, TExprKind, TFStrPart, TProgram, TStmt},
    },
    interpreter::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{Result, Span},
    },
};

// ── Public output ─────────────────────────────────────────────────────────────

/// The output of compiling a `TProgram` — everything the VM needs to execute.
pub struct CompiledProgram {
    /// Top-level code (globals, control flow, expressions at file scope).
    pub top: Chunk,
    /// Total register slots needed for the top-level frame.
    pub top_n_slots: u32,
    /// Struct field definitions (needed by the VM for struct instantiation and
    /// field-access method fallback).
    pub struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Compiled extend-block methods: `type_name → method_name → CompiledFn`.
    pub extend_methods: HashMap<String, HashMap<String, Rc<CompiledFn>>>,
    /// Interface definitions (method names required per interface).
    pub interface_defs: HashMap<String, Vec<String>>,
}

// ── Internal state ────────────────────────────────────────────────────────────

/// Shared context threaded through the whole compilation.
struct EmitCtx {
    struct_defs: HashMap<String, Vec<StructFieldDef>>,
    interface_defs: HashMap<String, Vec<String>>,
    extend_methods: HashMap<String, HashMap<String, Rc<CompiledFn>>>,
}

/// Per-chunk compilation state.
struct Emitter {
    chunk: Chunk,
    /// Next register to allocate.
    next_reg: u32,
    /// `None` = top-level code: `let` → globals.
    /// `Some` = function body: `let` → frame slots.
    locals: Option<HashMap<String, u32>>,
}

impl Emitter {
    fn new_top() -> Self {
        Emitter { chunk: Chunk::new("<top>"), next_reg: 0, locals: None }
    }

    fn new_fn(name: &str) -> Self {
        Emitter { chunk: Chunk::new(name), next_reg: 0, locals: Some(HashMap::new()) }
    }

    fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    /// Allocate a slot for a local variable and record the binding.
    fn define_local(&mut self, name: &str) -> u32 {
        let slot = self.next_reg;
        self.next_reg += 1;
        if let Some(locals) = &mut self.locals {
            locals.insert(name.to_string(), slot);
        }
        slot
    }

    fn lookup_local(&self, name: &str) -> Option<u32> {
        self.locals.as_ref()?.get(name).copied()
    }

    fn in_fn(&self) -> bool {
        self.locals.is_some()
    }

    /// Emit instructions to load `name` into a fresh register. Returns the reg.
    fn emit_load_var(&mut self, name: &str, span: Span) -> Reg {
        let dest = self.alloc_reg();
        if let Some(slot) = self.lookup_local(name) {
            self.chunk.emit(Instr::GetLocal(dest, slot), span);
        } else {
            self.chunk.emit(Instr::GetGlobal(dest, name.to_string()), span);
        }
        dest
    }

    /// Emit instructions to store `src` into `name` (local or global).
    fn emit_store_var(&mut self, name: &str, src: Reg, span: Span) {
        if let Some(locals) = &self.locals {
            if let Some(&slot) = locals.get(name) {
                self.chunk.emit(Instr::SetLocal(slot, src), span);
                return;
            }
        }
        self.chunk.emit(Instr::SetGlobal(name.to_string(), src), span);
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Compile a `TProgram` into bytecode ready for the VM.
pub fn emit(program: TProgram) -> Result<CompiledProgram> {
    // First pass: collect static metadata (struct/interface definitions).
    let mut ctx = EmitCtx {
        struct_defs: HashMap::new(),
        interface_defs: HashMap::new(),
        extend_methods: HashMap::new(),
    };
    for stmt in &program.stmts {
        match stmt {
            TStmt::StructDef { name, fields, .. } => {
                ctx.struct_defs.insert(name.clone(), fields.clone());
            }
            TStmt::InterfaceDef { name, methods, .. } => {
                ctx.interface_defs.insert(
                    name.clone(),
                    methods.iter().map(|m| m.name.clone()).collect(),
                );
            }
            _ => {}
        }
    }

    // Second pass: emit all statements into the top-level chunk.
    let mut em = Emitter::new_top();
    let dummy = Span { line: 0, col: 0 };
    for stmt in program.stmts {
        emit_stmt(stmt, &mut em, &mut ctx)?;
    }
    em.chunk.emit(Instr::Halt, dummy);

    let n_slots = em.next_reg;
    Ok(CompiledProgram {
        top_n_slots: n_slots,
        top: em.chunk,
        struct_defs: ctx.struct_defs,
        extend_methods: ctx.extend_methods,
        interface_defs: ctx.interface_defs,
    })
}

// ── Statement emission ────────────────────────────────────────────────────────

fn emit_stmt(stmt: TStmt, em: &mut Emitter, ctx: &mut EmitCtx) -> Result<()> {
    let dummy = Span { line: 0, col: 0 };
    match stmt {
        TStmt::Let { name, value, span } => {
            let src = emit_expr(&value, em, ctx)?;
            if em.in_fn() {
                let slot = em.define_local(&name);
                em.chunk.emit(Instr::SetLocal(slot, src), span);
            } else {
                em.chunk.emit(Instr::SetGlobal(name, src), span);
            }
        }

        TStmt::Assign { name, value, span } => {
            let src = emit_expr(&value, em, ctx)?;
            em.emit_store_var(&name, src, span);
        }

        TStmt::FnDef { name, params, body, span, .. } => {
            let compiled = emit_fn(&name, params, body, span, ctx)?;
            let rc = Rc::new(compiled);
            let idx = em.chunk.intern_fn(Rc::clone(&rc));
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadFn(dest, idx), span);
            if em.in_fn() {
                let slot = em.define_local(&name);
                em.chunk.emit(Instr::SetLocal(slot, dest), span);
            } else {
                em.chunk.emit(Instr::SetGlobal(name, dest), span);
            }
        }

        TStmt::Return { value, span } => {
            match value {
                Some(expr) => {
                    let src = emit_expr(&expr, em, ctx)?;
                    em.chunk.emit(Instr::Return(Some(src)), span);
                }
                None => {
                    em.chunk.emit(Instr::Return(None), span);
                }
            }
        }

        TStmt::If { condition, then_body, else_body, span } => {
            let cond = emit_expr(&condition, em, ctx)?;
            let jump_else = em.chunk.emit(Instr::JumpIfFalse(cond, 0), span);

            for s in then_body {
                emit_stmt(s, em, ctx)?;
            }

            if let Some(else_stmts) = else_body {
                let jump_end = em.chunk.emit(Instr::Jump(0), span);
                em.chunk.patch_jump(jump_else, em.chunk.len());
                for s in else_stmts {
                    emit_stmt(s, em, ctx)?;
                }
                em.chunk.patch_jump(jump_end, em.chunk.len());
            } else {
                em.chunk.patch_jump(jump_else, em.chunk.len());
            }
        }

        TStmt::While { condition, body, span } => {
            let loop_start = em.chunk.len();
            let cond = emit_expr(&condition, em, ctx)?;
            let jump_exit = em.chunk.emit(Instr::JumpIfFalse(cond, 0), span);

            for s in body {
                emit_stmt(s, em, ctx)?;
            }

            // Back-jump: offset = loop_start − (current + 1)
            let back = loop_start as i32 - (em.chunk.len() as i32 + 1);
            em.chunk.emit(Instr::Jump(back), span);
            em.chunk.patch_jump(jump_exit, em.chunk.len());
        }

        TStmt::For { var, iterable, body, span } => {
            // Evaluate the iterable into a register.
            let iter_reg = emit_expr(&iterable, em, ctx)?;

            // idx = 0
            let idx_reg = em.alloc_reg();
            em.chunk.emit(Instr::LoadInt(idx_reg, 0), span);

            // len = len(iter)
            let len_reg = em.alloc_reg();
            em.chunk.emit(Instr::CallLen(len_reg, iter_reg), span);

            // Allocate the loop variable: a local slot inside fn, a scratch reg at top level.
            let x_reg = if em.in_fn() {
                em.define_local(&var)
            } else {
                em.alloc_reg()
            };

            // ── Loop header ────────────────────────────────────────────────────
            let loop_start = em.chunk.len();

            let cond_reg = em.alloc_reg();
            em.chunk.emit(Instr::CmpLtInt(cond_reg, idx_reg, len_reg), span);
            let jump_exit = em.chunk.emit(Instr::JumpIfFalse(cond_reg, 0), span);

            // x = iter[idx]
            em.chunk.emit(Instr::GetIndex(x_reg, iter_reg, idx_reg), span);
            // At top level, also write to the global so body code can read it.
            if !em.in_fn() {
                em.chunk.emit(Instr::SetGlobal(var.clone(), x_reg), span);
            }

            // ── Loop body ──────────────────────────────────────────────────────
            for s in body {
                emit_stmt(s, em, ctx)?;
            }

            // ── Increment: idx = idx + 1 ───────────────────────────────────────
            let one_reg = em.alloc_reg();
            em.chunk.emit(Instr::LoadInt(one_reg, 1), span);
            let next_idx = em.alloc_reg();
            em.chunk.emit(Instr::AddInt(next_idx, idx_reg, one_reg), span);
            em.chunk.emit(Instr::Move(idx_reg, next_idx), span);

            // Back-jump and patch exit.
            let back = loop_start as i32 - (em.chunk.len() as i32 + 1);
            em.chunk.emit(Instr::Jump(back), span);
            em.chunk.patch_jump(jump_exit, em.chunk.len());
        }

        // Metadata only — already captured in the first pass.
        TStmt::StructDef { .. } | TStmt::InterfaceDef { .. } => {}

        TStmt::ExtendBlock { type_name, methods, .. } => {
            // Compile all methods first (releasing the ctx borrow from extend_methods),
            // then insert them all at once.
            let mut compiled_methods: Vec<(String, CompiledFn)> = Vec::new();
            for method_stmt in methods {
                if let TStmt::FnDef { name, params, body, span, .. } = method_stmt {
                    let compiled = emit_fn(&name, params, body, span, ctx)?;
                    compiled_methods.push((name, compiled));
                }
            }
            let method_map = ctx.extend_methods.entry(type_name.clone()).or_default();
            for (name, compiled) in compiled_methods {
                method_map.insert(name, Rc::new(compiled));
            }
        }

        TStmt::FieldAssign { object, field, value, span } => {
            let val = emit_expr(&value, em, ctx)?;
            let obj = em.emit_load_var(&object, span);
            em.chunk.emit(Instr::SetField(obj, field, val), span);
        }

        TStmt::IndexAssign { name, index, value, span } => {
            let obj = em.emit_load_var(&name, span);
            let idx = emit_expr(&index, em, ctx)?;
            let val = emit_expr(&value, em, ctx)?;
            em.chunk.emit(Instr::SetIndex(obj, idx, val), span);
            // Write the (value-semantics) modified collection back.
            em.emit_store_var(&name, obj, span);
        }

        TStmt::PromptDecl { name, body, span } => {
            let text = emit_expr(&body, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakePrompt(dest, text), span);
            if em.in_fn() {
                let slot = em.define_local(&name);
                em.chunk.emit(Instr::SetLocal(slot, dest), span);
            } else {
                em.chunk.emit(Instr::SetGlobal(name, dest), span);
            }
        }

        TStmt::Use { path, span } => {
            em.chunk.emit(Instr::ImportFile(path), span);
        }

        TStmt::Expr(expr) => {
            emit_expr(&expr, em, ctx)?;
        }
    }
    let _ = dummy;
    Ok(())
}

// ── Function compilation ──────────────────────────────────────────────────────

fn emit_fn(
    name: &str,
    params: Vec<String>,
    body: Vec<TStmt>,
    span: Span,
    ctx: &mut EmitCtx,
) -> Result<CompiledFn> {
    let mut fn_em = Emitter::new_fn(name);
    // Allocate slots for parameters first (slots 0..params.len()).
    for param in &params {
        fn_em.define_local(param);
    }
    for stmt in body {
        emit_stmt(stmt, &mut fn_em, ctx)?;
    }
    // Implicit return if execution falls off the end.
    fn_em.chunk.emit(Instr::Return(None), span);
    let n_slots = fn_em.next_reg;
    Ok(CompiledFn { params, chunk: fn_em.chunk, n_slots })
}

// ── Expression emission ───────────────────────────────────────────────────────

/// Emit instructions for `expr` and return the register holding the result.
fn emit_expr(expr: &TExpr, em: &mut Emitter, ctx: &mut EmitCtx) -> Result<Reg> {
    let span = expr.span;
    match &expr.kind {
        TExprKind::Integer(n) => {
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadInt(dest, *n), span);
            Ok(dest)
        }

        TExprKind::Float(f) => {
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadFloat(dest, *f), span);
            Ok(dest)
        }

        TExprKind::Bool(b) => {
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadBool(dest, *b), span);
            Ok(dest)
        }

        TExprKind::Str(s) => {
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadStr(dest, s.clone()), span);
            Ok(dest)
        }

        TExprKind::Identifier(name) => {
            // Builtin functions are handled at call sites; identifiers just load.
            Ok(em.emit_load_var(name, span))
        }

        TExprKind::Call { callee, args } => emit_call(callee, args, expr, em, ctx),

        TExprKind::BinOp { op, left, right } => emit_binop(op, left, right, em, ctx, span),

        TExprKind::UnaryOp { op, operand } => emit_unaryop(op, operand, em, ctx, span),

        TExprKind::Array { elements } => {
            let mut regs = Vec::with_capacity(elements.len());
            for e in elements {
                regs.push(emit_expr(e, em, ctx)?);
            }
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakeArray(dest, regs), span);
            Ok(dest)
        }

        TExprKind::Dict { entries } => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let kr = emit_expr(k, em, ctx)?;
                let vr = emit_expr(v, em, ctx)?;
                pairs.push((kr, vr));
            }
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakeDict(dest, pairs), span);
            Ok(dest)
        }

        TExprKind::StructLiteral { type_name, fields } => {
            let mut compiled_fields = Vec::with_capacity(fields.len());
            for (name, val_expr, is_prompt) in fields {
                let r = emit_expr(val_expr, em, ctx)?;
                compiled_fields.push((name.clone(), r, *is_prompt));
            }
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakeStruct(dest, type_name.clone(), compiled_fields), span);
            Ok(dest)
        }

        TExprKind::FieldAccess { object, field } => {
            let obj = emit_expr(object, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::GetField(dest, obj, field.clone()), span);
            Ok(dest)
        }

        TExprKind::Index { object, index } => {
            let obj = emit_expr(object, em, ctx)?;
            let idx = emit_expr(index, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::GetIndex(dest, obj, idx), span);
            Ok(dest)
        }

        TExprKind::FStr { parts } => {
            let mut compiled = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    TFStrPart::Literal(s) => compiled.push(FStrPart::Literal(s.clone())),
                    TFStrPart::Expr(e)    => compiled.push(FStrPart::Reg(emit_expr(e, em, ctx)?)),
                }
            }
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::BuildFStr(dest, compiled), span);
            Ok(dest)
        }

        TExprKind::PromptDeref { expr: pexpr, output_type } => {
            let src = emit_expr(pexpr, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::PromptDeref(dest, src, output_type.clone()), span);
            Ok(dest)
        }
    }
}

// ── Call emission ─────────────────────────────────────────────────────────────

fn emit_call(
    callee: &TExpr,
    args: &[TExpr],
    full_expr: &TExpr,
    em: &mut Emitter,
    ctx: &mut EmitCtx,
) -> Result<Reg> {
    let span = full_expr.span;

    // Fast path: built-in functions that map to dedicated opcodes.
    if let TExprKind::Identifier(name) = &callee.kind {
        match name.as_str() {
            "print" => {
                let mut arg_regs = Vec::with_capacity(args.len());
                for a in args {
                    arg_regs.push(emit_expr(a, em, ctx)?);
                }
                em.chunk.emit(Instr::CallPrint(arg_regs), span);
                let dest = em.alloc_reg();
                em.chunk.emit(Instr::LoadNil(dest), span);
                return Ok(dest);
            }
            "len" => {
                if args.len() != 1 {
                    return Err(crate::interpreter::error::JadeError::ArityMismatch {
                        expected: 1,
                        got: args.len(),
                        span,
                    });
                }
                let src = emit_expr(&args[0], em, ctx)?;
                let dest = em.alloc_reg();
                em.chunk.emit(Instr::CallLen(dest, src), span);
                return Ok(dest);
            }
            _ => {}
        }
    }

    // General call.
    let callee_reg = emit_expr(callee, em, ctx)?;
    let mut arg_regs = Vec::with_capacity(args.len());
    for a in args {
        arg_regs.push(emit_expr(a, em, ctx)?);
    }
    let dest = em.alloc_reg();
    em.chunk.emit(Instr::Call(dest, callee_reg, arg_regs), span);
    Ok(dest)
}

// ── Binary-op emission ────────────────────────────────────────────────────────

fn emit_binop(
    op: &BinOpKind,
    left: &TExpr,
    right: &TExpr,
    em: &mut Emitter,
    ctx: &mut EmitCtx,
    span: Span,
) -> Result<Reg> {
    use BinOpKind::*;
    use JadeType::*;

    // Short-circuit &&  and  ||
    if matches!(op, And | Or) {
        let dest = em.alloc_reg();
        let l = emit_expr(left, em, ctx)?;
        em.chunk.emit(Instr::Move(dest, l), span);
        let jump_idx = match op {
            And => em.chunk.emit(Instr::JumpIfFalse(l, 0), span),
            Or  => em.chunk.emit(Instr::JumpIfTrue(l, 0), span),
            _   => unreachable!(),
        };
        let r = emit_expr(right, em, ctx)?;
        em.chunk.emit(Instr::Move(dest, r), span);
        em.chunk.patch_jump(jump_idx, em.chunk.len());
        return Ok(dest);
    }

    let l = emit_expr(left, em, ctx)?;
    let r = emit_expr(right, em, ctx)?;
    let dest = em.alloc_reg();

    macro_rules! promote_left {
        ($dest_instr:expr) => {{
            let l2 = em.alloc_reg();
            em.chunk.emit(Instr::IntToFloat(l2, l), span);
            $dest_instr(dest, l2, r)
        }};
    }
    macro_rules! promote_right {
        ($dest_instr:expr) => {{
            let r2 = em.alloc_reg();
            em.chunk.emit(Instr::IntToFloat(r2, r), span);
            $dest_instr(dest, l, r2)
        }};
    }

    let instr = match (op, &left.ty, &right.ty) {
        // ── Arithmetic ───────────────────────────────────────────────────────
        (Add, Int, Int)     => Instr::AddInt(dest, l, r),
        (Add, Float, Float) => Instr::AddFloat(dest, l, r),
        (Add, Int, Float)   => promote_left!(Instr::AddFloat),
        (Add, Float, Int)   => promote_right!(Instr::AddFloat),
        (Add, Str, Str)     => Instr::ConcatStr(dest, l, r),

        (Sub, Int, Int)     => Instr::SubInt(dest, l, r),
        (Sub, Float, Float) => Instr::SubFloat(dest, l, r),
        (Sub, Int, Float)   => promote_left!(Instr::SubFloat),
        (Sub, Float, Int)   => promote_right!(Instr::SubFloat),

        (Mul, Int, Int)     => Instr::MulInt(dest, l, r),
        (Mul, Float, Float) => Instr::MulFloat(dest, l, r),
        (Mul, Int, Float)   => promote_left!(Instr::MulFloat),
        (Mul, Float, Int)   => promote_right!(Instr::MulFloat),

        (Div, Int, Int)     => Instr::DivInt(dest, l, r),
        (Div, Float, Float) => Instr::DivFloat(dest, l, r),
        (Div, Int, Float)   => promote_left!(Instr::DivFloat),
        (Div, Float, Int)   => promote_right!(Instr::DivFloat),

        (Mod, Int, Int)     => Instr::ModInt(dest, l, r),

        // ── Bitwise ──────────────────────────────────────────────────────────
        (BitAnd, Int, Int)  => Instr::BitAnd(dest, l, r),
        (BitOr,  Int, Int)  => Instr::BitOr(dest, l, r),
        (BitXor, Int, Int)  => Instr::BitXor(dest, l, r),
        (Shl,    Int, Int)  => Instr::Shl(dest, l, r),
        (Shr,    Int, Int)  => Instr::Shr(dest, l, r),

        // ── Comparisons — int ─────────────────────────────────────────────────
        (Eq, Int, Int)  => Instr::CmpEqInt(dest, l, r),
        (Ne, Int, Int)  => Instr::CmpNeInt(dest, l, r),
        (Lt, Int, Int)  => Instr::CmpLtInt(dest, l, r),
        (Gt, Int, Int)  => Instr::CmpGtInt(dest, l, r),
        (Le, Int, Int)  => Instr::CmpLeInt(dest, l, r),
        (Ge, Int, Int)  => Instr::CmpGeInt(dest, l, r),

        // ── Comparisons — float ───────────────────────────────────────────────
        (Eq, Float, Float) => Instr::CmpEqFloat(dest, l, r),
        (Ne, Float, Float) => Instr::CmpNeFloat(dest, l, r),
        (Lt, Float, Float) => Instr::CmpLtFloat(dest, l, r),
        (Gt, Float, Float) => Instr::CmpGtFloat(dest, l, r),
        (Le, Float, Float) => Instr::CmpLeFloat(dest, l, r),
        (Ge, Float, Float) => Instr::CmpGeFloat(dest, l, r),

        // ── Comparisons — mixed int/float ─────────────────────────────────────
        (Lt, Int, Float)   => Instr::CmpLtIntFloat(dest, l, r),
        (Gt, Int, Float)   => Instr::CmpGtIntFloat(dest, l, r),
        (Le, Int, Float)   => Instr::CmpLeIntFloat(dest, l, r),
        (Ge, Int, Float)   => Instr::CmpGeIntFloat(dest, l, r),
        (Lt, Float, Int)   => Instr::CmpLtFloatInt(dest, l, r),
        (Gt, Float, Int)   => Instr::CmpGtFloatInt(dest, l, r),
        (Le, Float, Int)   => Instr::CmpLeFloatInt(dest, l, r),
        (Ge, Float, Int)   => Instr::CmpGeFloatInt(dest, l, r),

        // ── Comparisons — bool ────────────────────────────────────────────────
        (Eq, Bool, Bool) => Instr::CmpEqBool(dest, l, r),
        (Ne, Bool, Bool) => Instr::CmpNeBool(dest, l, r),
        (Lt, Bool, Bool) => Instr::CmpLtBool(dest, l, r),
        (Gt, Bool, Bool) => Instr::CmpGtBool(dest, l, r),
        (Le, Bool, Bool) => Instr::CmpLeBool(dest, l, r),
        (Ge, Bool, Bool) => Instr::CmpGeBool(dest, l, r),

        // ── Comparisons — str ─────────────────────────────────────────────────
        (Eq, Str, Str) => Instr::CmpEqStr(dest, l, r),
        (Ne, Str, Str) => Instr::CmpNeStr(dest, l, r),
        (Lt, Str, Str) => Instr::CmpLtStr(dest, l, r),
        (Gt, Str, Str) => Instr::CmpGtStr(dest, l, r),
        (Le, Str, Str) => Instr::CmpLeStr(dest, l, r),
        (Ge, Str, Str) => Instr::CmpGeStr(dest, l, r),

        // ── Dynamic fallback (Unknown-typed operands) ─────────────────────────
        (Eq, _, _) => Instr::CmpEq(dest, l, r),
        (Ne, _, _) => Instr::CmpNe(dest, l, r),
        (Lt, _, _) => Instr::CmpLt(dest, l, r),
        (Gt, _, _) => Instr::CmpGt(dest, l, r),
        (Le, _, _) => Instr::CmpLe(dest, l, r),
        (Ge, _, _) => Instr::CmpGe(dest, l, r),
        _          => Instr::BinOp(dest, op.clone(), l, r),
    };
    em.chunk.emit(instr, span);
    Ok(dest)
}

// ── Unary-op emission ─────────────────────────────────────────────────────────

fn emit_unaryop(
    op: &UnaryOpKind,
    operand: &TExpr,
    em: &mut Emitter,
    ctx: &mut EmitCtx,
    span: Span,
) -> Result<Reg> {
    let src = emit_expr(operand, em, ctx)?;
    let dest = em.alloc_reg();
    let instr = match (op, &operand.ty) {
        (UnaryOpKind::Neg, JadeType::Int)    => Instr::NegInt(dest, src),
        (UnaryOpKind::Neg, JadeType::Float)  => Instr::NegFloat(dest, src),
        (UnaryOpKind::BitNot, JadeType::Int) => Instr::BitNot(dest, src),
        (UnaryOpKind::Not, JadeType::Bool)   => Instr::Not(dest, src),
        _                                    => Instr::UnaryOp(dest, op.clone(), src),
    };
    em.chunk.emit(instr, span);
    Ok(dest)
}
