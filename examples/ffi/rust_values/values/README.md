# `values`: a Rust package exporting every kind of value

## What this subtree is

A small Rust crate, edition 2024, that depends on [`jade-rs`](../../../../jade-rs/README.md) by path and exports, with `#[jade::export]`, one function for each kind of value that crosses to Jade: `i64`, `bool`, `char`, every sized number type, `f32`, `&str` and `String`, a borrowed `&str` result, `&[u8]` and `Vec<u8>`, `&[String]` and `Vec<usize>`, the struct `Point` by reference and by value, `Option`, `Result`, and a function that panics.

## Why it exists

It is what `../rust_values.jde` calls. Edition 2024 is on purpose: the code the macro writes takes the package's edition, and 2024 is the one that requires `#[unsafe(no_mangle)]` and `unsafe(link_section)`.

## What each file does

- *`Cargo.toml`* names the crate and its one dependency.
- *`src/lib.rs`* holds the exported struct and functions.

## Who uses it

`examples/ffi/rust_values/`, whose `jade.toml` names it, and `compiler/src/rustpkg/tests.rs`.
