# `examples/ffi/rust_removed_abi/`: a manifest from before 2.0.4

## What this subtree is

A project whose `jade.toml` names a dependency with `abi = "c"`, the direct C binding 2.0.4 removed. Checking `c_abi_error.jde` reads the manifest and stops on that line, naming the replacement: a Rust crate that wraps the library, with `abi = "rust"`.

## Why it exists

An upgrade from 2.0.3 meets this first, so the message is pinned where it is shown: on the manifest's own line, from `project::Abi`'s deserializer, rather than as an unknown variant.

## Who uses it

The examples check, which expects the `_error` fixture to fail.
