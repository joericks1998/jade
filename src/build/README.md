# `src/build/` — the native compilation entry point

## What this subtree is

A thin layer between the CLI and the LLVM backend. It takes a `TProgram`, decides what artifact to produce, and calls into `src/aot/`.

## Why it exists separately

It is the residue of a much larger module. This used to be a *client* for a build daemon listening on `$HOME/.jade/build.sock`, because code generation and the C runtime lived in a separate repository and keeping LLVM out of the `jade` binary seemed worth a socket. Both halves of that argument are gone — `src/aot/` owns the backend and the C runtime now — so the daemon's only remaining job was forwarding a request to a function this crate already exported.

What is left is the `Emit` decision and the plumbing around it. It stays a separate module because "what artifact do we want" is a different question from "how do we lower an opcode," and keeping the CLI out of `aot/` internals means the backend can be restructured without touching command wiring.

## What each file does

- **`mod.rs`** — the `Emit` enum (`Binary`, `Ir`, `CDylib { exports }`), its conversion into `aot::CompileMode`, and the function that drives compilation.
- **`tests.rs`** — tests for the emit-mode plumbing.

## Who uses it

*Depends on:* `compiler/tir` for the `TProgram` and `aot/` for the actual lowering.

*Used by:* `cli/build.rs`, which implements `jade build`.

## Gotchas

Building this module — and therefore the whole toolchain — requires LLVM 18, because `aot/` links it in. Set `LLVM_SYS_180_PREFIX` if you are not on Apple Silicon Homebrew (`.cargo/config.toml` has that path as a default, and Cargo's `[env]` does not overwrite a variable you have already set).

`Emit::Ir` prints IR rather than linking, so it uses the same `CompileMode::Binary` entry-point shape as a real binary — the mode only decides which entry point gets generated.
