# `src/build/`: the native compilation entry point

## What this subtree is

This is a thin layer between the CLI and the driver. It takes a `TProgram`, asks `src/aot/` to compile it, and prints the IR or MIR when that is what was asked for.

## Why it exists separately

It is what is left of a much larger module. This used to be a *client* for a build daemon listening on `$HOME/.jade/build.sock`. Code generation lived in a separate repository then, and keeping LLVM out of the `jade` binary seemed worth a socket. That argument is gone, because `src/aot/` and `src/backend/` now own the driver and the code generation. The daemon's only remaining job was forwarding a request to a function this crate already exported.

It stays a separate module so the CLI never reaches into `aot/` internals, and the driver can be restructured without touching command wiring.

## What each file does

- *`mod.rs`* re-exports `aot::Emit`, whose variants are `Binary`, `Ir` and `Mir`, and holds `build`, which compiles and writes IR or MIR to standard output.
- *`tests.rs`* holds tests for building a binary, printing IR, and a missing source file.

## Who uses it

*Depends on:* `typechecker/tir` for the `TProgram` and `aot/` for the compile.

*Used by:* `cli/build.rs`, which implements `jade build` and the builds behind `jade run` and `jade test`.

## Gotchas

Building this module, and therefore the whole toolchain, requires LLVM 18 and LLD, because `backend/` links both in. Set `LLVM_SYS_180_PREFIX` if you are not on Apple Silicon Homebrew. `.cargo/config.toml` carries that path as a default, and Cargo's `[env]` section does not overwrite a variable you have already set.
