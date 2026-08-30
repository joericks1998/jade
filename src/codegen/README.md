# `src/codegen/`: bytecode to LLVM IR

## What this subtree is

This directory does the translation and nothing else: a bytecode `Chunk` goes in, and an LLVM module comes out. `cfg.rs` turns the flat instruction stream into basic blocks. The rest of the directory lowers one opcode at a time, calling into the `jrt_*` C-ABI surface of `jade-runtime` for anything needing the heap, collections, strings, tasks, or inference.

Nothing here writes a file, runs a linker, or knows what a project is. [`src/aot/`](../aot/README.md) does all of that. It resolves imports, calls `lower_program`, wraps the result in a `main()`, and drives `cc`.

## Why it sits beside `aot` rather than inside it

This used to be `src/aot/lower/`, one level down inside the backend that calls it. That put the half of `jade build` about *the language* underneath the half about *producing a file*, and the two are not related that way.

Lowering is where the VM and the compiled path have to agree on what an opcode means. It is a peer of `src/vm/`, in the same way `src/vm/` is a peer of the AOT driver. Sitting a level down made it look like an implementation detail of linking.

The move also removed one level of nesting and fixed a name. "Lower" describes how the code is written, while "codegen" describes what it produces.

`cfg.rs` came along because the lowering is its only consumer. Leaving it in `aot/` would have had the two directories referencing each other in both directions. Now the dependency runs one way, from `aot` to `codegen`.

## Why the directory is split this way

Until v1.2.0, all of this was a single `lower.rs` of 5,224 lines, a third of which was one `lower_instr` match. It was the largest file in the repo by a wide margin, and the only place ignoring the "one concern per file" rule the rest of the tree follows.

The split was *purely mechanical*. Every item moved verbatim, and the only edits added `pub(super)` where a call now crosses a module line. Nothing was rewritten, reordered, or optimized. That mattered, because it is what let the change be verified rather than reviewed. See "Building and testing" below.

`Lowerer` is defined in `mod.rs`, and each file adds its own `impl` block, so a helper sits beside the operations that use it. The submodules are peers that call freely into one another, so `mod.rs` lifts their `pub(super)` items into the shared parent scope and each file opens with `use super::*`. That keeps every call site spelled exactly as it was when all of this lived in one file.

## What each file does

- *`mod.rs`* holds the tagged-value constants, `struct Lowerer` and `FnCtx`, struct-default construction, and the entry-block frame layout. The frame layout covers register slots, handler `jmp_buf` values, and `Lowerer::entry_buf` for call-site scratch buffers. It also holds the three entry points: `lower_program` for a whole program, `lower_chunk` for one chunk, used by tests, and `lower_body` for a single function body.
- *`cfg.rs`* rebuilds the control-flow graph. `emit.rs` produces a flat `Vec<Instr>` with jumps relative to the program counter, while LLVM needs basic blocks with explicit edges. This file computes the block boundaries and edges. It holds no LLVM state at all, so you can unit-test it on its own.
- *`abi.rs`* holds the tagged-value ABI: boxing and unboxing for ints, bools, and floats, pointer tagging, register slot loads and stores, and global slots. Everything else is written in terms of these.
- *`arith.rs`* holds integer, float, and bitwise arithmetic, plus the comparison family. It includes the dynamic paths `any2`, `eq_any`, and `cmp_any`, taken when a register's type is not known ahead of time.
- *`strings.rs`* holds string literals and interning, concatenation, ordering, and the primitive string methods.
- *`rc.rs`* holds reference counting: `incref`, `decref`, `retain`, slot replacement, and scope exit. Its header states the rule every new `TAG_PTR` value has to satisfy, so read it before adding one.
- *`exc.rs`* holds the exception frames: `throw`, the `setjmp` shim, and frame push and pop. Read the handler-stack gotcha in [`../aot/README.md`](../aot/README.md) before touching it.
- *`calls.rs`* handles call resolution and emission. `resolve_user_calls` decides, for each call site, which `CallKind` applies: direct, method, module, native, spawn, or stream. The emission side covers indirect calls and function boxing.
- *`builtins.rs`* handles builtin dispatch: native package calls, stdlib module calls, primitive value methods, and struct methods dispatched at run time. The `chunk_*_supported` predicates that gate them live here too.

  *A name those predicates answer `false` for is a hard build error*, so the set they accept has to match what `jade run` accepts. For a while it did not. `string.upper(s)` ran under the VM and refused to build until v1.3.21, while the identical `s.upper()` compiled fine.

  `as_receiver_first` closes that gap by lowering the package spelling as the method. It does so only for names the package table actually lists, and only for `std/string` and `std/dict`, where the two spellings are the same function. `std/array` is deliberately excluded, because `array.sort(a)` returns a copy while `a.sort()` mutates. Those get their own lowering over `jrt_coll_array_sorted`. Read `package_fn_is_the_method` before widening it.

  It also holds `check_globals_bound`, the one whole-program check this directory runs before emitting anything. Reading a global that nothing binds gives the lowering nothing to decline: `global_slot` creates the cell, initialized to nil, and the program builds. So an undefined name produced a binary that ran and then died inside the runtime with no message.

  Type inference catches most of these. What reaches here is the case it has to stay lenient about, which is a file importing a user module. By this point every import is inlined, so the program's own bindings plus the runtime's are all there is, and the question is finally answerable. Read `check_globals_bound` before adding a global that nothing writes with `SetGlobal`. Type names are already exempt, because they live in the side tables rather than in the instruction stream.
- *`trampoline.rs`* emits `jf_ind_<uid>`, the second entry point every function gets so that a *value* can be called. A call site that jumps at a value does not know which function it holds, so it cannot know the parameter count or the defaults; the entry does, and it checks the one and fills the other before calling the body. Its C counterpart is `jrt_call_value`, which is what knows how to enter a plain function, a bound method and a native binding. A direct call skips all of it and still calls `jf_<uid>`.
  `emit_method_fallback` is the other half of method dispatch. A call site resolves `obj.m(...)` by name and arity, which bytecode can answer and which says nothing about the receiver's type, so the emitted code guards on the type and branches: another struct dispatches on its runtime type, and a receiver that is not a struct at all takes the primitive method of that name — the case a struct declaring `contains` used to steal from `[1, 2].contains(x)`.

- *`llm.rs`* holds prompt values and dereferences, including the `stream(?p)` lowering. It is the smallest file here.
- *`instr.rs`* holds the `lower_instr` match. It is a dispatcher, so each arm either inlines a couple of lines or hands off to one of the files above.
- *`tests.rs`* holds the unit tests, which assert against emitted IR.

## Who uses it

*Depends on:* `bytecode/` for the instruction set, `vm::VmValue` for compile-time constants, `frontend::ast` for struct-default expressions, and `inkwell` for LLVM.

*Used by:* `aot/mod.rs` only, through `lower_program` and `LoweredProgram`.

## Gotchas

*Several of the gotchas that matter here live in the caller's README*, not this one. Those are reference-count ownership on borrowed value words, calling `jrt_require_kind` before untagging a receiver, the thread-wide handler stack, and the rule that any new opcode or builtin must be lowered here or the two engines quietly disagree. Read [`../aot/README.md`](../aot/README.md) before editing.

*A buffer a call site needs goes in the entry block, because the lowered code puts calls inside loops.* A call that marshals its arguments into memory needs somewhere to put them. The obvious spelling, an `alloca` right where the call is emitted, is wrong. LLVM does not reclaim an `alloca` until the function returns, so a call inside a loop walks the stack down once per iteration until it reaches the guard page.

An FFI call in a `while` loop died at a fixed iteration count for exactly that reason. The count scaled with `ulimit -s`, which is what identified it as stack exhaustion rather than a leak or an index overflow. The argument buffers for a native call, an indirect call, a `Spawn`, and a `Join` all had the bug. The `jmp_buf` for a `try` never did, and its comment in `lower_body` is where the rule was already written down.

`Lowerer::entry_buf` is now the only way to ask for such a buffer. It hands the same buffer to every site wanting the same purpose and length. That is safe because each site fills its buffer from register slots, which are plain loads with no call in between, then hands it straight to the call that consumes it. So no two are ever live at once. Two buffers a single callee reads and writes together, such as the futures and results for `Join`, must ask under different names.

*Never assume "it gets dead-code-eliminated".* The `GetGlobal` arm that materializes a native fn value said exactly that in a comment, and used it to justify a `malloc` on every evaluation. The reasoning was that a reference which is immediately called devirtualizes to a direct native call, so the value it built must be dead.

It is not dead. `GetGlobal` stores the tagged word into the register-file `alloca`, and a word that is dead to Jade is still a live store to LLVM. So both allocations stayed in the loop body next to the call. Nothing freed them either, because the `ObjKind::Fn` at offset 8 exists precisely so `is_collection` is false and `jrt_decref` returns early. A compiled binary therefore leaked 48 bytes per FFI call, without bound, while `jade run` leaked nothing.

The register file defeats dead-code elimination for every value that reaches a slot, which is most of them. If a lowering is only affordable when something downstream deletes it, it is not affordable.

*A value with no identity and no mutable state should be a link-time constant, not an allocation.* A native fn value is a pure function of its `(pkgid, fname)` pair, so there is now one `internal constant` box and environment per binding, and every evaluation hands out the same pointer.

Two things make that legal, and both are worth checking before doing the same thing elsewhere. Nothing ever writes to the object, because `jrt_incref` and `jrt_decref` are both gated on the kind and skip a fn box. And nothing can observe the sharing, because `==` on two native fn values raises on *both* engines, so pointer identity is not reachable from the language.

`set_constant` and `align 8` are load-bearing rather than decorative. A write should fault rather than corrupt an object the whole program shares, and `TAG_PTR` lives in the low three bits that `untag_ptr` masks off.

*A `dlopen` handle cannot go in a static initializer, so the environment holds the handle's address instead.* The obvious repair is to keep the global and store the handle into it on each evaluation. That is a data race the moment two tasks evaluate the same reference. It looks harmless and is undefined, and it invents an initialization-order rule for someone to break later.

Pointing the environment at `@native_pkg$<pkgid>` instead leaves nothing to initialize. The constant is correct before `main` runs, and `indirect_call` pays one extra load to reach through it. Prefer a second indirection over a write whenever the write would have to be ordered or repeated.

*A call site cannot know a callee's arity, so it must not try.* Everything callable — a compiled `fn`, a bound method, a native package binding, a core builtin — is entered through one signature, `int64_t entry(int64_t argc, const int64_t* argv)`. `indirect_call` packs its arguments into a buffer and hands the whole thing to `jrt_call_value`; `array.map`, `array.filter`, runtime-dispatched struct methods, and callbacks the C runtime invokes all go the same way.

It used to build a fixed-arity call out of the arguments the site happened to have and jump straight at the body. That is only correct when the site knows the callee, and an indirect call is precisely the case where it does not. `f(1, 2)` through a value dropped an argument, `f()` read the missing one out of an uninitialised register, and a default parameter was never filled at all. All three ran correctly under `jade run`.

The consequence for anything new: a value that can be called needs an entry with that signature and an `ObjKind` byte at offset 8, and it is reached through `jrt_call_value` rather than by reading a function pointer out of its box. The byte *above* the ObjKind says which sort of callable it is, which is how a renderer prints `<builtin len>` rather than `<object>`.

*A function containing a `try` must keep its register slots in memory.* A caught raise gets back into the handler with `longjmp`, and `longjmp` restores the callee-saved registers to what they held at the matching `setjmp`. Any slot LLVM had promoted out of its `alloca` and into one of those registers therefore reverts, silently undoing every write the try body made. `fn f() { let a = 0; try { a = 1; raise "x" } catch e {}; return a }` returned 1 interpreted and 0 compiled.

It is worse than a wrong number when the slot holds a heap value, because the reverted word names a value the slot no longer owns, and releasing it a second time is a double free. Three separate crashes traced back to it, including one an ordinary `for` loop with a `try` inside reached.

`Lowerer::volatile_slots` is true exactly for a function that contains a `try`, and every slot access goes through `slot_load` / `slot_store`, which mark the access volatile when it is set. A volatile access cannot be promoted, so the slot stays where `longjmp` cannot touch it. This is the same rule C states as "a local modified between `setjmp` and `longjmp` must be `volatile`". The `returns_twice` attribute on the `setjmp` declaration constrains control flow only and does not stop the promotion; both halves are needed. Only functions that can be re-entered pay for it, and a `try` in a loop measured the same before and after, because the `setjmp` dominates.

*Check the tag before untagging on the strength of a static type.* Inference is not always precise enough to be trusted with a dereference. A function whose branches return different types takes the first branch's type, and an `Unknown` value pushed into a typed container keeps the container's element type. When the static type is then wrong, the compiled binary reads an int as a `char*` or as a pointer to a double and dies, where the interpreter checks the value it actually has and raises.

So a lowering that turns a word into a pointer emits a guard first: `jrt_require_str_val` before a concatenation, `jrt_require_float_val` before an unbox, `jrt_require_dict_key` before a key becomes a `char*`, `jrt_require_callable` before an indirect call or a `map`/`filter` callback, and `jrt_require_kind` before a primitive method touches its receiver. Each raises the wording the VM raises. `{true: "y"}`, `let f = 5; f(1)`, and `[1].decode()` were all segfaults before their guard existed.

*Put a new helper in the file that owns its concern, not in `instr.rs`.* The old file reached 5,000 lines because `lower_instr` was the path of least resistance for every addition. An arm that grows past a few lines belongs in a topic file, with a thin call from the match.

*`use super::*` makes every sibling's items look local.* That is convenient, and it is why the split needed no call-site edits. It also means a name collision between two submodules shows up as a confusing ambiguity error rather than at the definition. Keep new `pub(super)` names distinct.

## Building and testing

```sh
cargo test codegen::
./src/scripts/backend-parity.sh
```

A change here alters generated code rather than observable behavior, so tests alone are a weak signal. The strongest check available is to diff the IR itself:

```sh
for f in $(find examples -name '*.jde'); do
  ./target/debug/jade build "$f" --emit ir > "before/$(echo $f | tr / _).ll" 2>/dev/null
done
# ... make the change, rebuild, emit again into after/, then diff the two trees
```

For a refactor meant to preserve behavior, every file should come out byte-identical. That is how both the original split and the move out of `aot/` were verified. 88 of the 95 examples emit IR, all 88 were unchanged, and the parity gate stayed at 86 ok, 10 skipped, 0 failed.
