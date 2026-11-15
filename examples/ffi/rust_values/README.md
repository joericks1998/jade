# `examples/ffi/rust_values/`: every kind of value, across

## What this subtree is

A project whose `rust_values.jde` calls the Rust package in [`values/`](values/README.md), one function for each kind of value that crosses between Rust and Jade, and prints what comes back; `rust_values.expected` is that output. The `*_error.jde` files beside it are programs the checker refuses, each with the message in its opening comment.

## Why it exists

It is the fixture for decision 4 of [`design/v2-rust-packages.md`](../../../design/v2-rust-packages.md), the types that cross, and for decision 9, errors and panics. Every C form the backend builds and reads is exercised here at least once, in both directions where the type allows.

## What each file does

- *`jade.toml`* names `values/` as a Rust dependency.
- *`rust_values.jde`* and *`rust_values.expected`*: the calls and their output.
- *`unknown_function_error.jde`*, *`argument_type_error.jde`*, *`arity_error.jde`*, *`struct_field_error.jde`* and *`sized_argument_error.jde`*: the refusals.

## Who uses it

The examples check and `golden.sh`, and `compiler/src/rustpkg/tests.rs`, which builds this project's package for real.
