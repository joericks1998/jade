# `examples/ffi/rust_zlib/`: a C library, through a Rust crate

## What this subtree is

A project whose `rust_zlib.jde` compresses, decompresses and checksums bytes with zlib, a C library, through the Rust crate in [`zlib/`](zlib/README.md). `rust_zlib.expected` is its output, including two zlib errors caught as raises.

## Why it exists

Rust packages replaced Jade's C foreign function interface, and this is the shape that replaces binding a C library directly: a `-sys` crate builds or finds the library and declares its functions, and a few lines of safe Rust export what Jade needs. There is no symbol table and no header in `jade.toml`.

The crate's name, `zlib-jade`, holds a `-`, which a Jade program cannot write, so `jade.toml` names the dependency `zlib` and gives the crate's name as `package`. That exercises the rename too.

## What each file does

- *`jade.toml`* names `zlib/` as a Rust dependency, with `package = "zlib-jade"`.
- *`rust_zlib.jde`* and *`rust_zlib.expected`*: the calls and their output.

## Who uses it

The examples check and `golden.sh`.
