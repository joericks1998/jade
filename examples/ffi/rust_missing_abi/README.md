# `examples/ffi/rust_missing_abi/`: a dependency that does not say what it is

## What this subtree is

A project whose `jade.toml` names a dependency with no `abi`. A dependency is Jade source or a Rust crate, and neither is a sensible default, so checking `missing_abi_error.jde` stops, naming the two spellings.

## Why it exists

Until 2.0.4 an entry with no `abi` meant a prebuilt Jade-ABI library, which no longer exists. The fixture pins that such an entry is refused with the fix rather than read as one kind or the other.

## Who uses it

The examples check, which expects the `_error` fixture to fail.
