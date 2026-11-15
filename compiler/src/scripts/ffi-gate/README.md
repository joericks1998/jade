# `src/scripts/ffi-gate/`: the FFI gate's package and programs

## What this subtree is

The Rust package and the two Jade programs `../ffi-gate.sh` builds. The script writes a project in a scratch directory whose `jade.toml` names this directory as a Rust dependency, so nothing here is built by `cargo build` or by the examples check.

## Why it was built

The gate exists to run code nobody here wrote through the whole Rust package path: cargo fetching a crate graph from crates.io, `#[jade::export]` bridging it, and LLD linking a staticlib that imports a system library. `regex` is the large, ordinary crate, and `libz-sys` with its default features links the machine's own zlib rather than carrying one, which is the case the examples, whose zlib is built in, do not reach. `../README.md` says what each check catches.

## What each file does

- *`Cargo.toml`* names the package `gate` and its dependencies: `jade-rs` by path, `regex`, and `libz-sys`.
- *`src/lib.rs`* exports the functions: `find_all`, `is_match` and `replace_all` over `regex`, `deflate`, `inflate` and `zlib_version` over the system zlib, and `echo` and `words`, which the long run calls to show that what Rust returns is freed.
- *`gate.jde`* calls each function once, including a pattern `regex` refuses and data zlib refuses, each caught as a raise.
- *`gate.expected`* is what `gate.jde` prints, which the gate compares.
- *`loop.jde`* calls `echo` and `words` `JADE_FFI_LOOP_ITERS` times, 100,000 by default, comparing every answer, and prints the count and how many were wrong.

## Who uses it

*Used by:* `../ffi-gate.sh`, which CI runs on every pull request.

*Depends on:* [`jade-rs`](../../../../jade-rs/README.md), cargo, the network for the first fetch, and on Linux the zlib development package, which `libz-sys` finds with pkg-config.
