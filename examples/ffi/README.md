# `examples/ffi/`: native code from a Jade program

## What this subtree is

The fixtures for what crosses between a Jade program and native code. Since 2.0.4 native code is a Rust package: a crate that marks functions and structs with `#[jade::export]`, named in `jade.toml` with `abi = "rust"`, and built by cargo into the program. A C library is reached through a Rust crate that wraps it. [`design/v2-rust-packages.md`](../../design/v2-rust-packages.md) has the design.

Each Rust fixture is a project of its own, with a `jade.toml`, because only a project has dependencies.

## What each directory holds

- *`char_across/`* converts between `char` and its code point, which reading a fixed-size C field once needed. It uses no package.
- *`rust_values/`* calls the crate in `values/`, which exports one function for each kind of value that crosses: numbers of every width, `bool`, `char`, text, bytes, arrays, a struct, an optional, a `Result` whose error is caught, and a panic, which raises naming the function. Its `*_error.jde` files pin what the checker refuses: a name the package does not export, with the closest one suggested; an argument of the wrong type; the wrong number of arguments; a struct literal missing a field; and a literal too large for a sized parameter.
- *`rust_zlib/`* calls the crate in `zlib/`, which wraps zlib through `libz-sys`, built in from source. The crate is named `zlib-jade`, which a program cannot write, so `jade.toml` names the dependency `zlib` and gives the crate's name as `package`.
- *`rust_build_failure/`* holds `badpkg/`, a crate that exports a `HashMap`, which has no Jade form, so it does not compile; checking `rust_build_error.jde` reports rustc's message, which lists the types that cross.
- *`rust_removed_abi/`* names a dependency with `abi = "c"`, removed in 2.0.4; reading its manifest stops on that line, naming the replacement.
- *`rust_missing_abi/`* names a dependency with no `abi`, which there is no default for.

## Who uses it

*Used by:* the examples check in `cargo test` and CI, which runs `jade check` over every `.jde` file, and `compiler/src/scripts/golden.sh`, which builds and runs each one that is not an `_error` fixture. Checking a Rust fixture builds its package with cargo, since the signatures come from the built library; the crates are kept in `~/.jade/rust/`.

*Depends on:* [`jade-rs`](../../jade-rs/README.md), which each crate here names by path, and cargo.
