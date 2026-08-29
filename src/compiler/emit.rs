use std::{collections::HashMap, sync::Arc};

use crate::{
    bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
    compiler::tir::{
        JadeType, TCatchArm, TDecorators, TExpr, TExprKind, TFStrPart, TProgram, TStmt,
    },
    frontend::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{JadeError, Result, Span},
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
    /// Every struct a type inherits, nearest first, flattened transitively.
    ///
    /// Fields and methods are already folded into the child by the time this
    /// exists, so neither engine walks it to build a value or to find a method.
    /// It is here for one thing: a typed `catch` arm matches a struct that
    /// *inherits* the named type, and that is the only question left at run time.
    pub struct_ancestors: HashMap<String, Vec<String>>,
    /// Compiled extend-block methods: `type_name → method_name → CompiledFn`.
    /// A parent's methods are folded into each child, the child's winning.
    pub extend_methods: HashMap<String, HashMap<String, Arc<CompiledFn>>>,
}

// ── Internal state ────────────────────────────────────────────────────────────

/// Every struct each type inherits, nearest first, transitively.
///
/// Nearest-first order is what makes an override readable: a `Puppy` that
/// inherits `Dog` which inherits `Animal` lists `Dog` before `Animal`, so the
/// first entry that supplies something is the one that wins. Cycles cannot reach
/// here — `resolve_inheritance` refuses them — but the visited set keeps this
/// terminating anyway rather than trusting a caller two passes away.
fn flatten_ancestry(parents: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    fn walk(
        name: &str,
        parents: &HashMap<String, Vec<String>>,
        out: &mut Vec<String>,
        seen: &mut Vec<String>,
    ) {
        if seen.iter().any(|s| s == name) {
            return;
        }
        seen.push(name.to_string());
        for p in parents.get(name).into_iter().flatten() {
            if !out.iter().any(|o| o == p) {
                out.push(p.clone());
            }
            walk(p, parents, out, seen);
        }
    }
    let mut all = HashMap::new();
    for name in parents.keys() {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        walk(name, parents, &mut out, &mut seen);
        if !out.is_empty() {
            all.insert(name.clone(), out);
        }
    }
    all
}

/// Shared context threaded through the whole compilation.
struct EmitCtx {
    struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Written parents per struct, before flattening. Consumed by
    /// `resolve_inheritance` and not part of `CompiledProgram`.
    struct_parents: HashMap<String, Vec<String>>,
    struct_ancestors: HashMap<String, Vec<String>>,
    extend_methods: HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    /// Counter for generating unique closure names (`__closure_0__`, etc.).
    next_closure_id: usize,
}

impl EmitCtx {
    fn next_closure_name(&mut self) -> String {
        let id = self.next_closure_id;
        self.next_closure_id += 1;
        format!("__closure_{}__", id)
    }
}

/// Per-chunk compilation state.
struct Emitter {
    chunk: Chunk,
    /// Next register to allocate.
    next_reg: u32,
    /// `None` = top-level code: `let` → globals.
    /// `Some` = function body: `let` → frame slots.
    locals: Option<HashMap<String, u32>>,
    /// Source positions `(line, col)` of array literals the escape analysis
    /// proved non-escaping; the emitter lowers these to `MakeArrayArena`. Empty
    /// for the top level (its `let`s are globals, which escape by nature).
    arena_eligible: std::collections::HashSet<(usize, usize)>,
    /// When this function uses the arena, the register holding its function-scope
    /// mark token: `ArenaReset(tok)` is emitted before every return so arena
    /// memory is reclaimed on exit (not left to balloon across calls).
    arena_fn_tok: Option<Reg>,
    /// One entry per loop currently being emitted, innermost last. Holds the
    /// jump sites that `break` and `continue` left behind for the loop to patch
    /// once it knows where its exit and its next iteration begin.
    loops: Vec<LoopSites>,
    /// How many exception handlers are installed at this point in the chunk. A
    /// jump out of a `try` body has to pop the ones it skips past — see
    /// [`Emitter::emit_loop_jump`].
    handler_depth: usize,
}

/// Where an enclosing loop has to patch the jumps its body left behind.
#[derive(Default)]
struct LoopSites {
    /// Instruction indices of `break` jumps, patched to just past the loop.
    breaks: Vec<usize>,
    /// Instruction indices of `continue` jumps, patched to the bottom of the
    /// body — before the increment and the arena reset, so a `continue` runs
    /// both rather than skipping them.
    continues: Vec<usize>,
    /// The handler depth outside the loop, so a `break` from inside a `try`
    /// knows how many frames it is jumping out of.
    handler_depth: usize,
}

impl Emitter {
    fn new_top() -> Self {
        Emitter {
            chunk: Chunk::new("<top>"),
            next_reg: 0,
            locals: None,
            arena_eligible: std::collections::HashSet::new(),
            arena_fn_tok: None,
            loops: Vec::new(),
            handler_depth: 0,
        }
    }

    fn new_fn(name: &str) -> Self {
        Emitter {
            chunk: Chunk::new(name),
            next_reg: 0,
            locals: Some(HashMap::new()),
            arena_eligible: std::collections::HashSet::new(),
            arena_fn_tok: None,
            loops: Vec::new(),
            handler_depth: 0,
        }
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

    /// Emit a `break` or `continue` as a jump for the enclosing loop to patch.
    ///
    /// Both first pop any exception handler the jump escapes. A `try` installs
    /// a handler and removes it with `PopHandler` on the way out; leaving by a
    /// jump instead skips that, and the frame stays installed pointing at code
    /// the loop has already left. The next exception anywhere in the function
    /// would then land in the wrong arm. Popping here is what makes
    ///
    /// ```text
    /// while true { try { step() } catch e { break } }
    /// ```
    ///
    /// mean what it reads as — and that shape is the natural one for a C
    /// library that signals end-of-input by returning an error.
    ///
    /// A `break` from inside a `catch` arm pops nothing, correctly: dispatching
    /// the exception already removed that frame.
    ///
    /// A `break` does skip the loop's `ArenaReset`, so the iteration it left
    /// from keeps its arena region until the function returns. That is one
    /// iteration's worth on the way out of a loop, reclaimed at the function's
    /// own reset; landing it somewhere that runs the reset would mean emitting
    /// a second one only reachable by `break`, which is more emitter to be
    /// wrong in than the memory is worth. `continue` is the case that would
    /// repeat, and it does run the reset.
    fn emit_loop_jump(&mut self, is_break: bool, span: Span) {
        let Some(depth_outside) = self.loops.last().map(|l| l.handler_depth) else { return };
        for _ in depth_outside..self.handler_depth {
            self.chunk.emit(Instr::PopHandler, span);
        }
        let site = self.chunk.emit(Instr::Jump(0), span);
        let Some(loop_sites) = self.loops.last_mut() else { return };
        if is_break {
            loop_sites.breaks.push(site);
        } else {
            loop_sites.continues.push(site);
        }
    }

    /// Open a per-iteration arena region inside a loop body — only when this
    /// function uses the arena (else the mark/reset would be pure overhead).
    /// Emits `ArenaMark` and returns the token register; the caller emits the
    /// matching `ArenaReset` at the bottom of the loop body.
    fn arena_loop_open(&mut self, span: Span) -> Option<Reg> {
        self.arena_fn_tok?;
        let tok = self.alloc_reg();
        self.chunk.emit(Instr::ArenaMark(tok), span);
        Some(tok)
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
        if let Some(locals) = &self.locals
            && let Some(&slot) = locals.get(name)
        {
            self.chunk.emit(Instr::SetLocal(slot, src), span);
            return;
        }
        self.chunk.emit(Instr::SetGlobal(name.to_string(), src), span);
    }
}

// ── Module-level constants ────────────────────────────────────────────────────

/// A zero-span used where no meaningful source location exists (e.g. synthetic
/// `Halt` and fallback `Return(None)` instructions injected by the emitter).
const NO_SPAN: Span = Span { line: 0, col: 0 };

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Evaluate a TExpr that must be a literal — used for struct decorator args.
fn eval_literal_expr(
    expr: &TExpr,
    span: crate::frontend::error::Span,
) -> Result<crate::vm::VmValue> {
    use crate::compiler::tir::TExprKind;
    use crate::vm::VmValue;
    match &expr.kind {
        TExprKind::Integer(n)  => Ok(VmValue::Int(*n)),
        TExprKind::Float(f)    => Ok(VmValue::Float(*f)),
        TExprKind::Bool(b)     => Ok(VmValue::Bool(*b)),
        TExprKind::Str(s)      => Ok(VmValue::Str(s.clone().into())),
        TExprKind::Identifier(s) if s == "None" || s == "nil" || s == "null" => Ok(VmValue::Nil),
        _ => Err(crate::frontend::error::JadeError::Exception {
            message: "struct decorator arguments must be literals (None, nil, null, numbers, booleans, strings)".to_string(),
            span,
        }),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Compile a `TProgram` into bytecode ready for the VM.
pub fn emit(program: TProgram) -> Result<CompiledProgram> {
    // First pass: collect static metadata (struct definitions and their parents).
    let mut ctx = EmitCtx {
        struct_defs: HashMap::new(),
        struct_parents: HashMap::new(),
        struct_ancestors: HashMap::new(),
        extend_methods: HashMap::new(),
        next_closure_id: 0,
    };
    for stmt in &program.stmts {
        match stmt {
            TStmt::StructDef { name, fields, parents, .. } => {
                ctx.struct_defs.insert(name.clone(), fields.clone());
                ctx.struct_parents.insert(name.clone(), parents.clone());
            }
            _ => {}
        }
    }

    // Ancestry, worked out here rather than in the checker because this is where
    // the final name set is known: the AOT backend inlines every imported module
    // into one stream and mangles the names first, so a parent written
    // `shapes.Animal` has already become whatever it is going to be.
    ctx.struct_ancestors = flatten_ancestry(&ctx.struct_parents);

    // Second pass: emit all statements into the top-level chunk.
    let mut em = Emitter::new_top();
    for stmt in program.stmts {
        emit_stmt(stmt, &mut em, &mut ctx)?;
    }
    em.chunk.emit(Instr::Halt, NO_SPAN);

    let n_slots = em.next_reg;

    // Tasks share one heap with no lock on the payload, so a spawned function
    // that mutates state the spawner can still reach is a data race. Reject it
    // here rather than locking every collection or silently deep-copying task
    // arguments; see `taskcheck` for why this is the right trade.
    //
    // This runs post-emit because the mutation opcodes only exist in bytecode:
    // the AST's assignment expression does not distinguish rebinding a local
    // from writing through a reference.
    if let Err(v) = crate::compiler::taskcheck::check(&em.chunk, &ctx.extend_methods) {
        return Err(crate::frontend::error::JadeError::SharedMutation {
            task: v.task,
            what: v.what,
            span: v.span,
        });
    }

    // Fold each parent's compiled methods into its children, nearest first, a
    // child's own entry standing. Doing it here rather than in the checker is
    // what keeps both engines out of it: `extend_methods` is already the flat
    // `type_name -> method_name -> CompiledFn` table each of them looks a method
    // up in, and after this it simply holds more entries.
    let inherited: Vec<(String, HashMap<String, Arc<CompiledFn>>)> = ctx
        .struct_ancestors
        .iter()
        .map(|(name, chain)| {
            let mut merged = ctx.extend_methods.get(name).cloned().unwrap_or_default();
            for ancestor in chain {
                for (method, f) in ctx.extend_methods.get(ancestor).into_iter().flatten() {
                    merged.entry(method.clone()).or_insert_with(|| Arc::clone(f));
                }
            }
            (name.clone(), merged)
        })
        .filter(|(_, m)| !m.is_empty())
        .collect();
    for (name, merged) in inherited {
        ctx.extend_methods.insert(name, merged);
    }

    Ok(CompiledProgram {
        top_n_slots: n_slots,
        top: em.chunk,
        struct_defs: ctx.struct_defs,
        struct_ancestors: ctx.struct_ancestors,
        extend_methods: ctx.extend_methods,
    })
}

// ── Statement emission ────────────────────────────────────────────────────────

fn emit_stmt(stmt: TStmt, em: &mut Emitter, ctx: &mut EmitCtx) -> Result<()> {
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

        TStmt::FnDef { name, params, body, span, decorators, .. } => {
            let compiled = emit_fn(&name, params, body, span, ctx)?;
            let rc = Arc::new(compiled);
            let idx = em.chunk.intern_fn(Arc::clone(&rc));
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadFn(dest, idx), span);
            if em.in_fn() {
                let slot = em.define_local(&name);
                em.chunk.emit(Instr::SetLocal(slot, dest), span);
                reject_local_decorators(&decorators, span)?;
            } else {
                em.chunk.emit(Instr::SetGlobal(name.clone(), dest), span);
                emit_fn_decorators(&name, &decorators, em, ctx, span)?;
            }
        }

        TStmt::Yield { value, span } => {
            let r = emit_expr(&value, em, ctx)?;
            em.chunk.emit(Instr::Yield(r), span);
        }

        TStmt::Return { value, span } => {
            // Evaluate the return value first, then reset the function's arena
            // region (freeing any arena memory) before returning. The value must
            // never be arena-allocated (the escape analysis guarantees a returned
            // array is not arena), so resetting before the return cannot free it.
            let ret = match value {
                Some(expr) => Some(emit_expr(&expr, em, ctx)?),
                None => None,
            };
            if let Some(tok) = em.arena_fn_tok {
                em.chunk.emit(Instr::ArenaReset(tok), span);
            }
            em.chunk.emit(Instr::Return(ret), span);
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

            // Per-iteration arena region: any arena array built in the body is
            // reclaimed at the bottom of each iteration, so a hot loop reuses the
            // same arena memory instead of accumulating it. Only in functions that
            // use the arena at all (else the mark/reset would be pure overhead).
            let loop_tok = em.arena_loop_open(span);

            em.loops.push(LoopSites { handler_depth: em.handler_depth, ..Default::default() });
            for s in body {
                emit_stmt(s, em, ctx)?;
            }
            let sites = em.loops.pop().unwrap_or_default();

            // `continue` lands here rather than at `loop_start`, so it still
            // runs the arena reset below instead of leaking a region per
            // iteration.
            for site in sites.continues {
                em.chunk.patch_jump(site, em.chunk.len());
            }

            if let Some(t) = loop_tok {
                em.chunk.emit(Instr::ArenaReset(t), span);
            }

            // Back-jump: offset = loop_start − (current + 1)
            let back = loop_start as i32 - (em.chunk.len() as i32 + 1);
            em.chunk.emit(Instr::Jump(back), span);
            em.chunk.patch_jump(jump_exit, em.chunk.len());
            for site in sites.breaks {
                em.chunk.patch_jump(site, em.chunk.len());
            }
        }

        TStmt::For { var, iterable, body, span } => {
            // Evaluate the iterable into a register.
            let iter_reg = emit_expr(&iterable, em, ctx)?;

            // idx = 0
            let idx_reg = em.alloc_reg();
            em.chunk.emit(Instr::LoadInt(idx_reg, 0), span);

            // len = len(iter) — call the global `len` BuiltinFn
            let len_fn_reg = em.alloc_reg();
            em.chunk.emit(Instr::GetGlobal(len_fn_reg, "len".to_string()), span);
            let len_reg = em.alloc_reg();
            em.chunk.emit(Instr::Call(len_reg, len_fn_reg, vec![iter_reg]), span);

            // Allocate the loop variable: a local slot inside fn, a scratch reg at top level.
            let x_reg = if em.in_fn() { em.define_local(&var) } else { em.alloc_reg() };

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
            let loop_tok = em.arena_loop_open(span);
            em.loops.push(LoopSites { handler_depth: em.handler_depth, ..Default::default() });
            for s in body {
                emit_stmt(s, em, ctx)?;
            }
            let sites = em.loops.pop().unwrap_or_default();

            // `continue` lands before the increment, never after it — landing
            // at the top of the loop instead would never advance the index, and
            // the loop would hang.
            for site in sites.continues {
                em.chunk.patch_jump(site, em.chunk.len());
            }

            // ── Increment: idx = idx + 1 ───────────────────────────────────────
            let one_reg = em.alloc_reg();
            em.chunk.emit(Instr::LoadInt(one_reg, 1), span);
            let next_idx = em.alloc_reg();
            em.chunk.emit(Instr::AddInt(next_idx, idx_reg, one_reg), span);
            em.chunk.emit(Instr::Move(idx_reg, next_idx), span);

            if let Some(t) = loop_tok {
                em.chunk.emit(Instr::ArenaReset(t), span);
            }

            // Back-jump and patch exit.
            let back = loop_start as i32 - (em.chunk.len() as i32 + 1);
            em.chunk.emit(Instr::Jump(back), span);
            em.chunk.patch_jump(jump_exit, em.chunk.len());
            for site in sites.breaks {
                em.chunk.patch_jump(site, em.chunk.len());
            }
        }

        TStmt::Break { span } => em.emit_loop_jump(true, span),
        TStmt::Continue { span } => em.emit_loop_jump(false, span),

        // Metadata only — already captured in the first pass.
        TStmt::StructDef { .. } => {}

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
                method_map.insert(name, Arc::new(compiled));
            }
        }

        TStmt::FieldAssign { object, field, value, span } => {
            let val = emit_expr(&value, em, ctx)?;
            let obj = em.emit_load_var(&object, span);
            em.chunk.emit(Instr::SetField(obj, field, val), span);
        }

        TStmt::IndexAssign { name, index, value, span } => {
            // A local *is* a register slot, so hand `SetIndex` the binding
            // itself rather than a copy of it. That is what lets the write
            // happen in place: a dict is copy-on-write, and loading it into a
            // second register would leave two holders, so every write would copy
            // the whole dict and building one would be quadratic. There is
            // nothing to write back afterwards either — the instruction already
            // wrote to the slot the variable lives in.
            if let Some(slot) = em.lookup_local(&name) {
                let idx = emit_expr(&index, em, ctx)?;
                let val = emit_expr(&value, em, ctx)?;
                em.chunk.emit(Instr::SetIndex(slot, idx, val), span);
                return Ok(());
            }
            // A global lives in a map (VM) or an LLVM cell (AOT), not a
            // register, so the instruction takes the name and owns the binding
            // for the write — same reason as the local case above.
            let idx = emit_expr(&index, em, ctx)?;
            let val = emit_expr(&value, em, ctx)?;
            em.chunk.emit(Instr::SetIndexGlobal(name, idx, val), span);
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

        TStmt::Use { path, as_name, span, .. } => {
            let namespace = as_name.unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&path)
                    .to_string()
            });
            em.chunk.emit(Instr::ImportFile(path, namespace), span);
        }

        TStmt::FromUse { path, names, span, .. } => {
            em.chunk.emit(Instr::ImportFrom(path, names), span);
        }

        TStmt::Raise { value, span } => {
            let src = emit_expr(&value, em, ctx)?;
            em.chunk.emit(Instr::Raise(src), span);
        }

        TStmt::TryCatch { body, arms, span } => {
            // Allocate a register to hold the caught exception value.
            let caught_reg = em.alloc_reg();

            // SetupHandler: on exception, store caught value in caught_reg and jump to handler.
            // Offset is patched below once we know where the handler arms begin.
            let setup_idx = em.chunk.emit(Instr::SetupHandler(caught_reg, 0), span);

            // Emit try body. The depth is raised only for the body: an arm runs
            // with the frame already popped by the dispatch that reached it.
            em.handler_depth += 1;
            for s in body {
                emit_stmt(s, em, ctx)?;
            }
            em.handler_depth -= 1;

            // Normal exit from try: pop the handler frame.
            em.chunk.emit(Instr::PopHandler, span);

            // Jump past all handler arms (patched below).
            let jump_end_idx = em.chunk.emit(Instr::Jump(0), span);

            // Patch SetupHandler to point here (start of handler arms).
            em.chunk.patch_jump(setup_idx, em.chunk.len());

            // Collect jumps to end that need patching after all arms.
            let mut end_jumps: Vec<usize> = Vec::new();

            let _n_arms = arms.len();
            for arm in arms.into_iter() {
                let TCatchArm { catch_type, binding, body: arm_body } = arm;

                // For typed arms, check the caught value's type name.
                let skip_idx = if let Some(type_name) = catch_type {
                    let type_reg = em.alloc_reg();
                    em.chunk.emit(Instr::GetTypeName(type_reg, caught_reg), span);
                    let expected_reg = em.alloc_reg();
                    em.chunk.emit(Instr::LoadStr(expected_reg, type_name), span);
                    let cmp_reg = em.alloc_reg();
                    em.chunk.emit(Instr::CmpEqStr(cmp_reg, type_reg, expected_reg), span);
                    // If type doesn't match, jump to next arm.
                    let idx = em.chunk.emit(Instr::JumpIfFalse(cmp_reg, 0), span);
                    Some(idx)
                } else {
                    None
                };

                // Bind the caught value to the arm's variable.
                if em.in_fn() {
                    let slot = em.define_local(&binding);
                    em.chunk.emit(Instr::SetLocal(slot, caught_reg), span);
                } else {
                    em.chunk.emit(Instr::SetGlobal(binding.clone(), caught_reg), span);
                }

                // Emit arm body.
                for s in arm_body {
                    emit_stmt(s, em, ctx)?;
                }

                // Always jump to end after the arm body executes, so execution
                // never falls through into the re-raise or the next arm.
                let j = em.chunk.emit(Instr::Jump(0), span);
                end_jumps.push(j);

                // Patch the type-mismatch skip to point past this arm (to the next arm).
                if let Some(skip) = skip_idx {
                    em.chunk.patch_jump(skip, em.chunk.len());
                }
            }

            // Re-raise fallthrough: if all typed arms failed to match, re-raise.
            // Arms with a catch-all will always jump past this via end_jumps.
            em.chunk.emit(Instr::Raise(caught_reg), span);

            // Patch all end jumps to point here (past the re-raise).
            let end_target = em.chunk.len();
            em.chunk.patch_jump(jump_end_idx, end_target);
            for j in end_jumps {
                em.chunk.patch_jump(j, end_target);
            }
        }

        TStmt::AsyncFnDef { name, params, body, span, decorators, .. } => {
            // Compile async fn body as a regular CompiledFn.
            // Call sites emit Instr::Spawn (instead of Instr::Call) based on
            // the callee's JadeType::AsyncFn — handled in emit_call below.
            let compiled = emit_fn(&name, params, body, span, ctx)?;
            let rc = Arc::new(compiled);
            let idx = em.chunk.intern_fn(Arc::clone(&rc));
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::LoadFn(dest, idx), span);
            if em.in_fn() {
                let slot = em.define_local(&name);
                em.chunk.emit(Instr::SetLocal(slot, dest), span);
                reject_local_decorators(&decorators, span)?;
            } else {
                em.chunk.emit(Instr::SetGlobal(name.clone(), dest), span);
                // Shares the global path with `fn`. It used to have its own copy
                // that resolved the decorator name with a bare GetGlobal, so
                // `@tools::register` worked on a `fn` and looked up a global
                // literally named "tools.register" on an `async fn`.
                emit_fn_decorators(&name, &decorators, em, ctx, span)?;
            }
        }

        TStmt::Expr(expr) => {
            emit_expr(&expr, em, ctx)?;
        }
    }
    Ok(())
}

// ── Decorators on a function ─────────────────────────────────────────────────
//
// A decorator on a `let` or a `prompt` is gone by the time TIR exists — the
// parser rewrites `@f let x = v` into `let x = f(v)`. A function cannot be done
// that way, because the value being wrapped is one this emitter is in the middle
// of building, so it is applied here instead: `foo = dec(foo, …)` against the
// global, in source order, which is what makes the decorator written first the
// innermost one.

/// Emit `name = dec(name, args…)` for each decorator, in source order.
fn emit_fn_decorators(
    name: &str,
    decorators: &TDecorators,
    em: &mut Emitter,
    ctx: &mut EmitCtx,
    span: Span,
) -> Result<()> {
    for (dec_name, dec_args) in decorators {
        // A namespaced `@tools::register` reached the parser's decorator list as
        // "tools.register": load the module, then take the field off it, the
        // same way `tools.register(x)` written by hand resolves.
        let parts: Vec<&str> = dec_name.splitn(2, '.').collect();
        let base_reg = em.alloc_reg();
        em.chunk.emit(Instr::GetGlobal(base_reg, parts[0].to_string()), span);
        let dec_reg = if parts.len() == 2 {
            let field_reg = em.alloc_reg();
            em.chunk.emit(Instr::GetField(field_reg, base_reg, parts[1].to_string()), span);
            field_reg
        } else {
            base_reg
        };
        let fn_reg = em.alloc_reg();
        em.chunk.emit(Instr::GetGlobal(fn_reg, name.to_string()), span);
        // The decorated function is the first argument; the decorator's own
        // arguments follow.
        let mut call_args = vec![fn_reg];
        for (_, arg_expr) in dec_args {
            let arg_reg = emit_expr(arg_expr, em, ctx)?;
            call_args.push(arg_reg);
        }
        let result_reg = em.alloc_reg();
        em.chunk.emit(Instr::Call(result_reg, dec_reg, call_args), span);
        em.chunk.emit(Instr::SetGlobal(name.to_string(), result_reg), span);
    }
    Ok(())
}

/// Refuse a decorator on a function bound to a local instead of a global.
///
/// The code above works against a global, so a function defined inside another
/// function has nothing for it to rewrite. This used to be an `if` with no
/// `else`: the decorator was dropped and the program ran as though it had never
/// been written. The parser now refuses a nested function of either kind, so
/// nothing in a source file reaches this — it is here so that a future path
/// which does reach it fails loudly rather than going quiet again.
fn reject_local_decorators(decorators: &TDecorators, span: Span) -> Result<()> {
    if decorators.is_empty() {
        return Ok(());
    }
    Err(JadeError::NestedFunction { span })
}

// ── Function compilation ──────────────────────────────────────────────────────

fn emit_fn(
    name: &str,
    params: Vec<(String, Option<TExpr>)>,
    mut body: Vec<TStmt>,
    span: Span,
    ctx: &mut EmitCtx,
) -> Result<CompiledFn> {
    let mut fn_em = Emitter::new_fn(name);
    // Decide which array literals in this function may be arena-allocated (AOT
    // only; the VM ignores the distinction). Runs on the typed body before it is
    // consumed by emission.
    let arena_plan = crate::compiler::escape::analyze(&body);
    fn_em.arena_eligible = arena_plan.eligible.clone();
    // Allocate slots for parameters first (slots 0..params.len()).
    let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
    for name in &param_names {
        fn_em.define_local(name);
    }
    // Open the function-scope arena region: a mark at entry, reset before every
    // return (below), so arena memory is reclaimed on exit. The mark token is an
    // even (int-like) word, so the AOT's scope-exit decref no-ops on its register.
    if !arena_plan.is_empty() {
        let tok = fn_em.alloc_reg();
        fn_em.chunk.emit(Instr::ArenaMark(tok), span);
        fn_em.arena_fn_tok = Some(tok);
    }
    // Compile literal defaults — non-literal defaults are unsupported for now.
    let defaults: Vec<Option<crate::vm::VmValue>> = params.iter()
        .map(|(_, default)| {
            match default {
                None => Ok(None),
                Some(expr) => match &expr.kind {
                    crate::compiler::tir::TExprKind::Integer(n) => Ok(Some(crate::vm::VmValue::Int(*n))),
                    crate::compiler::tir::TExprKind::Float(f)   => Ok(Some(crate::vm::VmValue::Float(*f))),
                    crate::compiler::tir::TExprKind::Bool(b)    => Ok(Some(crate::vm::VmValue::Bool(*b))),
                    crate::compiler::tir::TExprKind::Str(s)     => Ok(Some(crate::vm::VmValue::Str(s.clone().into()))),
                    crate::compiler::tir::TExprKind::Identifier(s) if s == "None" || s == "nil" || s == "null" => {
                        Ok(Some(crate::vm::VmValue::Nil))
                    }
                    _ => Err(crate::frontend::error::JadeError::Exception {
                        message: "default parameter values must be literals (None, nil, null, numbers, booleans, strings)".to_string(),
                        span,
                    }),
                }
            }
        })
        .collect::<Result<_>>()?;

    // A body containing a `yield` anywhere produces a stream rather than a
    // value. Detected before the body is consumed below, and recursively:
    // `yield` inside an `if` or a loop still makes the function a producer.
    let is_generator = body_yields(&body);

    // If the body already ends with an explicit terminator (return or raise),
    // we must not append a second Return(None) after it — that would be dead
    // code.  Check *before* the pop below so we see the original last stmt.
    let already_terminated =
        matches!(body.last(), Some(TStmt::Return { .. } | TStmt::Raise { .. }));

    // If the last statement is a bare expression, treat it as an implicit return value.
    let implicit_ret = if matches!(body.last(), Some(TStmt::Expr(_))) {
        if let Some(TStmt::Expr(expr)) = body.pop() { Some(expr) } else { None }
    } else {
        None
    };

    for stmt in body {
        emit_stmt(stmt, &mut fn_em, ctx)?;
    }

    if let Some(expr) = implicit_ret {
        let src = emit_expr(&expr, &mut fn_em, ctx)?;
        if let Some(tok) = fn_em.arena_fn_tok {
            fn_em.chunk.emit(Instr::ArenaReset(tok), span);
        }
        fn_em.chunk.emit(Instr::Return(Some(src)), span);
    } else if !already_terminated {
        // Implicit nil return if execution falls off the end of the function.
        if let Some(tok) = fn_em.arena_fn_tok {
            fn_em.chunk.emit(Instr::ArenaReset(tok), NO_SPAN);
        }
        fn_em.chunk.emit(Instr::Return(None), NO_SPAN);
    }

    let n_slots = fn_em.next_reg;
    Ok(CompiledFn {
        params: param_names,
        defaults,
        chunk: fn_em.chunk,
        n_slots,
        source_file: String::new(),
        module_scope: None,
        is_generator,
    })
}

/// Whether a body contains a `yield` at any depth inside this function.
///
/// Does not descend into a nested closure: a closure that yields is its own
/// producer, and its `yield`s belong to its stream, not the enclosing one.
fn body_yields(body: &[TStmt]) -> bool {
    body.iter().any(|s| match s {
        TStmt::Yield { .. } => true,
        TStmt::If { then_body, else_body, .. } => {
            body_yields(then_body) || else_body.as_deref().is_some_and(body_yields)
        }
        TStmt::While { body, .. } | TStmt::For { body, .. } => body_yields(body),
        TStmt::TryCatch { body, arms, .. } => {
            body_yields(body) || arms.iter().any(|a| body_yields(&a.body))
        }
        _ => false,
    })
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
            // nil/None are built-in literals, not globals — emit directly.
            if name == "nil" || name == "None" || name == "null" {
                let dest = em.alloc_reg();
                em.chunk.emit(Instr::LoadNil(dest), span);
                return Ok(dest);
            }
            // Builtin functions are handled at call sites; identifiers just load.
            Ok(em.emit_load_var(name, span))
        }

        TExprKind::Call { callee, args, kwargs } => emit_call(callee, args, kwargs, expr, em, ctx),

        TExprKind::BinOp { op, left, right } => emit_binop(op, left, right, em, ctx, span),

        TExprKind::UnaryOp { op, operand } => emit_unaryop(op, operand, em, ctx, span),

        TExprKind::Array { elements } => {
            let mut regs = Vec::with_capacity(elements.len());
            for e in elements {
                regs.push(emit_expr(e, em, ctx)?);
            }
            let dest = em.alloc_reg();
            // A literal the escape analysis cleared is lowered to MakeArrayArena so
            // the AOT backend can arena-allocate it; the VM treats it as MakeArray.
            let instr = if em.arena_eligible.contains(&(span.line, span.col)) {
                Instr::MakeArrayArena(dest, regs)
            } else {
                Instr::MakeArray(dest, regs)
            };
            em.chunk.emit(instr, span);
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
                    TFStrPart::Expr(e) => compiled.push(FStrPart::Reg(emit_expr(e, em, ctx)?)),
                }
            }
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::BuildFStr(dest, compiled), span);
            Ok(dest)
        }

        TExprKind::PromptLiteral { body } => {
            let text = emit_expr(body, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakePrompt(dest, text), span);
            Ok(dest)
        }

        TExprKind::PromptDeref { expr: pexpr, output_type, grammar_expr } => {
            let src = emit_expr(pexpr, em, ctx)?;
            let grammar_reg = grammar_expr.as_ref().map(|g| emit_expr(g, em, ctx)).transpose()?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::PromptDeref(dest, src, output_type.clone(), grammar_reg), span);
            Ok(dest)
        }

        TExprKind::Closure { params, body, .. } => {
            let name = ctx.next_closure_name();
            let owned: Vec<(String, Option<TExpr>)> =
                params.iter().map(|p| (p.clone(), None)).collect();
            let compiled = emit_fn(&name, owned, body.clone(), span, ctx)?;
            let rc = Arc::new(compiled);
            let idx = em.chunk.intern_fn(Arc::clone(&rc));
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::MakeClosure(dest, idx), span);
            Ok(dest)
        }

        TExprKind::Await { expr } => {
            let src = emit_expr(expr, em, ctx)?;
            let dest = em.alloc_reg();
            em.chunk.emit(Instr::Await(dest, src), span);
            Ok(dest)
        }
    }
}

// ── Call emission ─────────────────────────────────────────────────────────────

fn emit_call(
    callee: &TExpr,
    args: &[TExpr],
    kwargs: &[(String, TExpr)],
    full_expr: &TExpr,
    em: &mut Emitter,
    ctx: &mut EmitCtx,
) -> Result<Reg> {
    let span = full_expr.span;

    // `join` is async-specific and stays as a dedicated opcode.
    if let TExprKind::Identifier(name) = &callee.kind
        && name == "join"
    {
        let mut arg_regs = Vec::with_capacity(args.len());
        for a in args {
            arg_regs.push(emit_expr(a, em, ctx)?);
        }
        let dest = em.alloc_reg();
        em.chunk.emit(Instr::Join(dest, arg_regs), span);
        return Ok(dest);
    }

    // Async fn call → emit Spawn instead of Call.
    if matches!(callee.ty, JadeType::AsyncFn { .. }) {
        let callee_reg = emit_expr(callee, em, ctx)?;
        let mut arg_regs = Vec::with_capacity(args.len());
        for a in args {
            arg_regs.push(emit_expr(a, em, ctx)?);
        }
        let dest = em.alloc_reg();
        em.chunk.emit(Instr::Spawn(dest, callee_reg, arg_regs), span);
        return Ok(dest);
    }

    let callee_reg = emit_expr(callee, em, ctx)?;

    // If there are keyword arguments, emit CallNamed with mixed positional/named pairs.
    if !kwargs.is_empty() {
        let mut mixed: Vec<(Option<String>, Reg)> = Vec::with_capacity(args.len() + kwargs.len());
        for a in args {
            mixed.push((None, emit_expr(a, em, ctx)?));
        }
        for (name, val) in kwargs {
            mixed.push((Some(name.clone()), emit_expr(val, em, ctx)?));
        }
        let dest = em.alloc_reg();
        em.chunk.emit(Instr::CallNamed(dest, callee_reg, mixed), span);
        return Ok(dest);
    }

    // General positional-only call.
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
            Or => em.chunk.emit(Instr::JumpIfTrue(l, 0), span),
            _ => unreachable!(),
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
        (Add, Int, Int) => Instr::AddInt(dest, l, r),
        (Add, Float, Float) => Instr::AddFloat(dest, l, r),
        (Add, Int, Float) => promote_left!(Instr::AddFloat),
        (Add, Float, Int) => promote_right!(Instr::AddFloat),
        (Add, Str, Str) => Instr::ConcatStr(dest, l, r),

        (Sub, Int, Int) => Instr::SubInt(dest, l, r),
        (Sub, Float, Float) => Instr::SubFloat(dest, l, r),
        (Sub, Int, Float) => promote_left!(Instr::SubFloat),
        (Sub, Float, Int) => promote_right!(Instr::SubFloat),

        (Mul, Int, Int) => Instr::MulInt(dest, l, r),
        (Mul, Float, Float) => Instr::MulFloat(dest, l, r),
        (Mul, Int, Float) => promote_left!(Instr::MulFloat),
        (Mul, Float, Int) => promote_right!(Instr::MulFloat),

        (Div, Int, Int) => Instr::DivInt(dest, l, r),
        (Div, Float, Float) => Instr::DivFloat(dest, l, r),
        (Div, Int, Float) => promote_left!(Instr::DivFloat),
        (Div, Float, Int) => promote_right!(Instr::DivFloat),

        (Mod, Int, Int) => Instr::ModInt(dest, l, r),

        // ── Bitwise ──────────────────────────────────────────────────────────
        (BitAnd, Int, Int) => Instr::BitAnd(dest, l, r),
        (BitOr, Int, Int) => Instr::BitOr(dest, l, r),
        (BitXor, Int, Int) => Instr::BitXor(dest, l, r),
        (Shl, Int, Int) => Instr::Shl(dest, l, r),
        (Shr, Int, Int) => Instr::Shr(dest, l, r),

        // ── Comparisons — int ─────────────────────────────────────────────────
        (Eq, Int, Int) => Instr::CmpEqInt(dest, l, r),
        (Ne, Int, Int) => Instr::CmpNeInt(dest, l, r),
        (Lt, Int, Int) => Instr::CmpLtInt(dest, l, r),
        (Gt, Int, Int) => Instr::CmpGtInt(dest, l, r),
        (Le, Int, Int) => Instr::CmpLeInt(dest, l, r),
        (Ge, Int, Int) => Instr::CmpGeInt(dest, l, r),

        // ── Comparisons — float ───────────────────────────────────────────────
        (Eq, Float, Float) => Instr::CmpEqFloat(dest, l, r),
        (Ne, Float, Float) => Instr::CmpNeFloat(dest, l, r),
        (Lt, Float, Float) => Instr::CmpLtFloat(dest, l, r),
        (Gt, Float, Float) => Instr::CmpGtFloat(dest, l, r),
        (Le, Float, Float) => Instr::CmpLeFloat(dest, l, r),
        (Ge, Float, Float) => Instr::CmpGeFloat(dest, l, r),

        // ── Comparisons — mixed int/float ─────────────────────────────────────
        (Lt, Int, Float) => Instr::CmpLtIntFloat(dest, l, r),
        (Gt, Int, Float) => Instr::CmpGtIntFloat(dest, l, r),
        (Le, Int, Float) => Instr::CmpLeIntFloat(dest, l, r),
        (Ge, Int, Float) => Instr::CmpGeIntFloat(dest, l, r),
        (Lt, Float, Int) => Instr::CmpLtFloatInt(dest, l, r),
        (Gt, Float, Int) => Instr::CmpGtFloatInt(dest, l, r),
        (Le, Float, Int) => Instr::CmpLeFloatInt(dest, l, r),
        (Ge, Float, Int) => Instr::CmpGeFloatInt(dest, l, r),

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
        _ => Instr::BinOp(dest, op.clone(), l, r),
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
        (UnaryOpKind::Neg, JadeType::Int) => Instr::NegInt(dest, src),
        (UnaryOpKind::Neg, JadeType::Float) => Instr::NegFloat(dest, src),
        (UnaryOpKind::BitNot, JadeType::Int) => Instr::BitNot(dest, src),
        (UnaryOpKind::Not, JadeType::Bool) => Instr::Not(dest, src),
        _ => Instr::UnaryOp(dest, op.clone(), src),
    };
    em.chunk.emit(instr, span);
    Ok(dest)
}
