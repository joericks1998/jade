# `src/core/`: the globals that need no import

## What this subtree is

The handful of built-in names a program can use without a `use` line. Every other built-in belongs to a package and arrives through an import, so this directory is the short list of names that are simply *there*: `print`, `len`, `write`, `input`, `wait`, `route`, the task controls `cancelled`, `max_tasks` and `set_max_tasks`, the reply length `max_tokens` and `set_max_tokens`, the type functions `int`, `float`, `str`, `bool` and `char`, which check a value's type and return it unchanged, their converters `toInt`, `toFloat`, `toString`, `toBool` and `toChar`, the same pair for each sized number type, `byte` and `toByte` through `float32` and `toFloat32`, with `long` and `double` as other names for `int` and `float`, and `Grammar`.

`GLOBAL_NAMES` lists them, and `register_names` binds each in the per-file pass's name table, as a function, `Grammar` as a value, so a call to one is not an undefined name. The two sit side by side so a name cannot be listed without being bound. Nothing here runs: `typechecker/infer/builtins.rs` gives each one its static type, `mir/ops.rs` lowers each call, and the code they run is support code emitted by `src/backend/`. Until v1.4.8 this file also held each function's implementation for the bytecode VM.

## Why a name lives here rather than in a package

The test is whether an import would be *noise*. `len(x)` is not about a subsystem. Neither is `print`. Grouping them under a package would mean every program in the language starts with the same import line, which is a cost with nothing bought.

The async names are the interesting case, and they are here for the same reason rather than a different one. `async fn` and `await` are keywords, `join` is an operation in MIR, and `f.ready()` and `f.cancel()` are methods on a future. Nothing about running tasks is imported, so a package holding only the concurrency limit would have made that one knob the sole part of async needing a `use`. `async` is also a reserved word, so `std::async` could not have been spelled anyway.

## What each file does

- *`mod.rs`* holds `GLOBAL_NAMES` and `register_names`.

## The concurrency limit

`max_tokens()` answers how many tokens a prompt's reply runs to, 256 unless `set_max_tokens(n)` changes it, to at least 1. It lives in the program like the task limit, in `src/backend/infer.rs`, and setting it runs no model.

`max_tasks()` answers how many tasks may run at once, and `set_max_tasks(n)` changes it. The number itself lives in the program, in the task queue `src/backend/tasks.rs` emits, because a compiled binary obeys it through its own worker pool.

Three details are worth knowing:

*The default is a flat 32, not the core count.* A Jade task usually waits on a socket rather than saturating a core, so sizing the limit to the machine measured the wrong resource — the same fan-out took a different number of waves on a laptop and a build server.

*The setter answers with what took effect.* A request is clamped to `1..=512` rather than refused: zero runnable tasks is not a state a program can want, and 512 is a real property of the thread supply. Returning the clamped value is what makes the clamp visible, so a program asking for 9999 sees 512 without a second call.

*It replaced an environment variable.* `JADE_MAX_TASKS` did the same job through `getenv`, appeared nowhere in the docs, and reached compiled programs only, while `jade run` on the old interpreter ignored it entirely. A program that cares about its own fan-out width can now say so in the file that does the fan-out.

## Who uses it

*Depends on:* `typechecker/resolve` for `Names` and `NameKind`.

*Used by:* `builtins::global_names`, which the registry tests read, and `builtins::register_core_names`, which the per-file pass calls for every file. The static checker keeps its own list of the callable names in `is_builtin_value` and types each in `builtin_result`, both in `typechecker/infer/builtins.rs`, and `builtin` in `mir/ops.rs` lowers each one.

## Gotchas

*A global here needs a checker arm and a MIR arm in the same change.* Adding a name and a type teaches v1's type inference only. The name also goes in `is_builtin_value` and `builtin_result` in `typechecker/infer/builtins.rs`, in the `builtin` match in `mir/ops.rs`, and, if it does real work, in support code in `src/backend/`. Before `jade check` ran the backend's lowering, a missing arm meant `check` passed while `build` refused, which a program discovered only when it tried to ship. `jade check` runs MIR lowering now, so it reports the gap.

*A compiled integer is a full `i64` now.* v1 tagged its integers, so a lowering that returned one had to `tag_int` it and a lowering that took one had to `untag_int` first. Forgetting halved every value: `max_tasks()` answered 16 for 32, and `set_max_tasks(9999)` answered 256 for 512. The static backend has no tags, so a number passes straight through.

## Building and testing

```sh
cargo test core::
```
