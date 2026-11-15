# `badpkg`: a Rust package that does not compile

## What this subtree is

A Rust crate whose one exported function returns `HashMap<String, i64>`. No Jade type holds a `HashMap`, so the crate does not compile, on purpose.

## Why it exists

It is what `../rust_build_error.jde` uses, to show the message a user sees when a package exports a type that does not cross.

## What each file does

- *`Cargo.toml`* names the crate and its one dependency, `jade-rs` by path.
- *`src/lib.rs`* holds the function.

## Who uses it

`examples/ffi/rust_build_failure/`.
