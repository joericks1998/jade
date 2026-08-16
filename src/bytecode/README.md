# `src/bytecode/` — the instruction set both engines consume

## What this subtree is

The register-machine instruction set that sits between the compiler and the two execution engines. `compiler/emit.rs` produces a `Chunk`; `src/vm/` interprets one and `src/aot/` lowers one to LLVM IR.

It is small on purpose — types and a little jump-patching helper, no execution logic at all.

## Why it lives here rather than inside an engine

It belongs to neither engine, which is why it sits between them. Putting the instruction set inside `vm/` would make the interpreter the reference implementation and the AOT backend a follower; the language is instead defined by what the two agree on, and `src/scripts/backend-parity.sh` checks that agreement on every example.

The instructions are largely *monomorphic* — `AddInt` and `AddFloat` rather than one `Add` — because type inference has already run by the time anything gets here. That single decision is why the VM can add two `i64`s without a tag dispatch and the AOT backend can emit a bare LLVM `add`, and it is why the two engines share this representation but no execution code.

## What each file does

- **`mod.rs`** — the whole instruction set. `Instr` (the opcodes), `Chunk` (a linear `Vec<Instr>` plus its constant pools), `CompiledFn` (a function body with its parameters, defaults, slot count, and source file), `FStrPart` (f-string template pieces), and `Reg` (a register/slot index in the current call frame). Jumps are PC-relative: `patch_jump` writes offsets relative to the instruction *after* the jump, so a target is `idx + 1 + offset`.
- **`tests.rs`** — instruction-set tests.

## Who uses it

*Depends on:* `frontend/` for `BinOpKind`, `UnaryOpKind`, and `Span`; `vm::VmValue` for the constants stored in `CompiledFn::defaults`.

*Used by:* `compiler/emit.rs` writes chunks. `vm/dispatch.rs` decodes and runs them. `codegen/cfg.rs` reconstructs a control-flow graph from the flat instruction stream and the rest of `codegen/` translates each opcode into LLVM IR.

## Gotchas

Adding an opcode is a three-place change: emit it in `compiler/emit.rs`, interpret it in `vm/dispatch.rs`, and lower it in `src/codegen/`. **The AOT backend treats an opcode it cannot lower as a hard build error** — there is no fallback path — so skipping the third step breaks `jade build` for any program that reaches the new instruction.

It is a *four*-place change if the instruction changes the serialized shape at all: bump `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`, or a TIR cached by the previous build deserializes into a chunk this one cannot run. A tripwire test pins the number.

The jump-offset convention is easy to get wrong by one. Use `patch_jump` rather than computing offsets by hand.

**Why there are two index-assign opcodes.** `SetIndex` takes a register; `SetIndexGlobal` takes a name. They do the same thing, and the split exists because of what a dict is: a value, held copy-on-write, so a write copies whenever anything *else* is holding the dict. Loading a global into a register first makes that register a second holder, so every write copied and filling a dict was quadratic in its size. `SetIndexGlobal` owns the binding for the write instead. A local needs no equivalent — a local *is* a register slot, so the emitter hands `SetIndex` the binding directly rather than a copy of it.
