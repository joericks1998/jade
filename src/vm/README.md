# `src/vm/`: the bytecode interpreter

## What this subtree is

This is one of Jade's two execution engines. `jade run` compiles a program to a `Chunk` and interprets it here. `jade build` lowers the same chunk to LLVM in `src/aot/`.

Neither engine is the reference implementation of the other. `src/scripts/backend-parity.sh` runs every example through both and diffs the output. The two have silently disagreed before, and the language is defined by what they agree on.

## Why it is shaped this way

The important structural decision is that *value semantics do not live here*. Arithmetic, formatting, coercion rules, collection payloads, and the trust model all live in the shared `jade-runtime` crate. The VM links that crate as an rlib, and AOT binaries link it as a C-ABI static library.

That split fixes a real history. The VM, written in Rust around `VmValue` and `Arc`, and the AOT backend, written in C around `jrt_*`, used to be two independent implementations of the same language. Every place they diverged became a bug found after the fact.

What remains in this directory is *interpretation*: the dispatch loop, the call protocol, and the async and prompt machinery that has no compiled counterpart.

This was once a single large file, split up a piece at a time. `mod.rs` re-exports the shared import set at `pub(crate)`, so each submodule can pull it all in with one `use super::*;`. That is why the submodules have almost no import preamble.

## What each file does

- *`mod.rs`* holds the shared import preamble, the submodule declarations, and the re-exports. Very little logic.
- *`value.rs`* holds `VmValue`, the enum the interpreter dispatches on, plus its display and type-name projections. It also holds `NativeFnId`, the id of a native function that needs `VmState` access and therefore cannot be a plain `BuiltinFn`, and `BoundNativeFn`, which is one of those with its receiver already attached so it can be called as a method.
- *`state.rs`* holds `VmState`, which carries the globals, the struct and method tables, the import cycle guard, the inference backend, and the REPL capture slot. It also holds `VmOpts`, the per-run configuration. Globals use an `FxHashMap`, because variable names are short internal keys that get hashed on every `GetGlobal`.
- *`dispatch.rs`* is the interpreter loop. `execute_chunk` decodes each `Instr`, drives control flow and the exception handler stack, and hands value work to `jade-runtime` and the sibling submodules. The register slot accessors sit at the bottom of the file.
- *`call.rs`* handles call dispatch. `call_value` is the one entry point for calling anything callable, covering user functions and closures, bound methods, native and library functions, stateful `NativeFnId` package methods, and type constructors. `call_fn` runs a compiled body in a fresh register frame.
- *`chunk.rs`* holds the program entry points, `run` and `run_incremental`, plus user-import resolution. An imported file runs in a sub-state that shares globals, struct definitions, and methods with its importer.

  `resolve_user_import` is a thin adapter over `project::resolve_import`, which is also what `jade check` uses to walk the import graph. Sharing that one function is what stops `check` from accepting a `use` the VM then cannot find.
- *`coerce.rs`* turns model replies and other values into typed Jade values. It also handles calling a type as a constructor, such as `City(dict)` or `int("3")`, plus the bridge from JSON to `VmValue`.
- *`llm_prompt.rs`* handles prompt dereference. Both `?p` and `?p |> Type` lower to `vm_prompt_deref`, which sends the request, optionally constrains sampling with a grammar, coerces the reply, and retries on failure. It also drains live token streams, with optional anchor-based muting.
- *`async_tasks.rs`* holds the `JadeFuture` and token-stream handle types, plus the task body a spawned task runs on its own `VmState`. The `spawn`, `await`, and `join` opcodes are dispatched inline in `dispatch.rs`, because they manipulate register slots directly.

  Two of those three have a subtlety worth knowing. `call_value` is where an `async fn` reached as a *value* starts its task, because a call site with only the value has no static type to decide from — `let f = w`, `[1, 2].map(w)`, and an `async fn` imported from a module all arrive as ordinary calls. A task must not re-enter that decision, so the task body runs `call_value_body`; going back through the front door would spawn a second task and hand the awaiter a future where it expects a value. `dispatch.rs`'s `Instr::Call` has a fast path that borrows the `Arc<CompiledFn>` and calls `call_fn` directly, and it has to exclude an async callee for the same reason.

  And neither `vm_err!` nor `vm_try!` may be used inside a loop. They dispatch by popping a handler, setting `ip`, and `continue`-ing the dispatch loop, and `continue` binds to the nearest loop — so `join`'s loop over its tasks popped a handler without jumping, and a second failure found the stack empty and escaped the enclosing `try` entirely. `join` collects everything first and dispatches once at the end.
- *`ops.rs`* holds the dynamic operators, meaning the runtime-typed ones used when inference could not specialize. Decisions route through `jade_runtime::dynop` so the two engines cannot diverge. What this file owns is the VM-specific set: bitwise and shift, `in`, indexing, and unary operators.

  `vm_scalar_eq` is membership equality. It answers `false` across kinds where `==` raises. Its AOT counterpart is `jrt_core_eq_total`.
- *`exceptions.rs`* shapes a built-in error into a catchable Jade value. The `try`, `catch`, and `raise` control flow itself sits inline in `dispatch.rs`.
- *`tests.rs`* is the largest test file in the repo. Three helpers: `run_src(src)` runs the whole pipeline, `run_src_with_mock(src, responses)` stubs the inference backend, and `run_src_with_stdout_capture` checks printed output.

## Who uses it

*Depends on:* `bytecode/` for the instruction set, `compiler/` for `CompiledProgram`, `builtins/` for the native registry, `llm/` for the inference backend, `native/` for dlopen'd packages, `project/` for import resolution, and `jade-runtime` for all value semantics.

*Used by:* `cli/run.rs`, `cli/repl.rs`, and `cli/test.rs`. Also `builtins/` and the std packages, which are written against `VmValue`.

## Gotchas

*A keyword call has to fill the defaults it skipped, here.* `resolve_named_args` turns a mix of positional and named arguments into one positional list, and that list is the *complete* argument list — `call_fn` gets it and cannot tell a parameter that was omitted from one explicitly given `nil`. So a parameter that got neither kind of argument has to take its declared default before this function returns.

It used to start from a vector of `nil` and overwrite only what was supplied, which meant naming any argument silently blanked the defaults of the ones you did not name: `f(1, c = 9)` on `fn f(a, b = 2, c = 3)` passed `b` as `nil`. The compiled backend fills its defaults at the call site and was right all along, so this only ever went wrong under `jade run`. A parameter with no default and no argument is now a named error rather than another `nil`.

*Do not mutate process-global state in tests.* `cargo test` is heavily parallel, so `std::env::set_var` races against every other thread calling `getenv`. That is a genuine data race, and it is why `set_var` is `unsafe` as of the 2024 edition. Inject a path instead, or use a `#[cfg(test)]` thread-local with an RAII guard.

*No panics on the interpreter path.* Every failure returns a `JadeError` carrying a span, including anything derived from user input.

Adding a stateful package function means adding a `NativeFnId` variant in `value.rs` and a match arm in `call_value`.

Making it callable as a *method* is a second step. `find_primitive_method` hands back a plain `BuiltinFn`, which a stateful function is not. So `GetField` binds the receiver to the id instead, producing a `VmValue::BoundNativeFn`, and `call_value` unpacks that by putting the receiver back at the front of the arguments. `array_fn_method` in `dispatch.rs` is the complete list of these. Missing that step is why `array.map(a, f)` worked while `a.map(f)` did not, until v1.3.21.

## Building and testing

```sh
cargo test vm::
./src/scripts/backend-parity.sh    # run every example on both engines and diff
```
