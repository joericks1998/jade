//! Control-flow-graph reconstruction from the flat bytecode stream.
//!
//! `emit.rs` produces a `Chunk`: a linear `Vec<Instr>` with PC-relative jumps
//! (offsets are relative to the instruction *after* the jump, per
//! `bytecode.rs::patch_jump`: `target = idx + 1 + offset`). The LLVM backend
//! needs this as basic blocks with explicit edges — an SSA function is built by
//! creating one LLVM `BasicBlock` per block here and lowering each instruction
//! into it (per-register `alloca` + `mem2reg` recovers SSA).
//!
//! This module does only the block-boundary + edge computation; it holds no
//! LLVM state, so it is unit-testable in isolation and is the first brick of
//! the Chunk→LLVM path that replaces the `TProgram` re-lowering in `expr.rs`.
//!
//! ## Exceptions
//!
//! AOT exceptions use `setjmp`/`longjmp`, not LLVM `invoke`/`landingpad`, so a
//! handler is *not* a normal CFG successor of `SetupHandler` — it is entered
//! when the frame's `setjmp` returns non-zero after a `longjmp`. We therefore
//! record a handler target as a **leader** (it begins a block) but not as a
//! normal-flow successor; the lowering wires the setjmp→handler entry edge.

use std::collections::{BTreeSet, HashMap};

use crate::bytecode::Instr;

/// A maximal straight-line run of instructions `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    /// Index of the first instruction (a leader) in `chunk.code`.
    pub start: usize,
    /// One past the last instruction in this block.
    pub end: usize,
    /// Normal-control-flow successor block indices (into [`Cfg::blocks`]).
    /// Empty for `Return`/`Halt`/`Raise`. Does not include exception handlers.
    pub succs: Vec<usize>,
}

/// The reconstructed control-flow graph of one `Chunk`.
#[derive(Debug, Clone)]
pub struct Cfg {
    /// Blocks in ascending `start` order; block 0 is the entry.
    pub blocks: Vec<BasicBlock>,
    /// Leader instruction index → block index.
    pub block_at: HashMap<usize, usize>,
}

/// PC-relative jump target: `idx + 1 + offset` (see `bytecode.rs::patch_jump`).
#[inline]
fn jump_target(idx: usize, offset: i32) -> usize {
    (idx as i64 + 1 + offset as i64) as usize
}

/// Result of classifying a control-transfer instruction.
struct ControlFlow {
    /// Normal-flow branch targets (instruction indices).
    targets: Vec<usize>,
    /// Whether `idx + 1` is also reachable.
    fallthrough: bool,
    /// A handler entry that is a leader but not a normal successor.
    handler: Option<usize>,
}

/// Instructions that transfer control (end a block). Returns the explicit
/// branch/handler targets, and whether control also falls through to `idx + 1`.
/// `None` for ordinary instructions (which never end a block on their own).
fn control_of(instr: &Instr, idx: usize) -> Option<ControlFlow> {
    use Instr::*;
    let t = |off: i32| jump_target(idx, off);
    Some(match instr {
        Jump(off) => ControlFlow { targets: vec![t(*off)], fallthrough: false, handler: None },
        JumpIfFalse(_, off) | JumpIfTrue(_, off) => {
            ControlFlow { targets: vec![t(*off)], fallthrough: true, handler: None }
        }
        // The handler is a leader but is reached via longjmp, so it is not a
        // normal successor — record it only as a leader (see module docs).
        SetupHandler(_, off) => {
            ControlFlow { targets: vec![], fallthrough: true, handler: Some(t(*off)) }
        }
        Return(_) | Halt => ControlFlow { targets: vec![], fallthrough: false, handler: None },
        // Raise transfers to the active handler via longjmp — no normal edge.
        Raise(_) => ControlFlow { targets: vec![], fallthrough: false, handler: None },
        _ => return None,
    })
}

/// Reconstruct the CFG of a bytecode chunk.
pub fn build(code: &[Instr]) -> Cfg {
    if code.is_empty() {
        return Cfg { blocks: Vec::new(), block_at: HashMap::new() };
    }

    // ── Pass 1: collect leaders ────────────────────────────────────────────
    let mut leaders: BTreeSet<usize> = BTreeSet::new();
    leaders.insert(0);
    for (idx, instr) in code.iter().enumerate() {
        if let Some(cf) = control_of(instr, idx) {
            for t in &cf.targets {
                leaders.insert(*t);
            }
            if let Some(h) = cf.handler {
                leaders.insert(h);
            }
            // The instruction after any control transfer begins a new block
            // (fallthrough target, or unreachable start after an unconditional
            // jump/return — either way a leader).
            if idx + 1 < code.len() {
                leaders.insert(idx + 1);
            }
        }
    }

    // ── Pass 2: form blocks between consecutive leaders ────────────────────
    let leader_vec: Vec<usize> = leaders.iter().copied().collect();
    let mut block_at: HashMap<usize, usize> = HashMap::new();
    for (bi, &start) in leader_vec.iter().enumerate() {
        block_at.insert(start, bi);
    }
    let mut blocks: Vec<BasicBlock> = Vec::with_capacity(leader_vec.len());
    for (bi, &start) in leader_vec.iter().enumerate() {
        let end = leader_vec.get(bi + 1).copied().unwrap_or(code.len());
        blocks.push(BasicBlock { start, end, succs: Vec::new() });
    }

    // ── Pass 3: wire successors from each block's terminator ───────────────
    for bi in 0..blocks.len() {
        let end = blocks[bi].end;
        let last = end - 1;
        let mut succs = Vec::new();
        match control_of(&code[last], last) {
            Some(cf) => {
                for t in cf.targets {
                    if let Some(&b) = block_at.get(&t) {
                        succs.push(b);
                    }
                }
                if cf.fallthrough {
                    if let Some(&b) = block_at.get(&end) {
                        succs.push(b);
                    }
                }
            }
            // A block whose last instruction is not a control transfer simply
            // falls through to the next block.
            None => {
                if let Some(&b) = block_at.get(&end) {
                    succs.push(b);
                }
            }
        }
        blocks[bi].succs = succs;
    }

    Cfg { blocks, block_at }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Instr::*;

    // A simple `if`: cond in r0, jump-if-false over the then-block.
    //   0: LoadBool r0, true
    //   1: JumpIfFalse r0, +2   (→ 4)
    //   2: LoadInt r1, 1
    //   3: Jump +1              (→ 5)
    //   4: LoadInt r1, 2
    //   5: Halt
    fn if_chunk() -> Vec<Instr> {
        vec![LoadBool(0, true), JumpIfFalse(0, 2), LoadInt(1, 1), Jump(1), LoadInt(1, 2), Halt]
    }

    #[test]
    fn leaders_and_blocks() {
        let cfg = build(&if_chunk());
        // Leaders: 0 (entry), 2 (after JumpIfFalse), 4 (target + after Jump), 5 (target).
        let starts: Vec<usize> = cfg.blocks.iter().map(|b| b.start).collect();
        assert_eq!(starts, vec![0, 2, 4, 5]);
    }

    #[test]
    fn conditional_has_two_successors() {
        let cfg = build(&if_chunk());
        // Block 0 ends at JumpIfFalse(→4) with fallthrough to 2.
        let b0 = &cfg.blocks[0];
        let mut s = b0.succs.clone();
        s.sort();
        assert_eq!(s, vec![cfg.block_at[&2], cfg.block_at[&4]]);
    }

    #[test]
    fn unconditional_jump_single_successor() {
        let cfg = build(&if_chunk());
        // Block starting at 2 ends in Jump(→5): one successor, no fallthrough to 4.
        let b = &cfg.blocks[cfg.block_at[&2]];
        assert_eq!(b.succs, vec![cfg.block_at[&5]]);
    }

    #[test]
    fn halt_has_no_successors() {
        let cfg = build(&if_chunk());
        let b = &cfg.blocks[cfg.block_at[&5]];
        assert!(b.succs.is_empty());
    }

    #[test]
    fn straight_line_is_one_block() {
        let code = vec![LoadInt(0, 1), LoadInt(1, 2), AddInt(2, 0, 1), Return(Some(2))];
        let cfg = build(&code);
        assert_eq!(cfg.blocks.len(), 1);
        assert_eq!(cfg.blocks[0].start, 0);
        assert_eq!(cfg.blocks[0].end, 4);
        assert!(cfg.blocks[0].succs.is_empty()); // ends in Return
    }

    #[test]
    fn backward_jump_loop() {
        // 0: LoadInt r0, 0
        // 1: ... body ...
        // 2: Jump -2   (→ 1) infinite loop back to body
        let code = vec![LoadInt(0, 0), LoadInt(1, 1), Jump(-2)];
        let cfg = build(&code);
        // Leaders: 0, 1 (jump target). Block 1 (start=1) loops to itself.
        assert!(cfg.block_at.contains_key(&1));
        let bi = cfg.block_at[&1];
        assert_eq!(cfg.blocks[bi].succs, vec![bi]);
    }

    #[test]
    fn handler_is_a_leader_but_not_a_normal_successor() {
        // 0: SetupHandler r0, +3  (handler at 4)
        // 1: LoadInt r1, 1
        // 2: PopHandler
        // 3: Jump +1              (→ 5, skip handler)
        // 4: LoadInt r1, 9        (handler body)
        // 5: Halt
        let code =
            vec![SetupHandler(0, 3), LoadInt(1, 1), PopHandler, Jump(1), LoadInt(1, 9), Halt];
        let cfg = build(&code);
        // 4 is a leader (handler entry)...
        assert!(cfg.block_at.contains_key(&4), "handler target must be a leader");
        // ...but block 0 (ending in SetupHandler) only falls through to 1, not 4.
        let b0 = &cfg.blocks[0];
        assert_eq!(b0.succs, vec![cfg.block_at[&1]]);
    }
}
