# `src/dict/`: `std::dict` and the dict methods

## What this subtree is

`use std::dict` binds `dict`, whose functions take a dict first: `keys`, `values`, `has`, `get` and `merge`. A dict value also has methods, such as `d.keys()` and `d.has(k)`, which need no import.

This directory holds only names. Nothing here runs: the code each function runs is support code the backend writes into the program that calls it. Until v1.4.8 this directory also held the bytecode VM's half of the package, and until 2.0.6 a function that registered the package's name with the per-file pass, which now binds the name itself.

## Why it was built

Each std package is a flat top-level module, so the registry in `src/builtins/` can list every package in one table, and the checker, MIR and the backend can each find a package's functions by name. A dict holds one kind of value, as the checker in `typechecker/infer/` enforces, so `d.values()` is an array of that kind.

## What each file does

- *`mod.rs`* holds `DICT_PKG`, the `Package` entry that names the package's import path, the global it binds, and its functions, and `DICT_METHODS`, the names of the methods a `dict` value has.

## Who uses it

*Depends on:* `builtins::Package`, the shape of its entry.

*Used by:*

- [`src/builtins/`](../builtins/README.md) lists `DICT_PKG` in the package table, which the per-file pass in `typechecker/resolve.rs` reads to bind the `dict` name where a `use` names the package and to refuse a function the package does not have.
- `typechecker/infer/builtins.rs` gives each call its static type, and `mir/packages.rs` and `mir/ops.rs` lower it to a MIR operation.
- `src/backend/collections.rs` emits the code, into a program only when that program uses it.
- `builtins::primitive_method_tables` lists `DICT_METHODS` under `dict`, and `builtins::primitive_method_arity` holds each method's argument count.
