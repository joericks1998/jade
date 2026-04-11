use std::rc::Rc;

use crate::interpreter::{
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
    /// Bytecode body.
    pub chunk: Chunk,
    /// Total number of register slots needed by this function frame.
    pub n_slots: u32,
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
    pub fn_defs: Vec<Rc<CompiledFn>>,
}

impl Chunk {
    pub fn new(name: impl Into<String>) -> Self {
        Chunk {
            name: name.into(),
            code: Vec::new(),
            spans: Vec::new(),
            fn_defs: Vec::new(),
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
    pub fn intern_fn(&mut self, f: Rc<CompiledFn>) -> usize {
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
            Instr::Jump(o)           => *o = offset,
            Instr::JumpIfFalse(_, o) => *o = offset,
            Instr::JumpIfTrue(_, o)  => *o = offset,
            other => unreachable!("patch_jump on non-jump: {:?}", other),
        }
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
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
    Return(Option<Reg>),

    // ── Collections ────────────────────────────────────────────────────────
    MakeArray(Reg, Vec<Reg>),
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
    PromptDeref(Reg, Reg, Option<String>),

    // ── Built-ins ──────────────────────────────────────────────────────────
    CallPrint(Vec<Reg>),
    CallLen(Reg, Reg),

    // ── Misc ───────────────────────────────────────────────────────────────
    Move(Reg, Reg),
    Halt,
}
