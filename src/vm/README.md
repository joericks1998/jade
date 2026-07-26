# `src/vm/` — the bytecode interpreter

## What this subtree is

One of Jade's two execution engines. `jade run` compiles a program to a `Chunk` and interprets it here. `jade build` lowers the same chunk to LLVM in `src/aot/`.

Neither engine is the reference implementation of the other. `scripts/backend-parity.sh` runs every example through both and diffs the output, because they have silently disagreed before and the language is defined by what they agree on.

## Why it is shaped this way

The important structural decision: *value semantics do not live here*. Arithmetic, formatting, coercion rules, collection payloads, the trust model — all of it is in the shared `jade-runtime` crate, which the VM links as an rlib and AOT binaries link as a C-ABI staticlib. That is a deliberate fix for a real history: the VM (Rust, `VmValue` + `Arc`) and the AOT backend (C, `jrt_*`) used to be two independent implementations of the same language, and every divergence between them was a bug found after the fact.

What remains in this directory is *interpretation*: the dispatch loop, the call protocol, and the async and prompt machinery that has no compiled counterpart.

The file was once a monolith and has been split incrementally. `mod.rs` re-exports the shared import set at `pub(crate)` so each submodule can pull it all in with a single `use super::*;` — that is why the submodules have almost no import preamble.

## What each file does

- **`mod.rs`** — the shared import preamble, submodule declarations, and re-exports. Little logic.
- **`value.rs`** — `VmValue`, the enum the interpreter dispatches on, plus the display and type-name projections. Also `NativeFnId`: the id of a native function that needs `VmState` access and so cannot be a pure `BuiltinFn`.
- **`state.rs`** — `VmState` (globals, struct and method tables, the import cycle guard, the inference backend, the REPL capture slot) and `VmOpts`, the per-run configuration. Globals use `FxHashMap` because variable names are short internal keys hashed on every `GetGlobal`.
- **`dispatch.rs`** — the interpreter loop. `execute_chunk` decodes each `Instr`, drives control flow and the exception handler stack, and delegates value work to `jade-runtime` and the sibling submodules. The register slot accessors live at the bottom of the file.
- **`call.rs`** — call dispatch. `call_value` is the single entry point for calling anything callable: user functions and closures, bound methods, native and library functions, stateful `NativeFnId` package methods, and type constructors. `call_fn` runs a compiled body in a fresh register frame.
- **`chunk.rs`** — program entry points (`run`, `run_incremental`) and user-import resolution. An imported file runs in a sub-state that shares globals, struct definitions, and methods with its importer.
- **`coerce.rs`** — turning model replies and values into typed Jade values, plus calling a type as a constructor (`City(dict)`, `int("3")`), struct decorators, and the JSON-to-`VmValue` bridge.
- **`llm_prompt.rs`** — prompt dereference. `?p` and `?p |> Type` lower to `vm_prompt_deref`: send the request, optionally constrain sampling with a grammar, coerce the reply, retry on failure. Also drains live token streams, with optional anchor-based muting.
- **`async_tasks.rs`** — the `JadeFuture` and token-stream handle types, and the task body a spawned task runs on its own `VmState`. The `spawn`/`await`/`join` opcodes themselves are dispatched inline in `dispatch.rs` because they manipulate register slots.
- **`ops.rs`** — dynamic (runtime-typed) operators, for when inference could not specialize. Decisions route through `jade_runtime::dynop` so the two engines cannot diverge; what is owned here is the VM-specific set — bitwise and shift, `in`, indexing, unary.
- **`exceptions.rs`** — shaping a built-in error into a catchable Jade value. The `try`/`catch`/`raise` control flow itself is inline in `dispatch.rs`.
- **`tests.rs`** — the largest test file in the repo. Helpers: `run_src(src)` runs the whole pipeline, `run_src_with_mock(src, responses)` stubs the inference backend, `run_src_with_stdout_capture` checks printed output.

## Who uses it

*Depends on:* `bytecode/` for the instruction set, `compiler/` for `CompiledProgram`, `builtins/` for the native registry, `llm/` for the inference backend, `native/` for dlopen'd packages, `project/` for import resolution, and `jade-runtime` for all value semantics.

*Used by:* `cli/run.rs`, `cli/repl.rs`, and `cli/test.rs`. Also `builtins/` and the std packages, which are written against `VmValue`.

## Gotchas

**Do not mutate process-global state in tests.** `cargo test` is heavily parallel, so `std::env::set_var` races against every other thread calling `getenv` — a genuine data race, and why `set_var` is `unsafe` as of the 2024 edition. Inject a path or use a `#[cfg(test)]` thread-local with an RAII guard instead.

**No panics on the interpreter path.** Every failure returns a `JadeError` carrying a span, including anything derived from user input.

Adding a stateful package function means adding a `NativeFnId` variant in `value.rs` and a match arm in `call_value`.

## Building and testing

```sh
cargo test vm::
./scripts/backend-parity.sh    # run every example on both engines and diff
```
