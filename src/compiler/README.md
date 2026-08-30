# `src/compiler/`: AST to typed IR to bytecode

## What this subtree is

This is the middle of the pipeline. It takes the untyped `Program` the frontend produced, works out the type of every expression, and emits a `Chunk` of bytecode.

```
Program (AST) → type_infer → TProgram (TIR) → emit → CompiledProgram (bytecode)
```

It ends at the bytecode. What runs that bytecode is not this module's concern, whether that is the interpreter in `src/vm/` or the LLVM backend in `src/aot/`. Both live alongside it rather than inside it, and that is deliberate. Neither backend is privileged.

## Why it was built

Type inference exists so the instruction set can be *monomorphic*. Because the compiler already knows an addition is integer addition, it emits `AddInt` rather than a generic `Add`. That is what lets the VM add two `i64` values with no tag dispatch, and what lets the AOT backend emit a bare LLVM `add`. Almost every performance property of both engines traces back to this pass.

`JadeType::Unknown` is a real type, and it means "could not be determined ahead of time". It spreads through operations without raising, which keeps the checker conservative and stops it producing false positives. A program the checker cannot fully understand still runs, with the VM dispatching on runtime tags instead.

## What each file does

- *`type_infer.rs`* is the inference pass. It keeps a `TypeContext`, which holds a scope stack mirroring the evaluator's scoping, plus flat maps for struct definitions, their parents and ancestry, `extend` methods, and primitive method types. `resolve_inheritance` runs here, folding each parent's fields into its children so nothing downstream has to know inheritance exists. A built-in package registers its type information here when imported, through `Package::register_types`. *All type errors belong in this file*, because by the time bytecode runs, types are settled.

  It also *resolves* one piece of syntax rather than only checking it. `infer_pipe` decides whether a `|>` stage is a type, a Grammar, or a function, then lowers the pipe to a `Call` or folds it into a `PromptDeref`. Nothing downstream ever sees an `Expr::Pipe`.

  *How lenient this pass is about a name it cannot find is a real decision*, and getting it wrong fails quietly. An import can be what defines a name, so `opaque_imports` turns unknown identifiers into `Unknown` rather than errors. Only imports that genuinely are opaque should set that flag.

  A stdlib `use` is not opaque, because `register_types` runs in this same pass and defines everything the package contributes. When any `use` at all set the flag, an undefined name in a file holding one stdlib import went straight through. It reached the AOT backend as a read of a global that nothing binds, and produced a program that built cleanly and did nothing. `exit()` behaved that way until v1.3.20.

  Two things stay lenient. A `use` of a user module or a `[lib]`, whose names only exist once the importer merges the modules. And every `from … use`, which binds bare names this pass never sees. `codegen` re-checks both after inlining.
- *`tir.rs`* holds the typed IR: `JadeType`, `TExpr`, `TStmt`, and `TProgram`. All are serde-serializable, because the cache stores TIR alongside the AST.
- *`emit.rs`* turns TIR into a `CompiledProgram`. That covers the top-level `Chunk`, struct definitions, each struct's flattened ancestry, and compiled `extend` methods, a parent's folded into each child.
- *`taskcheck.rs`* rejects shared mutation across task boundaries. Jade tasks run on real operating system threads over one shared heap, so a task that writes a global or mutates a caller's struct is a data race. Rather than locking every collection or deep-copying task arguments, Jade refuses to compile those programs. Read the file header before touching it, because the reasoning is laid out there in full.

  It watches four opcodes, and two of them were added in v1.3.27. `SetIndex` was in the list from the start but read the wrong taint set: the emitter hands it the *slot* of the binding being written, and the arm was checking the register set, so it matched nothing and `async fn f(arr) { arr[0] = 9 }` compiled clean beside a correctly rejected `arr.push(9)`. `SetIndexGlobal` had no arm at all. Both were live data races, and both got much easier to reach once a `bytes` buffer became something you write into.

  The pass also knows about three functions by name. `bytes.zeros`, `bytes.from_ints`, and `bytes.concat` return storage nothing else points at, so taint stops at the call the way it stops at `MakeArray`. Without that, a task that allocates its own buffer and writes into it is rejected, which is the opposite of true. The check is that the receiver came from `GetGlobal("bytes")` and that the program does not bind a global of that name itself, which is the same test `codegen::calls` uses to tell a module call from a value method.

  It also watches the *spawner*, since v1.4.3. Everything above asks what a task does; a race needs only one side, and assigning a global a running task reads — or mutating a collection it was handed — is the same race from the other end. The window opens at the spawn and closes at the await, so the identical writes afterwards are still fine. Following registers was not enough to see it: a global is re-read into a fresh register every time it is used, so `read(s)` and `s.push(3)` never share one, and a future goes through a local before the await. The scan tracks the name and the local as well.

  Three things used to launder taint straight past the pass, all closed in v1.4.3 and all the same shape — a call whose callee the pass could not name. A closure handed to a task (`async fn run(f) { f() }` calls a parameter, which resolves to nothing, so the spawn site checks its arguments as well as its callee). A function value read back from a global (a closure binds a global named for the *variable*, so looking it up by definition name found nothing). And a user `extend` method (a method reaches its receiver as `self` rather than as an argument, so `b.grow()` had an empty argument list and looked harmless).

  One case this pass deliberately does *not* cover lives in `type_infer.rs` instead: a `handle` passed into a spawned function. This pass finds races by watching `SetIndex`, `SetField`, and mutating methods, and a handle triggers none of them. All the mutation happens inside the C library, where Jade sees a call and nothing more. So the rule is enforced where the type is known rather than where the opcodes are, and it refuses outright, because Jade cannot tell a thread-safe library from an unsafe one.
- *`escape.rs`* runs type-aware escape analysis for arena allocation. It decides which array literals can live in the per-frame bump arena instead of the reference-counted heap. It is deliberately narrow, and marks a literal eligible only when it can *prove* the value does not escape, because being wrong here means a use-after-free.
- *`gbnf.rs`* builds GBNF sampling grammars for a typed prompt dereference such as `?p |> int`. The pattern-wrapping code itself lives in `jade_runtime::grammarf`, so the VM and the AOT backend cannot wrap grammars differently.
- *`mod.rs`* and *`tests.rs`* hold the module declarations and the pass tests.

## Who uses it

*Depends on:* `frontend/` for the AST and errors, `builtins/` for the type information of built-in packages, `bytecode/` for the instruction set it emits into, and `jade_runtime` for the shared grammar wrapper.

*Used by:* `vm/` and `aot/`, which both consume a `CompiledProgram`. `build/` and `cli/check.rs` call `type_infer` directly, and `cache/` stores the `TProgram`.

## Mixed arrays, and the sum type that is still missing

Since v1.1.32, an array literal may hold two types. Before that, `[1, "two"]` was a type error. The check was a frontend gate over a runtime that never needed it. `push` built the same array without complaint, and the element type simply widens to `Unknown`, exactly as a dict's value type already did.

Removing the check cost about ten lines, and it surfaced three places where the two engines had been disagreeing. The restriction had hidden all three, because a mixed array was the only way to reach them. `arr.contains(x)` answered in the VM and raised when compiled. A cross-kind comparison gave a misleading message when compiled. And the VM interpolated a Rust enum name into arithmetic errors.

The lesson worth keeping: a restriction that makes a class of program unwritable also makes that class of bug unreachable.

What is still missing is a *named sum type*, written something like `type Frame = Token | Done | Error`, with `[Frame]` as a homogeneous array of it. Mixed arrays only make the heterogeneous list *writable*. The element type is `Unknown`, so every check happens at run time in the decoder.

A sum type would let the compiler reject a frame the protocol does not declare, and it would match how the Rust half already spells the same idea. Building it is a real feature, covering declaration syntax, inference, exhaustiveness, lowering in both engines, and an FFI representation. It is worth doing once frames stop being the only caller.

## Gotchas

`emit` is what rejects shared mutation across tasks, because the mutation opcodes `SetGlobal`, `SetIndex`, and `SetField` only exist in bytecode. The AST's assignment expression cannot tell rebinding a local from writing through a reference. That is why `cli/check.rs` runs `emit` as part of `jade check`. Running it keeps `check` an honest predictor of whether `run` and `build` will succeed.

`break` and `continue` are emitted as a plain `Jump`, which the enclosing loop patches once it knows where its exit is. Neither engine needed a new opcode, and the AOT backend lowers them without knowing they exist. Two details carry weight here.

A `continue` lands at the *bottom* of the body rather than the top, so it still runs the `for` loop's index increment and the per-iteration `ArenaReset`. Landing at the top would hang.

Both also emit a `PopHandler` first, for every `try` they jump out of, tracked by `Emitter::handler_depth`. A handler frame left installed points into code the loop has already left, and the next exception anywhere in the function would land there.

A struct literal's TIR field list is complete *except* under a `...base`, where it holds only the fields the literal named. Filling a field's declared default there would overwrite the value being copied, so the fill is skipped and the engines resolve the rest when the literal runs. The order they apply is fixed: a named field beats the base, and the base beats a default.

That skip is also what first asked a backend to build a default at run time. Every default used to be materialized here, so neither engine ever had to; the compiled side turned out to handle scalars only, and `let tags = []` came out as a missing field. Both build the collection now, and a default shape one engine knows and the other does not is a difference between the two.

Changing the shape of any TIR type means bumping `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`.

## Building and testing

```sh
cargo test compiler::
```
