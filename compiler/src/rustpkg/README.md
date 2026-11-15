# `src/rustpkg/`: Rust packages

## What this subtree is

This is how the compiler builds a project's Rust packages and learns what they export. A Rust package is a crate that depends on [`jade-rs`](../../../jade-rs/README.md) and marks functions and structs with `#[jade::export]`, named in `jade.toml` with `abi = "rust"`. `build` turns a project's Rust dependencies into a `Build`: the staticlib to link, the system libraries it needs, and for each package its functions' Jade signatures and its structs' fields.

```
jade.toml [dependencies] abi = "rust"
  → ~/.jade/rust/<hash>/{Cargo.toml, lib.rs}     the generated crate
  → cargo rustc --release                        one staticlib, every package in it
  → the jade_meta records in the staticlib       what each package exports
  → Build                                        signatures, classes, what to link
```

## Why it was built

Until 2.0.4 a native package was a C library behind a shim generated from its header, and its types came from tables in `jade.toml`. Rust keeps every fact the shim had to recover in its types, so `#[jade::export]` writes the bridge from the function's own signature, and a description of it into the built library. That description is this crate's header file: the compiler reads it rather than parsing source, so a type alias or a generated item is described as what rustc compiled. [`design/v2-rust-packages.md`](../../../design/v2-rust-packages.md) has the decisions and their reasons.

## How it works

*One generated crate per project.* `cargo.rs` writes a `staticlib` crate whose dependencies are every Rust package in `jade.toml` and whose `lib.rs` is one `extern crate` line per package, so rustc links each one in. One staticlib per program matters: two would each carry Rust's standard library, and their symbols would collide when LLD links the program. The crate lives at `~/.jade/rust/<first 8 bytes of a SHA-256 of the project's path>/`, named `jade_program_<hash>`, with an empty `[workspace]` so a workspace around it never claims it, and `panic = "unwind"`, which only the root crate's profile decides. Every project builds into `~/.jade/rust/target/`, so the crates packages share compile once per machine.

*Cargo builds it.* `cargo rustc --release --lib --message-format=json-render-diagnostics ... -- --print=native-static-libs`, from `CARGO` or `PATH`. The staticlib's path comes from cargo's `compiler-artifact` message; the system libraries come from rustc's `native-static-libs` note, which cargo prints again when the crate is already built. A failed build is passed on as rustc rendered it, without cargo's progress lines. A first build prints a note, since fetching and compiling the crates takes a while.

*The records come out of the archive.* `meta.rs` opens the staticlib with the `object` crate, and every member that is an object file, finds the `__jade_meta` or `.jade_meta` section, and splits it into records: `JADEMETA`, a little-endian `u32` length, and JSON, with any zero padding between them. A record names its format, `v`; one of another format is refused, naming the `jade-rs` to use.

*Structs are checked.* A struct's record carries rustc's size, alignment and field offsets for the `#[repr(C)]` mirror. `meta::check` lays the struct out by C's rules from its fields' Jade types, the way the backend will, and refuses a disagreement, naming the struct. It also refuses a function whose types have no C form, such as an `Option` of an `Option`.

*Packages are matched to crates by cargo.* A record carries the crate's name as rustc gives it, which a package may set apart from its own name. `cargo metadata --offline` on the built crate says which crate each dependency is.

## What each file does

- *`mod.rs`* is the entry: `build`, `dependencies`, `class_name` (`Point$rgeo` for the struct `Point` of crate `geo`), and the `Build`, `Package` and `Link` types.
- *`cargo.rs`* writes the generated crate, finds and runs cargo, and reads cargo's output: the staticlib, the `native-static-libs` note, its diagnostics, and `cargo metadata`'s map of dependency to crate.
- *`meta.rs`* reads the records out of a library and checks each struct's layout and each function's types.
- *`tests.rs`* tests each of those on its own, and builds `examples/ffi/rust_values` once for real.

## Who uses it

*Used by:* `aot/imports.rs`, which calls `build` the first time a program's `use` names a Rust package, turns each struct into a class and each function into a `NativeSig` (`typechecker/infer/native.rs`), and passes the `Link` on to MIR and the backend, where `backend/link.rs` links it. `cli/pkg.rs` calls `build` when `jade pkg add` adds a Rust package. `pkg::resolved_libraries` lists the packages with `dependencies`.

*Depends on:* `project/` for the manifest, the `object` crate for reading the archive, and cargo, which must be on `PATH` when a program uses a Rust package. Fetching a pinned toolchain instead is the first follow-up in the design.

## Gotchas

*`jade check` builds the Rust.* The records are the only place a signature comes from, so checking a program that uses a Rust package runs cargo. After the first build that is a fraction of a second.

*The record format is shared with `jade-rs`.* `meta::FORMAT` here and `FORMAT` in `jade-rs/src/meta.rs` move together, and so do the C forms in `jade-rs/src/c.rs` and the layout rules in `meta::c_layout`, which the backend's `c_ty` mirrors.

*Two threads may write one generated crate at once,* as two tests do. `write_if_changed` writes through a uniquely named temporary file and a rename, and skips a file whose contents have not changed, so cargo also sees an unchanged crate as fresh.

## Building and testing

```sh
cargo test rustpkg::
```
