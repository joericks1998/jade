# `examples/ffi/rust_build_failure/`: a package that does not compile

## What this subtree is

A project whose Rust package, [`badpkg/`](badpkg/README.md), exports a function returning a `HashMap`, which has no Jade form. Checking `rust_build_error.jde` builds the package, which fails, and the check reports rustc's message: `HashMap<String, i64>` cannot be returned to Jade, with a note listing the types that do cross.

## Why it exists

A Rust package's mistakes are found by rustc, from the trait bounds the code `#[jade::export]` writes, and `#[diagnostic::on_unimplemented]` on those traits turns them into a message naming the fix. This pins that the message reaches the person running `jade check`, without cargo's progress lines around it.

## What each file does

- *`jade.toml`* names `badpkg/` as a Rust dependency.
- *`rust_build_error.jde`* uses it, and must fail the check.

## Who uses it

The examples check, which expects the `_error` fixture to fail.
