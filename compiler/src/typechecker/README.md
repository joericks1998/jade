# `src/typechecker/`: the parser's tree to static types

## What this subtree is

This is the middle of the pipeline. It takes the `Program` the frontend produced, rewrites each file's tree on its own, and, once imports are merged, gives every value of the whole program one static type.

```
Program (AST) → resolve → Program (AST, rewritten), per file
merged Program → infer → Typed (types and call targets per instance)
```

There is one tree. The per-file pass rewrites a few shapes in place and gives no node a type; the whole-program checker keeps each value's type in its own table, keyed by the address of the node, and MIR reads that table. Until 2.0.6 the per-file pass built a second tree, TIR, that gave every node a type, and nothing after it read those types; part 1 of `design/v2-simplify.md` explains its removal.

It ends at the checker's answer. What compiles that answer, [`src/mir/`](../mir/README.md) and [`src/backend/`](../backend/README.md), lives alongside this module rather than inside it. Until phase 6 of `design/v2-static-values.md` this directory also emitted bytecode for v1's backend; that emitter, its escape analysis and its grammar builder went with the bytecode, and the task-safety check moved to `src/mir/taskcheck.rs`.

It was `src/compiler/` until 2.0.1, when the crate itself became `compiler/`. The name also keeps it apart from [`src/infer/`](../infer/README.md), which runs prompts: `typechecker/infer/` infers types, and `infer/` runs a model.

## Why it was built

`resolve` works one file at a time, before imports are merged, so it is the pass that can rewrite syntax that depends on what a name is, and report the errors one file shows: an unknown name, a pipe stage that cannot work. `infer` sees the whole program and decides every type for good, which is what lets the backend put an int in a register and a struct in a fixed layout. Every type error is `infer`'s to report.

## What each file does

- *`resolve.rs`* is the per-file pass. It keeps a `Names` table, a scope stack of what each name is, a `NameKind`: a function, an async function, a generator, a class, a grammar, a prompt, a std package, or some other value. The pipe and dereference rewrites need that and nothing more. It also keeps flat maps for each class's fields, parents, ancestry, member decorators and method names. A std package's name is bound when a `use` names it, from `Package::global_name`, and the globals every program has are bound by `builtins::register_core_names`.

  `resolve_inheritance` runs here, folding each parent's fields into its children so nothing downstream has to know inheritance exists. It inherits an ancestor reached by two paths once, by tracking which class declared each field, and two parents supplying different versions of one method are an error unless the class defines that method itself.

  It *rewrites* three things. `resolve_pipe` flattens the parser's `RawPipe` chain into one `ExprKind::Pipe`: its source, its stages as one expression over a name for the piece in hand, `g(f(piece), a)`, and the stage arguments worked out once. It refuses a converter, type function, class or grammar piped straight after `?p`, naming the call to write instead, and a stage that is plainly not a function: a literal, or a name bound to a prompt or a grammar. A converter, class or grammar called around a whole dereference, `toInt(?p)`, folds into the `PromptDeref` as what the model is asked for. And a class literal of a class this file defines is put in definition order, with its prompt fields marked and, with no `...base`, its defaults filled in. Nothing downstream ever sees a `RawPipe`; a stage that does reports an internal error. See `design/v2-pipes.md`.

  *Per-file errors belong in this file*: undefined names, unknown classes, fields and parents, inheritance cycles and clashes, `yield` beside a returned value, duplicate and missing fields, quoted imports and import aliases, unknown package functions, and the pipe stages above. An error that needs a value's type, such as `1 + "a"` or `if 1`, belongs in `infer/`.

  *How lenient this pass is about a name it cannot find is a real decision*, and getting it wrong fails quietly. An import can be what defines a name, so `opaque_imports` lets an unknown name through rather than refusing it. Only imports that genuinely are opaque set that flag. A std `use` is not opaque, because it contributes one name, its own, which this pass binds. When any `use` at all set the flag, an undefined name in a file holding one std import went straight through and produced a program that built cleanly and did nothing. `exit()` behaved that way until v1.3.20. Two things stay lenient: a `use` of a user module or a package, whose names only exist once the importer merges the modules, and every `from … use`, which binds bare names this pass never sees. `infer` checks both after the merge.

  `fill_struct_literal_defaults` is the half that waits for the merge. Over the merged program it folds an imported parent's fields into its children and fills the defaults of a literal whose class came from another file, copying each default from the class's definition, which the defining file's pass already rewrote and the import pass renamed. A default that builds its own class is refused in either half, since filling it would never end.
- *`infer/`* is Jade 2.0's whole-program checker, with its own README. It runs after imports are merged, gives every value one concrete type by checking each function once per set of argument types, and refuses the dynamic patterns v2 drops. Its answer is the input [`src/mir/`](../mir/README.md) lowers.
- *`mod.rs`* holds the module declarations.
- *`tests.rs`* holds the per-file pass tests (`mod resolve`), the types the whole-program checker gives the programs the per-file pass used to type (`mod types`), and serialization tests for the tree (`mod tree`).

## Who uses it

*Depends on:* `frontend/` for the tree and errors, and `builtins/` for the std packages and the global names.

*Used by:* `cli/build.rs` and `cli/check.rs`, which call `resolve` on the entry file; `aot/imports.rs`, which calls it on each imported file; `aot/mod.rs`, which runs `fill_struct_literal_defaults` and then `infer` over the merged program; `mir/`, which lowers the checker's answer; and `cache/`, which stores each file's tree after `resolve`.

## Gotchas

A class literal's field list is complete *except* under a `...base`, where it holds only the fields the literal named. Which of the other fields the base supplies depends on the base's class, which only `infer` knows, so the checker and MIR decide each one: a field the base's class has is copied from the base, and one it lacks takes the field's declared default. The order is fixed: a named field beats the base, and the base beats a default.

Each default in a class's definition is rewritten in place too, so a later stage never meets a `RawPipe` in one; a name it cannot see yet, such as a global defined further down, is left for the checker, because a default is only read where a literal leaves its field out.

Changing the shape of anything in the tree means bumping `CACHE_FORMAT_VERSION` in `src/cache/mod.rs`.

## Building and testing

```sh
cargo test typechecker::
```
