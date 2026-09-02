# `src/core/`: the globals that need no import

## What this subtree is

The handful of built-in functions a program can call without a `use` line. Every other built-in belongs to a package and arrives through an import, so this directory is the short list of names that are simply *there*: `write`, `len`, `input`, `cancelled`, `max_tasks`, and `set_max_tasks`.

It also holds `register_types`, which declares the type of every bare global to the checker. That covers a few names implemented elsewhere — `print`, `join`, and `wait` are stateful and dispatch through `NativeFnId` rather than living here — because the checker wants one place to read them all, and splitting the declarations from the list would mean a name could be callable without being typed.

## Why a name lives here rather than in a package

The test is whether an import would be *noise*. `len(x)` is not about a subsystem. Neither is `print`. Grouping them under a package would mean every program in the language starts with the same import line, which is a cost with nothing bought.

The async names are the interesting case, and they are here for the same reason rather than a different one. `async fn` and `await` are keywords, `join` is an opcode, and `f.ready()` and `f.cancel()` are methods on a future. Nothing about running tasks is imported, so a package holding only the concurrency limit would have made that one knob the sole part of async needing a `use`. `async` is also a reserved word, so `std::async` could not have been spelled anyway.

## What each file does

- *`mod.rs`* holds each function's implementation, the `BuiltinFn` constant the registry reads, and `register_types`.
- *`tests.rs`* covers the arity and type branches of each one.

## The concurrency limit

`max_tasks()` answers how many tasks may run at once, and `set_max_tasks(n)` changes it. The number itself lives in `jade_runtime::task`, because a compiled binary obeys it through its own worker pool and the two engines must not each keep a copy.

Three details are worth knowing:

*The default is a flat 32, not the core count.* A Jade task usually waits on a socket rather than saturating a core, so sizing the limit to the machine measured the wrong resource — the same fan-out took a different number of waves on a laptop and a build server.

*The setter answers with what took effect.* A request is clamped to `1..=512` rather than refused: zero runnable tasks is not a state a program can want, and 512 is a real property of the thread supply. Returning the clamped value is what makes the clamp visible, so a program asking for 9999 sees 512 without a second call.

*It replaced an environment variable.* `JADE_MAX_TASKS` did the same job through `getenv`, appeared nowhere in the docs, and reached the compiled engine only — `jade run` ignored it entirely. A program that cares about its own fan-out width can now say so in the file that does the fan-out.

## Who uses it

*Depends on:* `builtins/` for `BuiltinFn`, `vm/` for `VmValue`, `compiler/type_infer` for `TypeContext`, `stdio/` for writes that survive a closed pipe, and `jade_runtime::task` for the task limit.

*Used by:* `builtins::CORE_BUILTINS`, which seeds each one into a fresh `VmState`. `codegen/calls.rs` lists the same names in `LOWERABLE_BUILTINS` so a compiled program gets them too.

## Gotchas

*A global here needs a codegen arm in the same change.* Adding a `BuiltinFn` and a type teaches the interpreter only. The name also goes in `LOWERABLE_BUILTINS`, in both arity checks in `codegen/calls.rs`, in `RESERVED_BUILTINS` in `codegen/builtins.rs`, and in the lowering match in `codegen/instr.rs`. Miss the last and `jade check` passes while `jade build` reports an unsupported builtin call, which a program discovers only when it tries to ship.

*A compiled integer is tagged.* The runtime counts in plain `i64`, so a lowering that returns one has to `tag_int` it and a lowering that takes one has to `untag_int` first. Forgetting halves every value: `max_tasks()` answered 16 for 32, and `set_max_tasks(9999)` answered 256 for 512.

## Building and testing

```sh
cargo test core::
```
