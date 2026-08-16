# `src/compiler/` — AST to typed IR to bytecode

## What this subtree is

The middle of the pipeline. It takes the untyped `Program` the frontend produced, works out the type of every expression, and emits a `Chunk` of bytecode.

```
Program (AST) → type_infer → TProgram (TIR) → emit → CompiledProgram (bytecode)
```

It ends at the bytecode. What runs that bytecode — the interpreter in `src/vm/` or the LLVM backend in `src/aot/` — is not this module's concern. Both live alongside it rather than inside it, which is deliberate: neither backend is privileged.

## Why it was built

Type inference exists so the instruction set can be *monomorphic*. Because the compiler already knows an addition is integer addition, it can emit `AddInt` rather than a generic `Add`. That is what lets the VM add two `i64`s without a tag dispatch and lets the AOT backend emit a bare LLVM `add`. Almost every performance property of both engines traces back to this pass.

`JadeType::Unknown` is a first-class type meaning "could not be determined statically." It propagates through operations without raising, so the checker is conservative and never produces a false positive — a program it cannot fully understand still runs, with the VM dispatching on runtime tags.

## What each file does

- **`type_infer.rs`** — the inference pass. Maintains a `TypeContext`: a scope stack mirroring the evaluator's scoping, plus flat maps for struct definitions, interfaces, `extend` methods, and primitive method types. Built-in packages register their type information here when imported (`Package::register_types`). **All type errors belong in this file** — by the time bytecode runs, types are settled. It also *resolves* one piece of syntax rather than only checking it: `infer_pipe` decides whether a `|>` stage is a type, a Grammar, or a function, and lowers the pipe to a `Call` or folds it into a `PromptDeref`. Nothing downstream sees an `Expr::Pipe`.

  **How lenient it is about a name it cannot find is a real decision**, and getting it wrong is quiet. An import can be what defines a name, so `opaque_imports` exists to turn unknown identifiers into `Unknown` rather than errors — but only imports that genuinely are opaque should set it. A stdlib `use` is not: `register_types` runs in this same pass and defines everything the package contributes. When any `use` at all set the flag, an undefined name in a file with one stdlib import went through, reached the AOT backend as a read of a global nothing binds, and produced a program that built cleanly and did nothing. That was `exit()` until v1.3.20. What stays lenient is a `use` of a user module or a `[lib]`, whose names only exist once the importer merges the modules, and every `from … use`, which binds bare names this pass never sees. Both are re-checked in `codegen` after inlining.
- **`tir.rs`** — the typed IR: `JadeType`, `TExpr`, `TStmt`, `TProgram`. Serde-serializable, because the cache stores TIR alongside the AST.
- **`emit.rs`** — TIR to `CompiledProgram`: the top-level `Chunk`, struct definitions and decorators, compiled `extend` methods, and `@route` configuration.
- **`taskcheck.rs`** — rejects shared mutation across task boundaries. Jade tasks run on real OS threads over one shared heap, so a task that writes a global or mutates a caller's struct is a data race. Rather than locking every collection or deep-copying task arguments, Jade refuses to compile those programs. Read the file header before touching it; the reasoning is laid out there in full.

  One case it deliberately does *not* cover lives in `type_infer.rs` instead: a `handle` passed into a spawned function. This pass finds races by watching `SetIndex`/`SetField`/mutating methods, and a handle has none of those — all the mutation happens inside the C library, where Jade sees a call and nothing more. So the rule is enforced where the type is known rather than where the opcodes are, and it refuses outright, because Jade cannot tell a thread-safe library from an unsafe one.
- **`escape.rs`** — type-aware escape analysis for arena allocation. Decides which array literals can live in the per-frame bump arena instead of the refcounted heap. Deliberately narrow: it marks a literal eligible only when it can *prove* non-escape, because being wrong here is a use-after-free.
- **`gbnf.rs`** — builds GBNF sampling grammars for typed prompt dereferences (`?p |> int`). The pattern-wrapping implementation itself lives in `jade_runtime::grammarf` so the VM and the AOT backend cannot wrap grammars differently.
- **`mod.rs`**, **`tests.rs`** — module declarations and the pass tests.

## Who uses it

*Depends on:* `frontend/` for the AST and errors, `builtins/` for the type information of built-in packages, `bytecode/` for the instruction set it emits into, and `jade_runtime` for the shared grammar wrapper.

*Used by:* `vm/` and `aot/` both consume `CompiledProgram`; `build/` and `cli/check.rs` call `type_infer` directly; `cache/` stores the `TProgram`.

## Mixed arrays, and the sum type that is still missing

An array literal may hold two types as of v1.1.32; before that `[1, "two"]` was a type error. The check was a frontend gate over a runtime that never needed it — `push` built the same array without complaint, and the element type simply widens to `Unknown`, exactly as a dict's value type already did.

Removing it cost about ten lines and surfaced three engine divergences the restriction had been hiding, because a mixed array was the only way to reach them: `arr.contains(x)` answered in the VM and raised compiled, a cross-kind comparison gave a misleading message compiled, and the VM interpolated a Rust enum name into arithmetic errors. The lesson worth keeping is that a restriction which makes a class of program unwritable also makes that class of bug unreachable.

What is still missing is a **named sum type** — `type Frame = Token | Done | Error`, with `[Frame]` a homogeneous array of it. Mixed arrays only make the heterogeneous list *writable*; the element type is `Unknown`, so every check happens at run time in the decoder. A sum type would let the compiler reject a frame the protocol does not declare, and would match how the Rust half already spells it. That is a real feature — declaration syntax, inference, exhaustiveness, lowering in both engines, and an FFI representation — and worth doing when frames stop being the only caller.

## Gotchas

`emit` is what rejects shared mutation across tasks, because the mutation opcodes (`SetGlobal`, `SetIndex`, `SetField`) only exist in bytecode — the AST's assignment expression cannot tell rebinding a local from writing through a reference. That is why `cli/check.rs` runs `emit` as part of `jade check`: it keeps `check` an honest predictor of whether `run` and `build` will succeed.

`break` and `continue` are emitted as a plain `Jump` the enclosing loop patches once it knows where its exit is, so neither engine needed a new opcode — the AOT backend lowers them without knowing they exist. Two details are load-bearing. A `continue` lands at the *bottom* of the body rather than the top, so it still runs the `for` loop's index increment and the per-iteration `ArenaReset`; landing at the top would hang. And both first emit a `PopHandler` for every `try` they jump out of, tracked by `Emitter::handler_depth` — a handler frame left installed points into code the loop has already left, and the next exception anywhere in the function would land there.

Changing the shape of any TIR type means bumping `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`.

## Building and testing

```sh
cargo test compiler::
```
