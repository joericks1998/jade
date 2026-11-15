# `src/aot/`: the `jade build` driver

## What this subtree is

This is the driver every command that compiles goes through: `jade build`, `jade run`, `jade test` and `jade check`. It takes one file's tree, after the per-file pass in `typechecker/resolve.rs`, merges in everything the file imports, and runs the steps that turn it into a program:

```
Program → imports merged → class rules → struct defaults → prompts rewritten
        → typechecker::infer (static types) → mir (typed functions)
        → mir::taskcheck → backend (LLVM IR → objects → linked binary)
```

The tree is the parser's, `frontend::ast`, all the way through; no step adds a type to a node.

This directory owns the first steps and the order of the rest. Each step after the merge lives in its own directory: the checker in [`src/typechecker/infer/`](../typechecker/infer/README.md), the typed middle layer and the task check in [`src/mir/`](../mir/README.md), and code generation and linking in [`src/backend/`](../backend/README.md).

## Why it was built

Compiling in-process is a deliberate choice. Code generation used to live in a separate repository, behind a build daemon on `$HOME/.jade/build.sock`. A daemon built from an older commit resolved imports differently from the CLI calling it, and stayed silent until the two disagreed. Now there is one resolver, one code path, and one version.

The cost is that *building the toolchain needs LLVM 18 installed*, with `LLVM_SYS_180_PREFIX` pointing at it, and LLD from the same install. *Running* a released `jade` needs neither, because both are linked in. Nor does it need `cc`: a program is linked by the LLD inside `jade`, against the C library alone.

Until phase 6 of `design/v2-static-values.md` this directory also drove v1's backend, which lowered bytecode to LLVM IR one opcode at a time and linked two runtime libraries into every program. That backend, the bytecode and both runtimes are gone, and this is the only path.

## What each file does

- *`mod.rs`* is the driver. `merge` resolves imports, runs `typechecker::infer::check_members`, the class rules that need what a literal wrote rather than what the compiler filled in, then fills struct defaults with `typechecker::resolve::fill_struct_literal_defaults`, and adds the inference prelude when the program dereferences a prompt. A program with no file, such as a test's, has no imports to resolve, but a std package written in Jade that it uses is still loaded, through `imports::namespace_program`; and `stdlib::dedupe` keeps one copy of a library module the prelude and the program both loaded. `lower` runs the checker, lowers its answer to MIR, and runs `mir::taskcheck`. `compile` adds the backend: `Emit` picks a binary, the LLVM IR, or the MIR as text. Its `lib_cache` argument is where the backend keeps the std library's compiled instances, `backend::default_lib_cache()`, which is `~/.jade/lib`, when the CLI builds; tests pass a directory of their own, or `None`, which compiles the library with the program as 2.0.8 did. `--emit ir` and `--emit mir` never use it, so they show the whole program in one module. The Rust packages' staticlib, when a program calls one, rides along to MIR and the backend, which links it in; the binary needs nothing beside it. `would_build` runs everything short of LLVM, for `jade check`, which builds the Rust packages too, since their signatures come from the built library.
- *`imports.rs`* handles import resolution and module namespacing. Each imported file is parsed and run through the per-file pass, as the entry file was. Jade gives every imported file its own namespace, while a program has one symbol table. So this file mangles imported symbols to keep two modules that both define `greet` distinct. One renamer walks every expression, a class field's default included, since a default is the same tree as any other expression; a `RawPipe`, which the per-file pass never leaves behind, is reported as an internal error. A `use` naming a Rust package builds the project's Rust packages the first time (`rustpkg::build`): each exported struct becomes a class with public, required fields, and each function's signature is recorded for the checker; a name the package does not export is refused, which the gotcha below explains. A file inside a source package resolves its imports against the package's own `jade.toml` first, and `[package] exports` limits what an importing project can name.

  A `use std::path`, or any std package written in Jade (`src/lib/`, `crate::stdlib`), loads that module once per program, `load_library`, namespaced under the package's reserved id, so `path.join(a, b)` becomes `join$<id>(a, b)` and no name in the module can meet the program's. The `use` binds the package's public functions, its top-level `fn`s without a leading `_`; `from std::path use join` binds one. Its classes stay its own, so a program's class of the same name never meets one. A library module may `use` another, and then reaches all of its top-level functions, private ones included, since `Registry::library_values` keeps each loaded module's names: `uhttp.jde` calls `http._fetch` and returns `http.jde`'s `HttpResponse`. A call to a function the registry says folds, `path.join`, with more than two arguments becomes nested calls of two, `fold_long_call`, as v1's variadic join computed it. The hook package `std::__sys` is dropped like the backend's packages, and only library modules get that far with it.
- *`tests.rs`* holds the driver tests.

## Who uses it

*Depends on:* `frontend/` for the tree, `typechecker/` for the per-file pass, the default fill and the checker, `mir/` and `backend/` for everything after it, `project/` and `pkg/` for library resolution.

*Used by:* `src/build/`, which is the thin layer `cli/build.rs`, `cli/run.rs` and `cli/test.rs` call, and `cli/check.rs`, which calls `would_build`.

## Gotchas

*A mistyped native name has to be caught here.* Before 2.0.5, `gfx.jade_gfx_key_presed` could pass `jade check`, build, ship, and fail the first time that line ran, because nothing linked the name. A Rust package's library lists everything it exports, so `Renamer::ref_native_qual`, the one place `alias.field` becomes `__native$<pkgid>$<field>`, checks the field against that list and suggests the closest name. Its `.jde` sibling `ref_value_qual` checks whether a module exports the name in the same way. A struct's name is left to `ref_type`, which names its class.

*`would_build` has to tell an unresolved import apart from a wrong program.* It probes a build on behalf of `jade check`, and deliberately stays quiet about an import that does not resolve, because that means a dependency is not installed and `check_imports` says so in better words. That silence used to swallow *every* resolver error, which is why `check` once reported `ok` for a program with a bad FFI symbol. `ResolveError` splits the two: an `Unresolved` error is dropped, and a `Program` error is reported.

*No test may pass the real cache.* `cargo test` runs in parallel and must not touch `~/.jade`, so `compile` takes the cache's root as an argument rather than reading `HOME` itself, and every test passes a scratch directory or `None`. The CLI is the one caller that passes `backend::default_lib_cache()`. For a cold-cache measurement of the `jade` binary, run it with `HOME` set to an empty directory, which is where `rustpkg::jade_home` finds its root.

*The task check runs on every build.* v1 ran it only inside bytecode emission, and the static backend skipped it until phase 6, building programs whose tasks raced. It now runs in `lower`, so `jade build`, `jade run` and `jade check` all refuse the same programs.

## Building and testing

```sh
cargo test aot::
```
