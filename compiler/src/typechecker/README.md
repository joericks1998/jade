# `src/typechecker/`: AST to typed IR to static types

## What this subtree is

This is the middle of the pipeline. It takes the untyped `Program` the frontend produced, works out the type of every expression in each file, and, once imports are merged, gives every value of the whole program one static type.

```
Program (AST) → type_infer → TProgram (TIR), per file
merged TProgram → infer → Typed (types and call targets per instance)
```

It ends at the checker's answer. What compiles that answer, [`src/mir/`](../mir/README.md) and [`src/backend/`](../backend/README.md), lives alongside this module rather than inside it. Until phase 6 of `design/v2-static-values.md` this directory also emitted bytecode for v1's backend; that emitter, its escape analysis and its grammar builder went with the bytecode, and the task-safety check moved to `src/mir/taskcheck.rs`.

It was `src/compiler/` until 2.0.1, when the crate itself became `compiler/`. The name also keeps it apart from [`src/infer/`](../infer/README.md), which runs prompts: `typechecker/infer/` infers types, and `infer/` runs a model.

## Why it was built

`type_infer` works one file at a time, before imports are merged, so it is the pass that can resolve syntax and report the errors one file shows: an unknown name, a pipe stage that names nothing. `infer` sees the whole program and decides every type for good, which is what lets the backend put an int in a register and a struct in a fixed layout.

`JadeType::Unknown` is still a type in TIR, meaning "not known from this file alone". `infer` does not accept it: by the time the whole program is checked, every value has one concrete type or the build stops.

## What each file does

- *`type_infer.rs`* is the inference pass. It keeps a `TypeContext`, which holds a scope stack mirroring the language's scoping, plus flat maps for struct definitions, their parents and ancestry, `extend` methods, and primitive method types. `resolve_inheritance` runs here, folding each parent's fields into its children so nothing downstream has to know inheritance exists. A built-in package registers its type information here when imported, through `Package::register_types`. *Per-file type errors belong in this file*; errors that need the whole program, such as a function called with the wrong types, belong in `infer/`. Since 2.0.1 flattening inherits an ancestor reached by two paths once, by tracking which class declared each field, and two parents supplying different versions of one method are an error unless the class defines that method itself.

  It also *resolves* one piece of syntax rather than only checking it. `infer_pipe` decides whether a `|>` stage is a type, a Grammar, or a function, then lowers the pipe to a `Call` or folds it into a `PromptDeref`. Nothing downstream ever sees an `Expr::Pipe`.

  *How lenient this pass is about a name it cannot find is a real decision*, and getting it wrong fails quietly. An import can be what defines a name, so `opaque_imports` turns unknown identifiers into `Unknown` rather than errors. Only imports that genuinely are opaque should set that flag.

  A stdlib `use` is not opaque, because `register_types` runs in this same pass and defines everything the package contributes. When any `use` at all set the flag, an undefined name in a file holding one stdlib import went straight through. It reached the AOT backend as a read of a global that nothing binds, and produced a program that built cleanly and did nothing. `exit()` behaved that way until v1.3.20.

  Two things stay lenient. A `use` of a user module or a `[lib]`, whose names only exist once the importer merges the modules. And every `from … use`, which binds bare names this pass never sees. `infer` checks both after the merge.
- *`infer/`* is Jade 2.0's whole-program checker, with its own README. It runs after imports are merged, gives every value one concrete type by checking each function once per set of argument types, and refuses the dynamic patterns v2 drops. Its answer is the input [`src/mir/`](../mir/README.md) lowers.
- *`tir.rs`* holds the typed IR: `JadeType`, `TExpr`, `TStmt`, and `TProgram`. All are serde-serializable, because the cache stores TIR alongside the AST.
- *`mod.rs`* and *`tests.rs`* hold the module declarations and the pass tests.

## Who uses it

*Depends on:* `frontend/` for the AST and errors, and `builtins/` for the type information of built-in packages.

*Used by:* `aot/`, which runs `infer` over the merged program, and `mir/`, which lowers its answer. `cli/build.rs` and `cli/check.rs` call `type_infer` directly, and `cache/` stores the `TProgram`.

## Gotchas

A struct literal's TIR field list is complete *except* under a `...base`, where it holds only the fields the literal named. Filling a field's declared default there would overwrite the value being copied, so the fill is skipped and MIR lowering copies the rest from the base. The order they apply is fixed: a named field beats the base, and the base beats a default.

Changing the shape of any TIR type means bumping `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`.

## Building and testing

```sh
cargo test compiler::
```
