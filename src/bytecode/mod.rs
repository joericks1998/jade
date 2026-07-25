//! The instruction set both engines consume.
//!
//! `compiler::emit` produces a [`Chunk`]; [`crate::vm`] interprets one and
//! [`crate::aot`] lowers one to LLVM. It belongs to neither engine, which is
//! why it sits between them rather than inside either.
//!
//! Instructions are largely *monomorphic* — `AddInt` and `AddFloat` rather than
//! a single `Add` — because type inference has already run. That is what lets
//! the VM add two `i64`s without a tag dispatch and the AOT backend emit a bare
//! LLVM `add`, and it is why the two engines share this representation but no
//! execution code.

use std::{collections::HashMap, sync::Arc};
use parking_lot::Mutex;

use crate::frontend::{
    ast::{BinOpKind, UnaryOpKind},
    error::Span,
};

/// A register/slot index in the current call frame.
pub type Reg = u32;

/// Part of an f-string template produced by the emitter.
#[derive(Debug, Clone)]
pub enum FStrPart {
    Literal(String),
    Reg(Reg),
}

/// A compiled function body together with its metadata.
#[derive(Debug, Clone)]
pub struct CompiledFn {
    /// Parameter names, in declaration order. Slot 0 .. params.len()-1.
    pub params: Vec<String>,
    /// Default values for optional parameters, parallel to `params`.
    /// `None` means the parameter is required.
    pub defaults: Vec<Option<crate::vm::VmValue>>,
    /// Bytecode body.
    pub chunk: Chunk,
    /// Total number of register slots needed by this function frame.
    pub n_slots: u32,
    /// Source file this function was compiled from. Empty for top-level programs;
    /// set to the module's path when the function is loaded from an imported file.
    pub source_file: String,
    /// Persistent module scope shared by all functions from the same imported file.
    /// `None` for top-level functions. Reads and writes to module-level variables
    /// are routed through this scope so mutations survive across calls.
    pub module_scope: Option<Arc<Mutex<HashMap<String, crate::vm::VmValue>>>>,
}

/// A compiled code unit: top-level program or function body.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Diagnostic name ("<top>" or a function name).
    pub name: String,
    /// Instruction stream.
    pub code: Vec<Instr>,
    /// Parallel source spans — one per instruction, used for error messages.
    pub spans: Vec<Span>,
    /// Function literals embedded in this chunk (referred to by `LoadFn`).
    pub fn_defs: Vec<Arc<CompiledFn>>,
    /// Local slots that only ever hold arena-allocated arrays (from
    /// `MakeArrayArena`). The AOT backend must NOT decref these at scope exit and
    /// must store into them without refcounting — an arena pointer's memory is
    /// bulk-freed by `ArenaReset`, so touching it via the collector is a
    /// use-after-free. Empty unless the escape analysis found arena arrays.
    pub arena_slots: Vec<u32>,
}

impl Chunk {
    pub fn new(name: impl Into<String>) -> Self {
        Chunk {
            name: name.into(),
            code: Vec::new(),
            spans: Vec::new(),
            fn_defs: Vec::new(),
            arena_slots: Vec::new(),
        }
    }

    /// Append an instruction and return its index.
    pub fn emit(&mut self, instr: Instr, span: Span) -> usize {
        let idx = self.code.len();
        self.code.push(instr);
        self.spans.push(span);
        idx
    }

    /// Intern a `CompiledFn` and return its index in `fn_defs`.
    pub fn intern_fn(&mut self, f: Arc<CompiledFn>) -> usize {
        let idx = self.fn_defs.len();
        self.fn_defs.push(f);
        idx
    }

    /// Back-patch a `Jump`/`JumpIfFalse`/`JumpIfTrue` at `idx` to point to
    /// `target_idx`.  Offset convention: relative to the instruction *after*
    /// the jump (i.e. `offset = target_idx − (idx + 1)`), so offset 0 is a
    /// no-op and offset -1 is an infinite loop.
    pub fn patch_jump(&mut self, idx: usize, target_idx: usize) {
        let offset = target_idx as i32 - (idx as i32 + 1);
        match &mut self.code[idx] {
            Instr::Jump(o)             => *o = offset,
            Instr::JumpIfFalse(_, o)   => *o = offset,
            Instr::JumpIfTrue(_, o)    => *o = offset,
            Instr::SetupHandler(_, o)  => *o = offset,
            other => unreachable!("patch_jump on non-jump: {:?}", other),
        }
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }
}

/// Single bytecode instruction.
///
/// Register-based: every instruction that produces a value writes to a
/// destination `Reg`.  Jump offsets are signed and PC-relative (relative to
/// the instruction immediately *after* the jump instruction).
#[allow(dead_code)] // many variants are used only by the emitter or VM
#[derive(Debug, Clone)]
pub enum Instr {
    // ── Constant loads ─────────────────────────────────────────────────────
    LoadInt(Reg, i64),
    LoadFloat(Reg, f64),
    LoadBool(Reg, bool),
    LoadStr(Reg, String),
    LoadNil(Reg),
    /// Load a compiled function value from `chunk.fn_defs[idx]`.
    LoadFn(Reg, usize),
    /// Create a closure from `chunk.fn_defs[idx]`, capturing the current globals.
    MakeClosure(Reg, usize),

    // ── Variable access ────────────────────────────────────────────────────
    /// dest ← globals[name]
    GetGlobal(Reg, String),
    /// globals[name] ← src
    SetGlobal(String, Reg),
    /// dest ← frame.slots[slot]
    GetLocal(Reg, u32),
    /// frame.slots[slot] ← src
    SetLocal(u32, Reg),

    // ── Typed integer arithmetic ───────────────────────────────────────────
    AddInt(Reg, Reg, Reg),
    SubInt(Reg, Reg, Reg),
    MulInt(Reg, Reg, Reg),
    /// Integer division — runtime check for division by zero.
    DivInt(Reg, Reg, Reg),
    /// Integer remainder — runtime check for remainder by zero.
    ModInt(Reg, Reg, Reg),
    NegInt(Reg, Reg),

    // ── Typed float arithmetic ─────────────────────────────────────────────
    AddFloat(Reg, Reg, Reg),
    SubFloat(Reg, Reg, Reg),
    MulFloat(Reg, Reg, Reg),
    DivFloat(Reg, Reg, Reg),
    NegFloat(Reg, Reg),

    /// Widen an integer register to float.
    IntToFloat(Reg, Reg),
    /// String concatenation.
    ConcatStr(Reg, Reg, Reg),

    // ── Bitwise (integers only) ────────────────────────────────────────────
    BitAnd(Reg, Reg, Reg),
    BitOr(Reg, Reg, Reg),
    BitXor(Reg, Reg, Reg),
    BitNot(Reg, Reg),
    Shl(Reg, Reg, Reg),
    Shr(Reg, Reg, Reg),

    // ── Logical ────────────────────────────────────────────────────────────
    Not(Reg, Reg),

    // ── Dynamic fallback for Unknown-typed operands ────────────────────────
    BinOp(Reg, BinOpKind, Reg, Reg),
    UnaryOp(Reg, UnaryOpKind, Reg),

    // ── Typed comparisons → bool ───────────────────────────────────────────
    CmpEqInt(Reg, Reg, Reg),
    CmpNeInt(Reg, Reg, Reg),
    CmpLtInt(Reg, Reg, Reg),
    CmpGtInt(Reg, Reg, Reg),
    CmpLeInt(Reg, Reg, Reg),
    CmpGeInt(Reg, Reg, Reg),

    CmpEqFloat(Reg, Reg, Reg),
    CmpNeFloat(Reg, Reg, Reg),
    CmpLtFloat(Reg, Reg, Reg),
    CmpGtFloat(Reg, Reg, Reg),
    CmpLeFloat(Reg, Reg, Reg),
    CmpGeFloat(Reg, Reg, Reg),

    // Mixed int/float ordering.
    CmpLtIntFloat(Reg, Reg, Reg),
    CmpGtIntFloat(Reg, Reg, Reg),
    CmpLeIntFloat(Reg, Reg, Reg),
    CmpGeIntFloat(Reg, Reg, Reg),
    CmpLtFloatInt(Reg, Reg, Reg),
    CmpGtFloatInt(Reg, Reg, Reg),
    CmpLeFloatInt(Reg, Reg, Reg),
    CmpGeFloatInt(Reg, Reg, Reg),

    CmpEqBool(Reg, Reg, Reg),
    CmpNeBool(Reg, Reg, Reg),
    CmpLtBool(Reg, Reg, Reg),
    CmpGtBool(Reg, Reg, Reg),
    CmpLeBool(Reg, Reg, Reg),
    CmpGeBool(Reg, Reg, Reg),

    CmpEqStr(Reg, Reg, Reg),
    CmpNeStr(Reg, Reg, Reg),
    CmpLtStr(Reg, Reg, Reg),
    CmpGtStr(Reg, Reg, Reg),
    CmpLeStr(Reg, Reg, Reg),
    CmpGeStr(Reg, Reg, Reg),

    /// Dynamic comparison for Unknown-typed operands.
    CmpEq(Reg, Reg, Reg),
    CmpNe(Reg, Reg, Reg),
    CmpLt(Reg, Reg, Reg),
    CmpGt(Reg, Reg, Reg),
    CmpLe(Reg, Reg, Reg),
    CmpGe(Reg, Reg, Reg),

    // ── Control flow ───────────────────────────────────────────────────────
    /// Unconditional PC-relative jump.
    Jump(i32),
    /// Jump if `reg` is `false`.
    JumpIfFalse(Reg, i32),
    /// Jump if `reg` is `true`.
    JumpIfTrue(Reg, i32),

    // ── Function calls ─────────────────────────────────────────────────────
    /// dest ← callee_reg(arg_regs…)
    Call(Reg, Reg, Vec<Reg>),
    /// dest ← callee_reg(mixed_args) where args may be positional (None) or named (Some(name)).
    CallNamed(Reg, Reg, Vec<(Option<String>, Reg)>),
    Return(Option<Reg>),

    // ── Collections ────────────────────────────────────────────────────────
    MakeArray(Reg, Vec<Reg>),
    /// Like `MakeArray`, but the compiler's escape analysis proved this array
    /// does not escape its region, so the AOT backend allocates it in the
    /// per-frame bump arena (and skips refcounting it). The VM treats it exactly
    /// as `MakeArray` — arena vs heap is an AOT implementation detail, invisible
    /// in output, so backend parity holds.
    MakeArrayArena(Reg, Vec<Reg>),
    /// Snapshot the arena cursor into `dest` (an i64 token). AOT-only effect;
    /// a no-op that yields 0 in the VM.
    ArenaMark(Reg),
    /// Reset the arena to the token in `reg`, freeing everything arena-allocated
    /// since the matching `ArenaMark`. AOT-only effect; a no-op in the VM.
    ArenaReset(Reg),
    /// (dest, [(key_reg, val_reg), …])
    MakeDict(Reg, Vec<(Reg, Reg)>),
    /// dest ← obj_reg[idx_reg]
    GetIndex(Reg, Reg, Reg),
    /// obj_reg[idx_reg] ← val_reg  (value semantics: modifies the slot)
    SetIndex(Reg, Reg, Reg),

    // ── Struct ─────────────────────────────────────────────────────────────
    /// (dest, type_name, [(field_name, val_reg, is_prompt), …])
    MakeStruct(Reg, String, Vec<(String, Reg, bool)>),
    /// dest ← obj_reg.field_name
    GetField(Reg, Reg, String),
    /// obj_reg.field_name ← val_reg
    SetField(Reg, String, Reg),

    // ── String interpolation ───────────────────────────────────────────────
    BuildFStr(Reg, Vec<FStrPart>),

    // ── Prompt ─────────────────────────────────────────────────────────────
    MakePrompt(Reg, Reg),
    /// (dest, prompt_reg, output_type, grammar_reg)
    /// grammar_reg holds a VmValue::Grammar for user-supplied GBNF pattern.
    PromptDeref(Reg, Reg, Option<String>, Option<Reg>),

    // ── Exception handling ─────────────────────────────────────────────────
    /// Raise the value in `val` as an exception. If a handler frame is active,
    /// stores the value in `caught_reg` and jumps to the handler; otherwise
    /// stores it in `VmState::raised_exception` and propagates `JadeError::Exception`.
    Raise(Reg),
    /// Push an exception handler frame. On exception, stores the caught value in
    /// `caught_reg` and jumps `offset` instructions past this instruction.
    SetupHandler(Reg, i32),
    /// Pop the current exception handler frame (try block completed normally).
    PopHandler,
    /// Store the struct `type_name` of `src` as a string in `dest`.
    /// Yields an empty string if `src` is not a struct. Used for typed catch matching.
    GetTypeName(Reg, Reg),

    // ── Misc ───────────────────────────────────────────────────────────────
    Move(Reg, Reg),
    Halt,

    // ── Async ──────────────────────────────────────────────────────────────────
    /// dest ← start async fn immediately, return Future handle.
    Spawn(Reg, Reg, Vec<Reg>),
    /// dest ← block until Future resolves and unwrap result.
    Await(Reg, Reg),
    /// dest ← wait for all Futures in argument order, return Array.
    Join(Reg, Vec<Reg>),

    // ── Imports ────────────────────────────────────────────────────────────
    /// Run the full pipeline for the file at `path` and bind its exports under `namespace`.
    /// `namespace` is either the explicit `as` alias or the file's stem (e.g. `lib` for `lib.jde`).
    /// stdlib packages ignore this field and use their own `global_name`.
    ImportFile(String, String),
    /// `from X use Y, Z` — load package/file at path, then bind only the listed names directly.
    ImportFrom(String, Vec<String>),
}

#[cfg(test)]
mod tests;
