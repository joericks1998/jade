---
id: packages
title: Packages
sidebar_label: Packages
---

Jade projects depend on **prebuilt native shared libraries**. A dependency is declared in `jade.toml`, pinned in `jade.lock`, and installed into a project-local `libs/` directory.

```sh
jade pkg add fastmath --url 'https://example.com/fastmath-{platform}.so' --version 1.2.0
```

```jade
use fastmath

print(fastmath.triple(14))
```

## There is no registry

A dependency names **where it lives** — a URL or a local path — rather than an entry in a central index. That is a deliberate choice, and it has consequences worth knowing up front:

- **No transitive resolution.** A `.so` carries no manifest, so Jade cannot discover that one package needs another. `jade.lock` is a flat list. A package with its own dependencies must say so in its documentation.
- **No version ranges.** With no index to resolve `^1.2` against, a dependency names one exact version. `version = "^1.2"` is rejected at parse time rather than silently treated as a literal.
- **`jade update` reconciles, it does not discover.** It re-resolves the lock against the manifest and re-fetches. To move to a new version, edit `jade.toml` (or re-run `jade pkg add` with a new `--version`).

## Commands

| Command | Effect |
|---|---|
| `jade pkg add <name> --url <u> --version <v>` | Add a remote dependency and install it |
| `jade pkg add <name> --path <file>` | Add a local `.so`/`.dylib` |
| `jade pkg install` | Fetch and verify everything `jade.lock` pins |
| `jade pkg install --locked` | Same, but fail rather than update the lock (use in CI) |
| `jade pkg update [name]` | Re-resolve against `jade.toml` and rewrite the lock |
| `jade pkg remove <name>` | Drop it from the manifest, the lock, and `libs/` |
| `jade pkg list` | Show what is locked and whether it is installed here |

`jade run` and `jade test` install anything missing automatically, so a fresh clone needs no separate step.

:::note
`jade pkg update` manages **dependencies**. `jade upgrade` updates the **jade toolchain itself**. They are unrelated.
:::

## Platforms

A shared library is built for one OS and architecture, so a lockfile naming a single artifact would only be valid on the machine that wrote it. Instead, a `{platform}` URL is expanded across every supported platform when the lock is generated, and **all** of their checksums are recorded:

```toml
[[package]]
name = "fastmath"
version = "1.2.0"
source = "url+https://example.com/fastmath-{platform}.so"
abi = "jade"

[package.artifacts.darwin-aarch64]
url = "https://example.com/fastmath-darwin-aarch64.so"
file = "fastmath.so"
sha256 = "daf44949…"

[package.artifacts.linux-x86_64]
url = "https://example.com/fastmath-linux-x86_64.so"
file = "fastmath.so"
sha256 = "9c1f0a72…"
```

**Only the artifact matching your machine is ever downloaded.** The other entries are a few hundred bytes of text apiece, and they are what let a lock committed from a Mac be installed *and verified* on Linux CI — with no registry to ask for the Linux checksum at install time. A Homebrew formula's `bottle` block works the same way.

A package that ships for only some platforms is fine; the missing ones are simply absent from the lock, and installing on one of them fails with a message naming what *is* available.

Supported tags: `darwin-aarch64`, `darwin-x86_64`, `linux-aarch64`, `linux-x86_64`.

## Integrity

Every artifact is verified against its SHA-256 in `jade.lock` on **every** install, not only on first download. A `.so` is `dlopen`ed — that is arbitrary code execution before any Jade code runs — so an artifact that is merely *present* is not thereby trusted. A mismatch is refused and nothing is written to `libs/`.

Checksums live in the lock, not the manifest: `jade pkg add` computes them on first fetch, exactly as Cargo does.

## Committing

Commit `jade.lock`. Do not commit `libs/` — `jade new` adds it to `.gitignore`. The lock is what travels; the binaries are rebuilt from it.

## Using a plain C library

A library like `libz` exports no `jade_pkg_init`, so it cannot be loaded directly. Declare `abi = "c"` and the symbols to bind, and Jade generates and compiles a binding shim at install time:

```toml
[dependencies.plainc]
version = "1.0.0"
path    = "vendor/libplainc.dylib"
abi     = "c"

[dependencies.plainc.symbols.square]
args = ["int"]
ret  = "int"

[dependencies.plainc.symbols.half]
args = ["float"]
ret  = "float"
```

```jade
use plainc

print(plainc.square(9))    // 81
print(plainc.half(7.0))    // 3.5
```

This needs a C compiler (`cc`) on the machine running `jade pkg install`.

Argument and return types come from the FFI's vocabulary: `int`, `float`, `bool`, `str`, and `nil` for a return. A symbol using anything else is rejected **by name** at install time rather than silently marshalled to nil.

## Publishing a Jade package

`jade build --lib` compiles a Jade file to a shared library exporting `jade_pkg_init`:

```sh
jade build mathlib.jde --lib               # -> mathlib.dylib (or .so)
jade build mathlib.jde --lib --export add  # bind only `add`
```

Jade has no `pub` keyword — every top-level function is public — so the default is to export all of them. `--export` narrows that.

Publish the result wherever you like (GitHub Releases is the natural home), one build per platform, named so a `{platform}` URL finds them. Consumers then `jade pkg add` it like any other dependency.

## The FFI's limits

The native ABI carries `int`, `float`, `bool`, `str`, `nil`, and — since v1.1.31 — arrays, dicts, and structs, in both directions. A struct crosses with its type name attached, so the receiving side can tell a `Config` from anything else shaped like one.

What still does not cross: **functions and futures**, which arrive as `nil`. A package API cannot take a callback.

In practice, package APIs are scalar-and-string shaped. Widening the ABI is the natural next step for the package ecosystem, and it is the one change here that would be difficult to make after packages are widely published.
