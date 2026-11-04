# `src/bytecode/`: the instruction set between the compiler and the backend

## What this subtree is

This is the register-machine instruction set that sits between the compiler and the execution engine. `compiler/emit.rs` produces a `Chunk`, and `src/codegen/`, driven by `src/aot/`, lowers one to LLVM IR. Until v1.4.8 a bytecode VM in `src/vm/` also interpreted one.

It is small on purpose. It holds types and one small jump-patching helper, and no execution logic at all.

## Why it lives here rather than inside an engine

Until v1.4.8 there were two engines, and the instruction set belonged to neither, which is why it sits between them. Putting it inside `vm/` would have made the interpreter the reference implementation and the AOT backend a follower. Instead, the language was defined by what the two agreed on, and a parity script checked that agreement on every example. v2 has one engine, and `src/scripts/golden.sh` now holds every example to the output recorded where the two agreed.

The instructions are mostly *monomorphic*, meaning `AddInt` and `AddFloat` rather than one `Add`, because type inference has already run by the time anything reaches here. That single decision is why the AOT backend can emit a bare LLVM `add`, and why the VM could add two `i64` values with no tag dispatch. It is also why the two engines shared this representation and no execution code.

## What each file does

- *`mod.rs`* holds the whole instruction set. `Instr` is the opcodes. `Chunk` is a linear `Vec<Instr>` plus its constant pools. `CompiledFn` is a function body with its parameters, defaults, slot count, and source file, plus two facts about the body that a caller needs and cannot see: `is_generator` and `is_async`. Both exist because a call site that only has the *value* has no static type to consult — that is what lets `[1, 2].map(some_async_fn)` start tasks. `FStrPart` holds f-string template pieces, and `Reg` is a register index in the current call frame. `Const` is a value known at compile time, used for parameter and struct-field defaults: nil, an int, a float, a bool, a string, or an empty array or dict. It replaced the VM's `VmValue` for those defaults in v2.

  Jumps are relative to the program counter. `patch_jump` writes an offset relative to the instruction *after* the jump, so a target is `idx + 1 + offset`.
- *`tests.rs`* holds the instruction-set tests.

## Who uses it

*Depends on:* `frontend/` for `BinOpKind`, `UnaryOpKind`, and `Span`, and nothing else.

*Used by:* `compiler/emit.rs` writes chunks. `codegen/cfg.rs` reconstructs a control-flow graph from the flat instruction stream and the rest of `codegen/` translates each opcode into LLVM IR.

## Gotchas

Adding an opcode is a change in two places. Emit it in `compiler/emit.rs`, and lower it in `src/codegen/`. *The AOT backend treats an opcode it cannot lower as a hard build error*, and there is no fallback path. So skipping the second step breaks `jade build` and `jade run` for any program that reaches the new instruction.

It is a change in *three* places if the instruction changes the serialized shape at all. Bump `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`, or a TIR cached by the previous build will deserialize into a chunk this one cannot run. A tripwire test pins the number.

The jump-offset convention is easy to get wrong by one. Use `patch_jump` rather than computing offsets by hand.

*Why `MakeStruct` carries an optional base register.* It is the `...` of a copy-with literal, and the copy runs on this instruction rather than being expanded by the emitter. Two things force that. The base expression has to be evaluated exactly once, which one register gives and one field access per field would not. And the fields to copy are the ones the type declares, which a struct inheriting a parent from another file does not know while it is being emitted: the parent arrives when the import is merged, which the AOT backend does by inlining and the VM did at `ImportFile`. The parent is known by the time the instruction runs.

*Why there are two index-assign opcodes.* `SetIndex` takes a register, and `SetIndexGlobal` takes a name. They do the same thing, and the split exists because of what a dict is: a value held copy-on-write, so a write copies whenever anything *else* is also holding the dict.

Loading a global into a register first makes that register a second holder. Every write then copied, and filling a dict cost time proportional to the square of its size. `SetIndexGlobal` owns the binding for the write instead. A local needs no equivalent, because a local *is* a register slot, so the emitter hands `SetIndex` the binding directly rather than a copy.
