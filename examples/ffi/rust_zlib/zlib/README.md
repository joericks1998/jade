# `zlib-jade`: zlib for Jade

## What this subtree is

A Rust crate that wraps zlib through [`libz-sys`](https://crates.io/crates/libz-sys) and exports four functions with `#[jade::export]`: `compress`, `decompress`, `crc32` and `version`. A zlib status other than `Z_OK` comes back as an `Err`, which a Jade program catches as a raise.

## Why it exists

It is what `../rust_zlib.jde` calls, and the pattern for reaching any C library from Jade. `libz-sys` is used with its `static` feature, which builds zlib from the source it carries and links it into the crate, so the program needs no zlib where it runs. (`compiler/src/scripts/ffi-gate/` uses the other case, the system's zlib.)

## What each file does

- *`Cargo.toml`* names the crate `zlib-jade` and its dependencies, `jade-rs` by path and `libz-sys`.
- *`src/lib.rs`* holds the four functions, each a safe wrapper around zlib's C calls.

## Who uses it

`examples/ffi/rust_zlib/`, whose `jade.toml` names it.
