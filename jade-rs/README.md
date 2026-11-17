# jade-rs: writing Jade packages in Rust

## What this subtree is

This is the crate a Rust library depends on to be called from a Jade program. It gives `#[jade::export]`, which goes on a function or a struct, and everything the code that attribute writes calls at run time. It is two crates in one directory, both members of the repo's workspace: `jade-rs` here, whose library is named `jade`, and `jade-rs-macros` in [`macros/`](macros/README.md), the proc macro, which this crate re-exports. A package depends on this crate alone.

```toml
# the package's Cargo.toml
[dependencies]
jade-rs = { git = "https://github.com/joericks1998/jade", tag = "v2.0.5" }
```

```rust
#[jade::export]
pub struct Point { pub x: f64, pub y: f64 }

#[jade::export]
pub fn midpoint(a: &Point, b: &Point) -> Point {
    Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }
}

#[jade::export]
pub fn parse_port(s: &str) -> Result<u16, String> {
    s.parse::<u16>().map_err(|e| e.to_string())
}
```

```toml
# the Jade project's jade.toml
[dependencies.geo]
path = "rust/geo"
abi = "rust"
```

```jade
use geo
let m = geo.midpoint(geo.Point { x: 0.0, y: 0.0 }, geo.Point { x: 2.0, y: 4.0 })
print(m.y)     // 2.0
```

The library is named `jade` so a plain `jade-rs = ...` dependency gives `#[jade::export]` with no rename. A package that does rename it writes `#[jade::export(crate = "the_name")]`.

## Why it was built

Until jade 2.0.5 a native package was a C library behind a generated C shim, typed from tables in `jade.toml` that were in turn generated from the library's header. Most of that machinery recovered facts C does not keep, such as who frees a string or how wide an integer is. Rust keeps them in its types, so the bridge can be written from the function's own signature, by the Rust compiler. [`design/v2-rust-packages.md`](../design/v2-rust-packages.md) has the whole design and the reasons for each choice.

## How it works

*The boundary is C's calling convention.* Rust has no stable ABI, so for each exported function the macro writes a `#[no_mangle] extern "C"` wrapper named `__jade_<crate>_<function>`, which takes a pointer to each argument's C form and a pointer to write the result's C form to, and returns a status: 0, or 1 with a message. A second function, `__jade_<crate>_<function>__free`, releases what the call handed over. For each exported struct the macro writes a `#[repr(C)]` mirror, which rustc lays out by C's rules.

*Ownership never crosses.* An argument is the Jade program's, lent for the call: a string argument points at the Jade string's own bytes. A result is Rust's: the program copies it into Jade values and calls the `__free` companion, which drops it with Rust's allocator.

*Nothing unwinds into Jade.* Every call runs inside `catch_unwind`. An `Err` raises its text in the program, a panic raises `<crate>.<function> panicked: <message>`, and the panic hook stays quiet for a panic that is caught this way.

*The description is in the library.* Each export adds a record, `JADEMETA`, a length and JSON, to a section of the built library, `__DATA,__jade_meta` on macOS and `.jade_meta` elsewhere. The JSON is built at compile time from each type's `Type::JADE` constant, so the Jade compiler reads what rustc compiled, aliases resolved, rather than parsing source.

*The types that cross:*

| Rust | Jade |
|------|------|
| `i64`, `isize` | `int` |
| `f64` | `float` |
| `bool`, `char` | `bool`, `char` |
| `u8`, `i16`, `u16`, `i32`, `u32`, `u64` / `usize`, `f32` | `byte`, `short`, `ushort`, `int32`, `uint32`, `ulong`, `float32` |
| `&str`, `String` | `str` |
| `&[u8]`, `Vec<u8>` | `bytes` |
| `Vec<T>`, `&[T]` | `[T]` |
| `Option<T>` | `T?` |
| an exported struct | a class with public fields |
| `Result<T, E>`, as the whole result | `T`, raising `E`'s text |
| `()` | `nil` |

Anything else stops the package's build with a message listing these. So do a generic function, an `async fn`, an `unsafe fn`, a `&mut` argument, a method, `impl Trait`, and a struct with a private field, each with the fix named.

## What each file does

- *`src/lib.rs`* re-exports the macro, documents the crate, and gathers what the generated code calls under `__private`, which is not an API.
- *`src/c.rs`* holds the two C forms that are not scalars: `CVec<T>`, a pointer, length and capacity, for a string, bytes or array; and `COpt<T>`, a flag and a value, for an optional. The Jade backend builds and reads the same structs.
- *`src/convert.rs`* holds the traits, named after UniFFI's: `Type` gives a type's C form and Jade spelling, `Lift` reads an argument, `Lower` hands a result over and frees it, and `Return` lets a `Result` raise. It implements them for every type in the table above. `Elem` marks the types that may sit in a `Vec`, which is every one but `u8`, since a `Vec<u8>` is `bytes`. `Borrowed` names what the wrapper owns for a `&T` argument.
- *`src/meta.rs`* holds `Meta`, which builds a description at compile time with `const fn`s, and `FORMAT`, the record format the compiler checks.
- *`src/call.rs`* holds `call`, which every wrapper runs inside, `free`, which every companion runs, and the quiet panic hook.
- *`src/tests.rs`* exports functions with the real macro and calls their wrappers the way a Jade program does.

## Who uses it

*Used by:* every Rust package a Jade program depends on, such as [`examples/ffi/rust_values/values/`](../examples/ffi/rust_values/values/src/lib.rs) and [`examples/ffi/rust_zlib/zlib/`](../examples/ffi/rust_zlib/zlib/src/lib.rs). The compiler never links this crate: it reads the records it produces, in [`compiler/src/rustpkg/`](../compiler/src/rustpkg/README.md), and calls the wrappers from the code [`compiler/src/codegen/rustpkg.rs`](../compiler/src/codegen/rustpkg.rs) generates.

*Depends on:* `jade-rs-macros`, and nothing else.

## Gotchas

*A change to a C form or a type's spelling is a format change.* Bump `meta::FORMAT`, and the compiler's matching constant in `compiler/src/rustpkg/meta.rs`, together; the compiler refuses a record of a format it does not read.

*Exported names are one namespace per crate*, as a Jade package's are. Two exported functions with the same name in different modules collide, and rustc names the duplicate symbol.

*An exported struct cannot implement `Drop`*, because lowering moves its fields out. A plain struct does not need to.

## Building and testing

```sh
cargo test -p jade-rs -p jade-rs-macros
```
