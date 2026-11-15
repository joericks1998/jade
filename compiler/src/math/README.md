# `src/math/`: `std::math`

## What this subtree is

`use std::math` binds `math`: rounding, `abs`, `min`, `max`, `clamp`, `pow` and `sqrt`, logarithms and `exp`, the trigonometric functions, `is_nan` and `is_inf`, and the constants `pi`, `e`, `tau`, `inf` and `nan`.

This directory holds only names. Nothing here runs: the code each function runs is support code the backend writes into the program that calls it. Until v1.4.8 this directory also held the bytecode VM's half of the package, and until 2.0.6 a function that registered the package's name with the per-file pass, which now binds the name itself.

## Why it was built

Each std package is a flat top-level module, so the registry in `src/builtins/` can list every package in one table, and the checker, MIR and the backend can each find a package's functions by name. Most functions are one LLVM intrinsic for the operand kinds the checker settled; `round` takes halves away from zero, as v1 did.

## What each file does

- *`mod.rs`* holds `MATH_PKG`, the `Package` entry that names the package's import path, the global it binds, and its functions.

## Who uses it

*Depends on:* `builtins::Package`, the shape of its entry.

*Used by:*

- [`src/builtins/`](../builtins/README.md) lists `MATH_PKG` in the package table, which the per-file pass in `typechecker/resolve.rs` reads to bind the `math` name where a `use` names the package and to refuse a function the package does not have.
- `typechecker/infer/builtins.rs` gives each call its static type, and `mir/packages.rs` lowers it to a MIR operation.
- `src/backend/math.rs` emits the code, into a program only when that program uses it.
