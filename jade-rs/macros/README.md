# jade-rs-macros: the `#[jade::export]` attribute

## What this subtree is

The proc macro behind `#[jade::export]`. It reads the function or struct it is on and writes, beside it, what a Jade program needs to call it: an `extern "C"` wrapper, a function that frees what the wrapper returned, a `#[repr(C)]` mirror for a struct, and a description record. Packages never depend on this crate directly; [`jade-rs`](../README.md), the directory above, re-exports the macro.

## Why it was built

A proc-macro crate can export macros and nothing else, and the code the macro writes calls traits and functions that have to live in an ordinary crate. So the macro is here and its runtime is in `jade-rs`. The reasons for each part of the expansion are in [`design/v2-rust-packages.md`](../../design/v2-rust-packages.md).

## What each file does

- *`src/lib.rs`* is the attribute's entry point. It reads the crate's name from `CARGO_CRATE_NAME`, which the symbols carry, parses the optional `crate = "name"` argument, and hands the item to `func.rs` or `record.rs`. `expand` works on `proc_macro2` tokens so tests can call it.
- *`src/func.rs`* expands a function. It refuses what no wrapper can call, naming the fix, and decides for each parameter whether it is lifted as its own type or lifted into a value the wrapper owns and lent as `&T`. Then it writes the wrapper, the `__free` companion and the record.
- *`src/record.rs`* expands a struct into the mirror and the `Type`, `Elem`, `Lift` and `Lower` impls, and holds `emit`, which writes the `#[used]` static that puts a record in the `jade_meta` section.
- *`src/types.rs`* spells a type the two ways the wrapper needs: with every lifetime `'static`, to name a C form, and with named lifetimes `'_`, to lift a value whose borrow rustc infers.
- *`src/tests.rs`* checks every refusal for the fix it names, and the shape of an expansion.

## Who uses it

*Used by:* `jade-rs`, which re-exports it.

*Depends on:* `syn`, `quote` and `proc-macro2`.

## Gotchas

*The generated code writes `#[unsafe(no_mangle)]` and `unsafe(link_section = ...)`*, which edition 2024 requires and Rust accepts in every edition from 1.82. That is why both crates set `rust-version = "1.82"`.

*Tokens from `quote!` take the package's edition*, not this crate's, so anything the expansion writes must compile under 2015 through 2024 rules. `examples/ffi/rust_values/values/` is edition 2024 and the crate's own tests are 2021, which covers both ends that matter.

## Building and testing

```sh
cargo test -p jade-rs-macros
```
