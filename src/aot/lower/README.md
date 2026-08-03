# `src/aot/lower/` — bytecode → LLVM IR

## What this subtree is

The translation itself: one LLVM IR sequence per bytecode `Instr`, plus the calls into `jade-runtime`'s `jrt_*` C-ABI surface for anything needing the heap, collections, strings, tasks, or inference. `cfg.rs` has already turned the flat instruction stream into basic blocks by the time this code runs; `aot/mod.rs` takes the module it produces and hands it to the linker.

## Why it is split this way

Until v1.2.0 all of it was a single `lower.rs` of 5,224 lines, a third of which was one `lower_instr` match. It was the largest file in the repo by a wide margin and the only place that ignored the "one concern per file" rule the rest of the tree follows.

The split is *purely mechanical*: every item moved verbatim, and the only edits were adding `pub(super)` where a call now crosses a module line. Nothing was rewritten, reordered, or optimized. That mattered, because it is what let the change be verified rather than reviewed — see "Building and testing".

`Lowerer` is defined in `mod.rs` and each file adds its own `impl` block, so a helper lives beside the operations that use it. The submodules are peers that call freely into one another, so `mod.rs` lifts their `pub(super)` items into the shared parent scope and each file opens with `use super::*`. That keeps every call site spelled exactly as it was written when it all lived in one file.

## What each file does

- **`mod.rs`** — the tagged-value constants, `struct Lowerer` and `FnCtx`, struct-default construction, and the three entry points: `lower_program` (a whole program), `lower_chunk` (one chunk, used by tests), and `lower_body` (a single function body).
- **`abi.rs`** — the tagged-value ABI: int/bool/float boxing and unboxing, pointer tagging, register slot load and store, global slots. Everything else is written in terms of these.
- **`arith.rs`** — integer, float, and bitwise arithmetic, plus the comparison family. Includes the dynamic paths (`any2`, `eq_any`, `cmp_any`) taken when a register's type is not statically known.
- **`strings.rs`** — string literals and interning, concatenation, ordering, and the primitive string methods.
- **`rc.rs`** — reference counting: `incref`, `decref`, `retain`, slot replacement, and scope exit.
- **`exc.rs`** — exception frames: `throw`, the `setjmp` shim, and frame push/pop. Read the handler-stack gotcha in `../README.md` before touching it.
- **`calls.rs`** — call resolution and emission. `resolve_user_calls` decides, per call site, which `CallKind` applies (direct, method, module, native, spawn, stream); the emission side covers indirect calls and function boxing.
- **`builtins.rs`** — builtin dispatch: native package calls, stdlib module calls, primitive value methods, and runtime-dispatched struct methods. The `chunk_*_supported` predicates that gate them live here too.
- **`llm.rs`** — prompt values and dereferences, including the `stream(?p)` lowering. The smallest file, and the one the 1.2.0 streaming work will grow.
- **`instr.rs`** — the `lower_instr` match. Now a dispatcher: each arm either inlines a couple of lines or delegates to one of the files above.
- **`tests.rs`** — the backend's unit tests, moved verbatim.

## Who uses it

*Depends on:* `bytecode/` for the instruction set, `vm::VmValue` for compile-time constants, `frontend::ast` for struct-default expressions, `aot::cfg` for basic blocks, and `inkwell` for LLVM.

*Used by:* `aot/mod.rs` only, through `lower_program`.

## Gotchas

**The gotchas that matter here are in the parent's README**, not this one: refcount ownership on borrowed value words, `jrt_require_kind` before untagging a receiver, the thread-wide handler stack, and the rule that any new opcode or builtin must be lowered here or the two engines quietly disagree. Read [`../README.md`](../README.md) before editing.

**Put a new helper in the file that owns its concern, not in `instr.rs`.** The reason the old file reached 5,000 lines is that `lower_instr` was the path of least resistance for every addition. An arm that grows past a few lines belongs in a topic file with a thin call from the match.

**`use super::*` makes every sibling's items look local.** Convenient, and the reason the split needed no call-site edits, but it also means a name collision between two submodules surfaces as a confusing ambiguity error rather than at its definition. Keep new `pub(super)` names distinct.

## Building and testing

```sh
cargo test aot::
./src/scripts/backend-parity.sh
```

Because a change here alters generated code rather than observable behavior, tests alone are a weak signal. The strongest check available is to diff the IR itself:

```sh
for f in $(find examples -name '*.jde'); do
  ./target/debug/jade build "$f" --emit ir > "before/$(echo $f | tr / _).ll" 2>/dev/null
done
# ... make the change, rebuild, emit again into after/, then diff the two trees
```

For a refactor that is meant to be behavior-preserving, every file should come out byte-identical. That is how the original split was verified: 71 of the 74 examples emit IR, all 71 were unchanged, and the parity gate stayed at 68 ok / 6 skipped / 0 failed.
