# `src/codegen/func/`: writing an LLVM function

## What this subtree is

Everything codegen writes is an LLVM function: each MIR function the program defines, and each piece of support code it needs, such as concatenating two strings. This directory holds the three things every one of those functions uses: the builder they are written in, the way they raise an error, and the lowering of one MIR function into LLVM.

## Why it is a directory

These were three files at the top of `src/codegen/` until 2.0.13, beside the files for values, tasks and inference. They belong together because they are the tools for writing a function, and the rest of codegen is written with them.

## What each file does

- *`builder.rs`* is `F`, a function-building layer over inkwell's `Builder`. Its methods return values directly, and `if_`, `if_else`, `while_` and `for_range` take closures, so support code reads close to the C it replaces. All of codegen is written in it; `codegen::F` re-exports it. It was `fb.rs` until 2.0.13.
- *`errors.rs`* has raising. A raised error is a class value in the thread-local slot `jade.err`; the code after every operation that can raise checks the slot, and goes to the nearest `catch` or leaves the function. It has the slot, `jade.new_error`, an error's message, the report for an uncaught error, the `F::fail` and `F::raise` helpers, `IO_ERROR`, the prefix of every failure of the outside world, `RECURSION_LIMIT`, and `thread_global`, which makes the thread-local slots through `Cx::state`.
- *`mod.rs`* lowers one MIR function: locals as zeroed stack slots, statements with the ownership rules, terminators, every binary operator with its overflow and zero checks, and every support operation. Its `Env` holds each function's LLVM function by id; a library module declares only the ones it calls. A library function, `Func::library`, skips the recursion count, as a built-in always did, so the optimizer inlines a one-instruction one such as `math.sqrt` down to that instruction.

## Who uses it

*Used by:* `../mod.rs`, which calls `lower` for each function of the program's module, and `../libcache/`, which calls it for each library instance's module. `F` and the raising helpers are used by every file in codegen.

*Depends on:* `../memory/` for layouts and reference counting, `../tasks/` for a job's offsets, `../rustpkg.rs` for a call into a Rust package, `../sys/` for each `__sys` hook, and `../support/` for the support operations.

## Gotchas

*Never emit into a terminated block.* `F`'s branch helpers check, and `mod.rs` starts a fresh block after any statement that ends one, such as a failing operation. Raw builder calls placed after a `ret` or `unreachable` produce a module that does not verify.

## Building and testing

```sh
cargo test codegen::
```
