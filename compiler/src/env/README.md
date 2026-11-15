# `src/env/`: `std::env`

## What this subtree is

`use std::env` binds `env`: `get` and `set` for environment variables, `args` for the program's arguments, and `cwd` for its working directory.

This directory holds only names. Nothing here runs: the code each function runs is support code the backend writes into the program that calls it. Until v1.4.8 this directory also held the bytecode VM's half of the package, and until 2.0.6 a function that registered the package's name with the per-file pass, which now binds the name itself.

## Why it was built

Each std package is a flat top-level module, so the registry in `src/builtins/` can list every package in one table, and the checker, MIR and the backend can each find a package's functions by name. An environment variable is set by whoever started the program, so what `env.get` returns is tainted, and `std::sh` refuses it as a command. The arguments and the working directory are trusted.

## What each file does

- *`mod.rs`* holds `ENV_PKG`, the `Package` entry that names the package's import path, the global it binds, and its functions.

## Who uses it

*Depends on:* `builtins::Package`, the shape of its entry.

*Used by:*

- [`src/builtins/`](../builtins/README.md) lists `ENV_PKG` in the package table, which the per-file pass in `typechecker/resolve.rs` reads to bind the `env` name where a `use` names the package and to refuse a function the package does not have.
- `typechecker/infer/builtins.rs` gives each call its static type, and `mir/packages.rs` lowers it to a MIR operation.
- `src/backend/env.rs` emits the code, into a program only when that program uses it.
