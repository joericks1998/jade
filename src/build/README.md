# `src/build/`: the native compilation entry point

## What this subtree is

This is a thin layer between the CLI and the LLVM backend. It takes a `TProgram`, decides which artifact to produce, and calls into `src/aot/`.

## Why it exists separately

It is what is left of a much larger module. This used to be a *client* for a build daemon listening on `$HOME/.jade/build.sock`. Code generation and the C runtime lived in a separate repository then, and keeping LLVM out of the `jade` binary seemed worth a socket. Both halves of that argument are gone, because `src/aot/` now owns the backend and the C runtime. The daemon's only remaining job was forwarding a request to a function this crate already exported.

What is left is the `Emit` decision and the plumbing around it. It stays a separate module because "what artifact do we want" is a different question from "how do we lower an opcode". Keeping the CLI out of `aot/` internals also means the backend can be restructured without touching command wiring.

## What each file does

- *`mod.rs`* holds the `Emit` enum, whose variants are `Binary`, `Ir`, and `CDylib { exports }`, its conversion into `aot::CompileMode`, and the function that drives compilation.
- *`tests.rs`* holds tests for the emit-mode plumbing.

## Who uses it

*Depends on:* `compiler/tir` for the `TProgram` and `aot/` for the actual lowering.

*Used by:* `cli/build.rs`, which implements `jade build`.

## Gotchas

Building this module, and therefore the whole toolchain, requires LLVM 18, because `aot/` links it in. Set `LLVM_SYS_180_PREFIX` if you are not on Apple Silicon Homebrew. `.cargo/config.toml` carries that path as a default, and Cargo's `[env]` section does not overwrite a variable you have already set.

`Emit::Ir` prints IR rather than linking, so it uses the same `CompileMode::Binary` entry-point shape a real binary uses. The mode only decides which entry point gets generated.
