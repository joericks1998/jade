# `src/build/`: the driver

## What this subtree is

This is the driver every command that compiles goes through: `jade build`, `jade run`, `jade test` and `jade check`. `cli/build.rs` lexes and parses the entry file and runs the per-file pass, `typechecker/resolve.rs`; this directory takes the tree from there and runs each later step in order, one step per directory:

```
one file's tree
  → mir/imports.rs        every imported file merged into one tree
  → class rules, struct defaults, each ?p rewritten into a call into the inference prelude
  → typechecker/infer/    each value's type, each call's target
  → mir/lower/            MIR
  → mir/taskcheck.rs      refuses tasks that race
  → codegen/              LLVM IR, which LLVM compiles into object files
  → link/                 the objects and the C library joined into a binary
```

Each step lives in its own directory. This one owns the order, and nothing in a later step calls back into an earlier one or ahead into the next.

## Why it was built

Compiling in-process is a deliberate choice. Code generation used to live in a separate repository, and this directory was a client for a build daemon on `$HOME/.jade/build.sock`. A daemon built from an older commit resolved imports differently from the CLI calling it, and stayed silent until the two disagreed. Now there is one resolver, one code path and one version.

The cost is that *building the toolchain needs LLVM 18 installed*, with `LLVM_SYS_180_PREFIX` pointing at it, and LLD from the same install. *Running* a released `jade` needs neither, because both are linked in. Nor does it need `cc`: a program is linked by the LLD inside `jade`, against the C library alone.

Until 2.0.11 the driver was `src/aot/mod.rs`, beside the import pass, and this directory was a thin layer that called it. The import pass moved into `src/mir/`, since it is the first step toward MIR, and the driver moved here.

## What each file does

- *`mod.rs`* is the driver. `merge` resolves imports through `mir::imports`, runs `typechecker::infer::check_members`, the class rules that need what a literal wrote rather than what the compiler filled in, then fills struct defaults with `typechecker::resolve::fill_struct_literal_defaults`, and adds the inference prelude when the program dereferences a prompt. A program with no file, such as a test's, has no imports to resolve, but a std package written in Jade that it uses is still loaded, through `imports::namespace_program`; and `stdlib::dedupe` keeps one copy of a library module the prelude and the program both loaded. `lower` runs the checker, lowers its answer to MIR, and runs `mir::taskcheck`. `compile` runs the rest: `Emit` picks a binary, the LLVM IR, the MIR as text, or the assembly for this machine. `binary` is the last two steps: `codegen::objects` writes the object files, `link::link` joins them, and when a link with an object from the cache fails, it compiles once more in one module, since a damaged object is the one thing a cache adds, and warns. Its `lib_cache` argument is where codegen keeps the std library's compiled instances, `codegen::default_lib_cache()`, which is `~/.jade/lib`, when the CLI builds; tests pass a directory of their own, or `None`, which compiles the library with the program. `--emit ir`, `--emit mir` and `--emit asm` never use it, so they show the whole program in one module. `would_build` runs everything short of LLVM, for `jade check`, which builds the Rust packages too, since their signatures come from the built library. `build` is what the CLI calls: it compiles and writes IR or MIR to standard output.
- *`tests.rs`* holds tests for building a binary, with a cache root inside the test's own directory, printing IR, MIR and assembly, a missing source file, a type error and a racing task reported by `would_build`.

## Who uses it

*Used by:* `cli/build.rs`, which implements `jade build` and the builds behind `jade run` and `jade test`, and `cli/check.rs`, which calls `would_build`. Codegen's tests call `binary` to build and link a program from MIR.

*Depends on:* `mir/` for imports, lowering and the task check, `typechecker/` for the checker and the default fill, `infer/` for the prompt rewrite and the model's plan, `codegen/` for object files, and `link/` for the binary.

## Gotchas

*`would_build` has to tell an unresolved import apart from a wrong program.* It probes a build on behalf of `jade check`, and deliberately stays quiet about an import that does not resolve, because that means a dependency is not installed and `check_imports` says so in better words. That silence used to swallow *every* resolver error, which is why `check` once reported `ok` for a program with a bad FFI symbol. `ResolveError` splits the two: an `Unresolved` error is dropped, and a `Program` error is reported.

*No test may pass the real cache.* `cargo test` runs in parallel and must not touch `~/.jade`, so `compile` takes the cache's root as an argument rather than reading `HOME` itself, and every test passes a scratch directory or `None`. The CLI is the one caller that passes `codegen::default_lib_cache()`. For a cold-cache measurement of the `jade` binary, run it with `HOME` set to an empty directory, which is where `rustpkg::jade_home` finds its root.

*The task check runs on every build.* v1 ran it only inside bytecode emission, and the compiled backend skipped it until phase 6, building programs whose tasks raced. It now runs in `lower`, so `jade build`, `jade run` and `jade check` all refuse the same programs.

*Building this module, and so the whole toolchain, requires LLVM 18 and LLD.* Set `LLVM_SYS_180_PREFIX` if you are not on Apple Silicon Homebrew. `.cargo/config.toml` carries that path as a default, and Cargo's `[env]` section does not overwrite a variable you have already set.

## Building and testing

```sh
cargo test build::
```
