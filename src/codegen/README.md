# `src/codegen/` — bytecode → LLVM IR

## What this subtree is

The translation, and only the translation: a bytecode `Chunk` in, an LLVM module out. `cfg.rs` turns the flat instruction stream into basic blocks, and the rest of the directory lowers one opcode at a time, calling into `jade-runtime`'s `jrt_*` C-ABI surface for anything that needs the heap, collections, strings, tasks, or inference.

Nothing here writes a file, runs a linker, or knows what a project is. [`src/aot/`](../aot/README.md) does all of that: it resolves imports, calls `lower_program`, wraps the result in a `main()`, and drives `cc`.

## Why it sits beside `aot` rather than inside it

It used to be `src/aot/lower/`, one level down inside the backend that calls it. That put the half of `jade build` that is about *the language* underneath the half that is about *producing a file*, and the two are not related that way. Lowering is where the VM and the compiled path have to agree on what an opcode means — it is a peer of `src/vm/`, in the same sense `src/vm/` is a peer of the AOT driver. Sitting a level down made it look like an implementation detail of linking.

The move also flattened one level of nesting and fixed a name: "lower" says how the code is written, "codegen" says what it produces.

`cfg.rs` came along because its only consumer is the lowering. Leaving it in `aot/` would have had the two directories referencing each other in both directions; now the dependency runs one way, `aot` → `codegen`.

## Why the directory is split this way

Until v1.2.0 all of it was a single `lower.rs` of 5,224 lines, a third of which was one `lower_instr` match. It was the largest file in the repo by a wide margin and the only place that ignored the "one concern per file" rule the rest of the tree follows.

The split was *purely mechanical*: every item moved verbatim, and the only edits were adding `pub(super)` where a call now crosses a module line. Nothing was rewritten, reordered, or optimized. That mattered, because it is what let the change be verified rather than reviewed — see "Building and testing".

`Lowerer` is defined in `mod.rs` and each file adds its own `impl` block, so a helper lives beside the operations that use it. The submodules are peers that call freely into one another, so `mod.rs` lifts their `pub(super)` items into the shared parent scope and each file opens with `use super::*`. That keeps every call site spelled exactly as it was written when it all lived in one file.

## What each file does

- **`mod.rs`** — the tagged-value constants, `struct Lowerer` and `FnCtx`, struct-default construction, the entry-block frame layout (register slots, handler `jmp_buf`s, and `Lowerer::entry_buf` for call-site scratch buffers), and the three entry points: `lower_program` (a whole program), `lower_chunk` (one chunk, used by tests), and `lower_body` (a single function body).
- **`cfg.rs`** — control-flow-graph reconstruction. `emit.rs` produces a flat `Vec<Instr>` with PC-relative jumps; LLVM needs basic blocks with explicit edges. This file computes block boundaries and edges and holds no LLVM state at all, so it is unit-testable in isolation.
- **`abi.rs`** — the tagged-value ABI: int/bool/float boxing and unboxing, pointer tagging, register slot load and store, global slots. Everything else is written in terms of these.
- **`arith.rs`** — integer, float, and bitwise arithmetic, plus the comparison family. Includes the dynamic paths (`any2`, `eq_any`, `cmp_any`) taken when a register's type is not statically known.
- **`strings.rs`** — string literals and interning, concatenation, ordering, and the primitive string methods.
- **`rc.rs`** — reference counting: `incref`, `decref`, `retain`, slot replacement, and scope exit. Its header carries the invariant every new `TAG_PTR` value has to satisfy; read it before adding one.
- **`exc.rs`** — exception frames: `throw`, the `setjmp` shim, and frame push/pop. Read the handler-stack gotcha in [`../aot/README.md`](../aot/README.md) before touching it.
- **`calls.rs`** — call resolution and emission. `resolve_user_calls` decides, per call site, which `CallKind` applies (direct, method, module, native, spawn, stream); the emission side covers indirect calls and function boxing.
- **`builtins.rs`** — builtin dispatch: native package calls, stdlib module calls, primitive value methods, and runtime-dispatched struct methods. The `chunk_*_supported` predicates that gate them live here too.
- **`llm.rs`** — prompt values and dereferences, including the `stream(?p)` lowering. The smallest file.
- **`instr.rs`** — the `lower_instr` match. A dispatcher: each arm either inlines a couple of lines or delegates to one of the files above.
- **`tests.rs`** — unit tests, which assert against emitted IR.

## Who uses it

*Depends on:* `bytecode/` for the instruction set, `vm::VmValue` for compile-time constants, `frontend::ast` for struct-default expressions, and `inkwell` for LLVM.

*Used by:* `aot/mod.rs` only, through `lower_program` and `LoweredProgram`.

## Gotchas

**Several of the gotchas that matter here are in the caller's README**, not this one: refcount ownership on borrowed value words, `jrt_require_kind` before untagging a receiver, the thread-wide handler stack, and the rule that any new opcode or builtin must be lowered here or the two engines quietly disagree. Read [`../aot/README.md`](../aot/README.md) before editing.

**A buffer a call site needs goes in the entry block, because the lowered code puts calls inside loops.** A call that marshals its arguments into memory needs somewhere to put them, and the obvious spelling — an `alloca` right where the call is emitted — is wrong. LLVM does not reclaim an `alloca` until the function returns, so a call inside a loop walks the stack down once per iteration until it hits the guard page. An FFI call in a `while` loop died at a fixed iteration count for exactly that reason, and the count scaled with `ulimit -s`, which is what named it stack exhaustion rather than a leak or an index overflow. The argv buffers for a native call, an indirect call, a `Spawn`, and a `Join` all had it; the `jmp_buf` for a `try` never did, and its comment in `lower_body` is where the rule was already written down. `Lowerer::entry_buf` is the one way to ask for such a buffer now, and it hands the same buffer to every site that wants the same purpose and length — safe because each site fills its buffer from register slots (plain loads, no call) and hands it straight to the call that consumes it, so no two are ever live at once. Two buffers a single callee reads and writes together, like `Join`'s futures and results, must ask under different names.

**"It gets dead-code-eliminated" is not a thing you may assume.** The `GetGlobal` arm materializing a native fn value said so in a comment, and used it to justify a `malloc` per evaluation: a reference that is immediately called devirtualizes to a direct native call, so surely the value it built is dead. It is not. `GetGlobal` stores the tagged word into the register-file alloca, and a word that is dead to Jade is still a live store to LLVM, so both allocations stayed in the loop body next to the call. Nothing freed them either — the `ObjKind::Fn` at offset 8 exists precisely so `is_collection` is false and `jrt_decref` returns early — so a compiled binary leaked 48 bytes per FFI call, without bound, while `jade run` leaked nothing. The register file defeats DCE for every value that reaches a slot, which is most of them; if a lowering is only affordable when something downstream deletes it, it is not affordable.

**A value with no identity and no mutable state should be a link-time constant, not an allocation.** A native fn value is a pure function of `(pkgid, fname)`, so there is now one `internal constant` box and env per binding and every evaluation hands out the same pointer. Two things make that legal, and both are worth checking before doing it again elsewhere: nothing ever writes to the object, because `jrt_incref` and `jrt_decref` are both gated on the kind and skip a fn box; and nothing can observe the sharing, because `==` on two native fn values raises on *both* engines, so pointer identity is not reachable from the language. `set_constant` and `align 8` are load-bearing rather than decorative — a write should fault instead of corrupting an object the whole program shares, and `TAG_PTR` lives in the low three bits that `untag_ptr` masks off.

**A `dlopen` handle cannot go in a static initializer, so the env holds the handle's address instead.** The obvious repair — keep the global but store the handle into it on each evaluation — is a data race the moment two tasks evaluate the same reference, benign-looking but undefined, and it invents an initialization-order rule for someone to break later. Pointing the env at `@native_pkg$<pkgid>` instead leaves nothing to initialize at all: the constant is correct before `main` runs, and `indirect_call` pays one extra load to reach through it. Prefer a second indirection over a write whenever the write would need to be ordered or repeated.

**Put a new helper in the file that owns its concern, not in `instr.rs`.** The reason the old file reached 5,000 lines is that `lower_instr` was the path of least resistance for every addition. An arm that grows past a few lines belongs in a topic file with a thin call from the match.

**`use super::*` makes every sibling's items look local.** Convenient, and the reason the split needed no call-site edits, but it also means a name collision between two submodules surfaces as a confusing ambiguity error rather than at its definition. Keep new `pub(super)` names distinct.

## Building and testing

```sh
cargo test codegen::
./src/scripts/backend-parity.sh
```

Because a change here alters generated code rather than observable behavior, tests alone are a weak signal. The strongest check available is to diff the IR itself:

```sh
for f in $(find examples -name '*.jde'); do
  ./target/debug/jade build "$f" --emit ir > "before/$(echo $f | tr / _).ll" 2>/dev/null
done
# ... make the change, rebuild, emit again into after/, then diff the two trees
```

For a refactor that is meant to be behavior-preserving, every file should come out byte-identical. That is how both the original split and the move out of `aot/` were verified: 88 of the 95 examples emit IR, all 88 were unchanged, and the parity gate stayed at 86 ok / 10 skipped / 0 failed.
