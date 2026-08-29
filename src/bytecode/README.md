# `src/bytecode/`: the instruction set both engines consume

## What this subtree is

This is the register-machine instruction set that sits between the compiler and the two execution engines. `compiler/emit.rs` produces a `Chunk`. `src/vm/` interprets one, and `src/aot/` lowers one to LLVM IR.

It is small on purpose. It holds types and one small jump-patching helper, and no execution logic at all.

## Why it lives here rather than inside an engine

The instruction set belongs to neither engine, which is why it sits between them. Putting it inside `vm/` would make the interpreter the reference implementation and the AOT backend a follower. Instead, the language is defined by what the two agree on, and `src/scripts/backend-parity.sh` checks that agreement on every example.

The instructions are mostly *monomorphic*, meaning `AddInt` and `AddFloat` rather than one `Add`, because type inference has already run by the time anything reaches here. That single decision is why the VM can add two `i64` values with no tag dispatch, and why the AOT backend can emit a bare LLVM `add`. It is also why the two engines share this representation and no execution code.

## What each file does

- *`mod.rs`* holds the whole instruction set. `Instr` is the opcodes. `Chunk` is a linear `Vec<Instr>` plus its constant pools. `CompiledFn` is a function body with its parameters, defaults, slot count, and source file. `FStrPart` holds f-string template pieces, and `Reg` is a register index in the current call frame.

  Jumps are relative to the program counter. `patch_jump` writes an offset relative to the instruction *after* the jump, so a target is `idx + 1 + offset`.
- *`tests.rs`* holds the instruction-set tests.

## Who uses it

*Depends on:* `frontend/` for `BinOpKind`, `UnaryOpKind`, and `Span`, and `vm::VmValue` for the constants stored in `CompiledFn::defaults`.

*Used by:* `compiler/emit.rs` writes chunks. `vm/dispatch.rs` decodes and runs them. `codegen/cfg.rs` reconstructs a control-flow graph from the flat instruction stream and the rest of `codegen/` translates each opcode into LLVM IR.

## Gotchas

Adding an opcode is a change in three places. Emit it in `compiler/emit.rs`, interpret it in `vm/dispatch.rs`, and lower it in `src/codegen/`. *The AOT backend treats an opcode it cannot lower as a hard build error*, and there is no fallback path. So skipping the third step breaks `jade build` for any program that reaches the new instruction.

It is a change in *four* places if the instruction changes the serialized shape at all. Bump `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`, or a TIR cached by the previous build will deserialize into a chunk this one cannot run. A tripwire test pins the number.

The jump-offset convention is easy to get wrong by one. Use `patch_jump` rather than computing offsets by hand.

*Why `MakeStruct` carries an optional base register.* It is the `...` of a copy-with literal, and the copy runs on this instruction rather than being expanded by the emitter. Two things force that. The base expression has to be evaluated exactly once, which one register gives and one field access per field would not. And the fields to copy are the ones the type declares, which a struct inheriting a parent from another file does not know while it is being emitted: the parent arrives when the engine merges the import, which the VM does at `ImportFile` and the AOT does by inlining. Both engines know by the time the instruction runs.

*Why there are two index-assign opcodes.* `SetIndex` takes a register, and `SetIndexGlobal` takes a name. They do the same thing, and the split exists because of what a dict is: a value held copy-on-write, so a write copies whenever anything *else* is also holding the dict.

Loading a global into a register first makes that register a second holder. Every write then copied, and filling a dict cost time proportional to the square of its size. `SetIndexGlobal` owns the binding for the write instead. A local needs no equivalent, because a local *is* a register slot, so the emitter hands `SetIndex` the binding directly rather than a copy.
