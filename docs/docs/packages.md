---
id: packages
title: Packages
sidebar_label: Packages
---

A Jade project depends on two kinds of package, both built from source with the program that uses them:

- a *Jade source package*, a directory, git repository or `.tar.gz` archive of `.jde` files with a `[package]` section, compiled into each program that uses it;
- a *Rust package*, a crate that marks what Jade may call with `#[jade::export]`, built by cargo and linked into the program. A C library is reached this way too, through a Rust crate that wraps it.

You declare a dependency in `jade.toml`, and every program in the project can `use` it by name.

```sh
jade pkg add geometry --path ../geometry        # a directory with a jade.toml
jade pkg add imgtools --path rust/imgtools      # a directory with a Cargo.toml
```

```jade
use geometry
use imgtools

print(geometry.area(3, 4))
print(imgtools.greet("Jade"))
```

The built program needs nothing beside it. A source package is compiled into it, and a Rust package is linked into it, so the binary can be copied to another machine on its own.

## There is no registry

A Jade source package names *where it lives*, a path, a git repository or an archive URL, rather than an entry in a central index. That is deliberate.

*No version ranges.* With no index to resolve `^1.2` against, a dependency names one exact version. Writing `version = "^1.2"` is rejected when the file is parsed, rather than quietly treated as a literal string.

*`jade pkg update` reconciles, it does not discover.* It resolves the lock against the manifest again. To move to a new version, edit `jade.toml`, or run `jade pkg add` again.

A Rust package can also come from crates.io by an exact `version`, since cargo has a registry of its own.

## Commands

| Command | What it does |
|---------|--------------|
| `jade pkg add <name> --path <dir>` | Add a local package: Jade source when the directory has a `jade.toml`, Rust when it has a `Cargo.toml` |
| `jade pkg add <name> --git <url> --tag <t>` | Add a Jade source package from git; with `--rust`, a Rust package |
| `jade pkg add <name> --url <archive.tar.gz> --version <v>` | Add a Jade source package from an archive |
| `jade pkg add <name> --rust --version <v>` | Add a Rust package from crates.io |
| `jade pkg install` | Fetch and verify every source package `jade.lock` pins |
| `jade pkg install --locked` | Same, but fail rather than update the lock (use in CI) |
| `jade pkg update [name]` | Re-resolve against `jade.toml` and rewrite the lock |
| `jade pkg remove <name>` | Drop it from the manifest, the lock, and `libs/` |
| `jade pkg list` | Show what is locked and installed here, and the Rust packages |

:::warning The package commands are nested
Every one of them is `jade pkg <something>`. There is no bare `jade add`, `jade install`, or `jade update`.
:::

`jade pkg update` manages *dependencies*. `jade upgrade` updates the *jade toolchain itself*. The two are unrelated. See the [CLI reference](cli#jade-upgrade).

## Jade source packages

```toml
[dependencies]
geometry = { path = "../geometry", abi = "source" }
shapes   = { git = "https://github.com/me/shapes", tag = "v1.2.0", abi = "source" }
vectors  = { url = "https://example.com/vectors-1.0.tar.gz", version = "1.0", abi = "source" }
```

A path package is read where it is. A git or archive package is unpacked into `libs/<name>-<version>/`. `jade.lock` pins a hash of the package's `jade.toml` and every `.jde` file, so a README edit does not move a pin, and an install checks the unpacked tree rather than the bytes it came in.

A source package is a project with a `[package]` section:

```toml
[package]
name    = "geometry"
entry   = "geometry.jde"                      # optional; defaults to <name>.jde
exports = ["area", "Rect"]                    # optional; defaults to every top-level name
```

A program that imports it can use what `exports` lists and nothing else. A source package's own dependencies are added to the project that adds it, so a package with a Rust dependency brings that dependency along.

### Local `path` dependencies

A `path` package is the one source that can legitimately change while the lock stays correct, so Jade re-hashes it on every install and every run, and re-pins the lock when it has changed:

```
$ jade run main.jde
note: re-pinned geometry (local source changed)
```

Under `--locked`, that same drift is an error rather than a fix, because a changed source means the committed lock is stale.

### Committing

Commit `jade.lock`. Do not commit `libs/`, which `jade new` already adds to `.gitignore`.

## Rust packages

A Rust package is a library crate that depends on `jade-rs` and marks what Jade may call:

```toml
# rust/imgtools/Cargo.toml
[package]
name = "imgtools"
version = "0.1.0"
edition = "2021"

[dependencies]
jade-rs = { git = "https://github.com/joericks1998/jade", tag = "v2.0.5" }
```

```rust
// rust/imgtools/src/lib.rs
#[jade::export]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[jade::export]
pub fn midpoint(a: &Point, b: &Point) -> Point {
    Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }
}

#[jade::export]
pub fn parse_port(s: &str) -> Result<u16, String> {
    s.parse::<u16>().map_err(|e| format!("'{s}' is not a port: {e}"))
}
```

```toml
# jade.toml
[dependencies.imgtools]
path = "rust/imgtools"
abi = "rust"
```

```jade
use imgtools

let m = imgtools.midpoint(imgtools.Point { x: 0.0, y: 0.0 }, imgtools.Point { x: 2.0, y: 4.0 })
print(m.y)                              // 2.0
try {
    imgtools.parse_port("eighty")
} catch e {
    print(e.message)                    // 'eighty' is not a port: invalid digit found in string
}
```

A Rust package comes from a `path` holding its `Cargo.toml`, a `git` repository with a `rev`, `tag` or `branch`, or a crates.io `version`. Its name in `jade.toml` is the name a program writes, so it is an identifier; when the crate's own name has a `-` or differs, give it as `package`:

```toml
[dependencies.zlib]
path = "rust/zlib-jade"
package = "zlib-jade"
abi = "rust"
```

### The types that cross

| Rust | Jade |
|------|------|
| `i64`, `isize` | `int` |
| `f64` | `float` |
| `bool`, `char` | `bool`, `char` |
| `u8`, `i16`, `u16`, `i32`, `u32`, `u64` and `usize`, `f32` | `byte`, `short`, `ushort`, `int32`, `uint32`, `ulong`, `float32` |
| `&str`, `String` | `str` |
| `&[u8]`, `Vec<u8>` | `bytes` |
| `Vec<T>`, `&[T]` | `[T]` |
| `Option<T>` | `T?` |
| a `#[jade::export]` struct | a class with public fields, `pkg.Point` |
| `Result<T, E>`, as the whole result | `T`, raising `E`'s text |
| `()` | `nil` |

A number literal takes a sized parameter's type, as it does in an assignment: `pkg.f(1)` passes the byte 1 to a `u8` parameter. An exported struct's fields must all be `pub`, and a Jade literal of its class gives every one.

The crate does not compile when it exports anything else, and rustc's message lists these types. So does a generic function, an `async fn`, an `unsafe fn`, a method, `impl Trait`, or a `&mut` argument, each with the fix named.

### Errors and panics

An `Err` raises its text in the program. A panic raises too, naming the function, `imgtools.explode panicked: <message>`, and never unwinds into the program. Both are caught with `try` and `catch` like any other error.

### What building one needs

Building a program that uses a Rust package runs cargo, which fetches the package's crates the first time and keeps what it built in `~/.jade/rust/`. Jade runs cargo from `PATH`; if none is there, the build says so. On macOS it also needs Apple's Command Line Tools, which cargo itself needs to build any crate with a build script or proc macro.

`jade check` builds the Rust too, because the signatures it checks a program against are in the built library. After the first build, a check asks cargo and is done in a moment.

### Reaching a C library

A C library is reached through a Rust crate that wraps it. An existing `-sys` crate builds or finds the library and declares its functions, and a few lines of safe Rust export what Jade needs:

```rust
use libz_sys as z;

#[jade::export]
pub fn crc32(data: &[u8]) -> u32 {
    unsafe { z::crc32(0, data.as_ptr(), data.len() as z::uInt) as u32 }
}
```

Jade 2.0.4 and earlier bound C libraries directly, with symbol tables in `jade.toml` generated from the library's header. `abi = "c"` is now refused with a message pointing here.

### Not yet

- *Opaque values,* such as a connection Jade holds and Rust keeps the inside of. Return the data instead, for now.
- *Callbacks,* a Jade function passed to Rust.
- *Iterators.* Return a `Vec`; iterators will connect to Jade's pipes.
- *Dicts, enums, tuples and `impl` blocks.*
