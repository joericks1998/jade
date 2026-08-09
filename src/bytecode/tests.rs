//! Unit tests for the bytecode instruction set and `Chunk`.
#![allow(clippy::all)]

use crate::bytecode::{Chunk, Instr};
use crate::frontend::error::Span;

fn sp() -> Span {
    Span { line: 1, col: 1 }
}

#[test]
fn chunk_new_is_empty() {
    let c = Chunk::new("<top>");
    assert_eq!(c.name, "<top>");
    assert_eq!(c.len(), 0);
    assert!(c.code.is_empty());
    assert!(c.spans.is_empty());
    assert!(c.fn_defs.is_empty());
}

#[test]
fn chunk_emit_returns_index_and_tracks_spans() {
    let mut c = Chunk::new("t");
    let i0 = c.emit(Instr::LoadInt(0, 1), sp());
    let i1 = c.emit(Instr::LoadInt(1, 2), sp());
    assert_eq!(i0, 0);
    assert_eq!(i1, 1);
    assert_eq!(c.len(), 2);
    // Spans stay parallel to code — one per instruction.
    assert_eq!(c.spans.len(), c.code.len());
}

#[test]
fn patch_jump_forward_offset_is_relative_to_next_instr() {
    let mut c = Chunk::new("t");
    // idx 0: the jump we will patch to target idx 3.
    let j = c.emit(Instr::Jump(0), sp());
    c.emit(Instr::LoadInt(0, 1), sp());
    c.emit(Instr::LoadInt(0, 2), sp());
    // target idx == 3 (one past the last emitted instruction).
    c.patch_jump(j, 3);
    // offset = target - (idx + 1) = 3 - 1 = 2
    match c.code[j] {
        Instr::Jump(o) => assert_eq!(o, 2),
        ref other => panic!("expected Jump, got {:?}", other),
    }
}

#[test]
fn patch_jump_zero_offset_is_noop_fallthrough() {
    let mut c = Chunk::new("t");
    let j = c.emit(Instr::Jump(99), sp());
    // target is the instruction immediately after the jump → offset 0.
    c.patch_jump(j, j + 1);
    match c.code[j] {
        Instr::Jump(o) => assert_eq!(o, 0, "jump-to-next is a zero offset"),
        ref other => panic!("expected Jump, got {:?}", other),
    }
}

#[test]
fn patch_jump_backward_offset_is_negative() {
    let mut c = Chunk::new("t");
    c.emit(Instr::LoadInt(0, 1), sp()); // idx 0 (loop top)
    let j = c.emit(Instr::Jump(0), sp()); // idx 1
    c.patch_jump(j, 0);
    // offset = 0 - (1 + 1) = -2
    match c.code[j] {
        Instr::Jump(o) => assert_eq!(o, -2),
        ref other => panic!("expected Jump, got {:?}", other),
    }
}

#[test]
fn patch_jump_handles_conditional_variants() {
    let mut c = Chunk::new("t");
    let jf = c.emit(Instr::JumpIfFalse(0, 0), sp());
    let jt = c.emit(Instr::JumpIfTrue(1, 0), sp());
    c.patch_jump(jf, 5);
    c.patch_jump(jt, 5);
    match c.code[jf] {
        Instr::JumpIfFalse(r, o) => {
            assert_eq!(r, 0);
            assert_eq!(o, 4);
        }
        ref other => panic!("expected JumpIfFalse, got {:?}", other),
    }
    match c.code[jt] {
        Instr::JumpIfTrue(r, o) => {
            assert_eq!(r, 1);
            assert_eq!(o, 3);
        }
        ref other => panic!("expected JumpIfTrue, got {:?}", other),
    }
}

#[test]
#[should_panic(expected = "patch_jump on non-jump")]
fn patch_jump_on_non_jump_panics() {
    let mut c = Chunk::new("t");
    let idx = c.emit(Instr::LoadInt(0, 1), sp());
    c.patch_jump(idx, 5);
}

#[test]
fn intern_fn_returns_sequential_indices() {
    use crate::bytecode::CompiledFn;
    use std::sync::Arc;
    let mut c = Chunk::new("t");
    let mk = |name: &str| {
        Arc::new(CompiledFn {
            params: vec![],
            defaults: vec![],
            chunk: Chunk::new(name),
            n_slots: 0,
            source_file: String::new(),
            module_scope: None,
            is_generator: false,
        })
    };
    let a = c.intern_fn(mk("a"));
    let b = c.intern_fn(mk("b"));
    assert_eq!(a, 0);
    assert_eq!(b, 1);
    assert_eq!(c.fn_defs.len(), 2);
    assert_eq!(c.fn_defs[0].chunk.name, "a");
}
