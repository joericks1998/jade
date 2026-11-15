# `src/random/`: `std::random`

## What this subtree is

`use std::random` binds `random`: `int`, between a low and a high int, `float`, `choice` from an array, `shuffle`, and `seed`, which fixes the sequence.

This directory holds only names. Nothing here runs: the code each function runs is support code the backend writes into the program that calls it. Until v1.4.8 this directory also held the bytecode VM's half of the package, and until 2.0.6 a function that registered the package's name with the per-file pass, which now binds the name itself.

## Why it was built

Each std package is a flat top-level module, so the registry in `src/builtins/` can list every package in one table, and the checker, MIR and the backend can each find a package's functions by name. One generator serves the whole program, v1's xoshiro256**, so a seeded program draws the same values it drew under v1.

## What each file does

- *`mod.rs`* holds `RANDOM_PKG`, the `Package` entry that names the package's import path, the global it binds, and its functions.

## Who uses it

*Depends on:* `builtins::Package`, the shape of its entry.

*Used by:*

- [`src/builtins/`](../builtins/README.md) lists `RANDOM_PKG` in the package table, which the per-file pass in `typechecker/resolve.rs` reads to bind the `random` name where a `use` names the package and to refuse a function the package does not have.
- `typechecker/infer/builtins.rs` gives each call its static type, and `mir/packages.rs` lowers it to a MIR operation.
- `src/backend/random.rs` emits the code, into a program only when that program uses it.
