# `src/array/`: `std::array` and the array methods

## What this subtree is

`use std::array` binds `array`, whose functions take an array first: `map`, `filter`, `sort`, `reverse` and `join`. An array value also has methods, such as `a.push(x)` and `a.len()`, which need no import.

This directory holds only names. Nothing here runs: the code each function runs is support code the backend writes into the program that calls it. Until v1.4.8 this directory also held the bytecode VM's half of the package, and until 2.0.6 a function that registered the package's name with the per-file pass, which now binds the name itself.

## Why it was built

Each std package is a flat top-level module, so the registry in `src/builtins/` can list every package in one table, and the checker, MIR and the backend can each find a package's functions by name. The package functions take the *functional* style, so `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place. `typechecker/infer/builtins.rs` names the receiver-first functions one by one for that reason; see `src/builtins/README.md`.

## What each file does

- *`mod.rs`* holds `ARRAY_PKG`, the `Package` entry that names the package's import path, the global it binds, and its functions, and `ARRAY_METHODS`, the names of the methods a `array` value has.

## Who uses it

*Depends on:* `builtins::Package`, the shape of its entry.

*Used by:*

- [`src/builtins/`](../builtins/README.md) lists `ARRAY_PKG` in the package table, which the per-file pass in `typechecker/resolve.rs` reads to bind the `array` name where a `use` names the package and to refuse a function the package does not have.
- `typechecker/infer/builtins.rs` gives each call its static type, and `mir/lower/packages.rs` and `mir/lower/ops.rs` lower it to a MIR operation.
- `src/codegen/support/collections.rs` emits the code, into a program only when that program uses it.
- `builtins::primitive_method_tables` lists `ARRAY_METHODS` under `array`, and `builtins::primitive_method_arity` holds each method's argument count.
