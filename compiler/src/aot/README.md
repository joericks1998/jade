# `src/aot/`: the `jade build` driver

## What this subtree is

This is the driver every command that compiles goes through: `jade build`, `jade run`, `jade test` and `jade check`. It takes one file's typed tree, merges in everything the file imports, and runs the steps that turn it into a program:

```
TProgram → imports merged → class rules → struct defaults
         → typechecker::infer (static types) → mir (typed functions)
         → mir::taskcheck → backend (LLVM IR → object → linked binary)
```

This directory owns the first steps and the order of the rest. Each step after the merge lives in its own directory: the checker in [`src/typechecker/infer/`](../typechecker/infer/README.md), the typed middle layer and the task check in [`src/mir/`](../mir/README.md), and code generation and linking in [`src/backend/`](../backend/README.md).

## Why it was built

Compiling in-process is a deliberate choice. Code generation used to live in a separate repository, behind a build daemon on `$HOME/.jade/build.sock`. A daemon built from an older commit resolved imports differently from the CLI calling it, and stayed silent until the two disagreed. Now there is one resolver, one code path, and one version.

The cost is that *building the toolchain needs LLVM 18 installed*, with `LLVM_SYS_180_PREFIX` pointing at it, and LLD from the same install. *Running* a released `jade` needs neither, because both are linked in. Nor does it need `cc`: a program is linked by the LLD inside `jade`, against the C library alone.

Until phase 6 of `design/v2-static-values.md` this directory also drove v1's backend, which lowered bytecode to LLVM IR one opcode at a time and linked two runtime libraries into every program. That backend, the bytecode and both runtimes are gone, and this is the only path.

## What each file does

- *`mod.rs`* is the driver. `merge` resolves imports, runs `typechecker::infer::check_members`, the class rules that need what a literal wrote rather than what the compiler filled in, and then fills struct defaults. `lower` runs the checker, lowers its answer to MIR, and runs `mir::taskcheck`. `compile` adds the backend: `Emit` picks a binary, the LLVM IR, or the MIR as text. A binary build also hands back the native packages from the project's `libs/` the program will look for, so the CLI copies them beside it. `would_build` runs everything short of LLVM, for `jade check`.
- *`imports.rs`* handles import resolution and module namespacing. Jade gives every imported file its own namespace, while a program has one symbol table. So this file mangles imported symbols to keep two modules that both define `greet` distinct. It also reads what `jade.toml` declares about native packages: their functions' types, and the records they pass, which become classes; and it checks a call into a C dependency against the symbols its manifest declares, which the gotcha below explains. A file inside a source package resolves its imports against the package's own `jade.toml` first, and `[package] exports` limits what an importing project can name.
- *`tests.rs`* holds the driver tests.

## Who uses it

*Depends on:* `typechecker/` for the typed tree and the checker, `mir/` and `backend/` for everything after it, `project/` and `pkg/` for library resolution.

*Used by:* `src/build/`, which is the thin layer `cli/build.rs`, `cli/run.rs` and `cli/test.rs` call, and `cli/check.rs`, which calls `would_build`.

## Gotchas

*A mistyped FFI symbol is nobody's link error, so this pass has to be the one that catches it.* `gfx.jade_gfx_key_presed` used to pass `jade check`, then build, link, package, and ship, and fail the first time that line ran. Nothing linked the name, so no linker could object to it.

The answer was in the project's own `jade.toml` the whole time. An `abi = "c"` dependency must declare a `[symbols]` table, and that table is the complete list of what the generated shim binds. `Renamer::ref_native_qual` is the one place `alias.field` becomes `__native$<pkgid>$<field>`, and it checks the field against that table. Its `.jde` sibling `ref_value_qual` has always checked whether the module exports the name, in exactly the same way.

Two rules keep the check from rejecting anything real. A package with *no* declared table is not checked at all, because a Jade-ABI package declares its exports in its own project, which this manifest cannot see. And a `[lib]` sharing a name with a dependency wins the import, so the dependency's table describes a library the build is not using, and the check turns itself off.

*`would_build` has to tell an unresolved import apart from a wrong program.* It probes a build on behalf of `jade check`, and deliberately stays quiet about an import that does not resolve, because that means a dependency is not installed and `check_imports` says so in better words. That silence used to swallow *every* resolver error, which is why `check` once reported `ok` for a program with a bad FFI symbol. `ResolveError` splits the two: an `Unresolved` error is dropped, and a `Program` error is reported.

*An artifact must not name a dependency by where that dependency sat at build time.* A program looks for a native package in `JADE_LIBS`, then in `libs/` beside itself, and only then at the absolute path it had at build time, which is the answer for a library that is not a dependency at all. `backend/native.rs` holds that search, and the CLI copies the project's dependencies into a `libs/` beside the binary.

*The task check runs on every build.* v1 ran it only inside bytecode emission, and the static backend skipped it until phase 6, building programs whose tasks raced. It now runs in `lower`, so `jade build`, `jade run` and `jade check` all refuse the same programs.

## Building and testing

```sh
cargo test aot::
```
