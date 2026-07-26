# `src/pkg/` — the package manager

## What this subtree is

The machinery behind `jade add` / `remove` / `install` / `update` / `list`. It turns `[dependencies]` in `jade.toml` into a pinned `jade.lock` and a populated `libs/` directory.

```
jade.toml [dependencies] → jade.lock → libs/
```

## Why it was built this way

Dependencies are **prebuilt native shared libraries**, sourced from a local path or a URL. There is deliberately no package registry — like Go, a dependency names where it lives rather than an entry in a central index.

That choice has a consequence worth stating plainly: a `.so` carries no manifest of its own, so **there is no transitive resolution and no version solving**. Each dependency contributes exactly one artifact, `jade.lock` is a flat list, and "resolution" means picking the right platform build. A package that needs another package must say so in its documentation; Jade cannot discover it.

The second decision is that the lock records an artifact **for every platform**, not just the current one. Unlike Cargo, which locks portable source, a lock naming one artifact would only be valid on the machine that generated it — a macOS developer would commit a lock that Linux CI could not install, and with no registry to ask, could not even *verify*.

The integration surface with the rest of the compiler is one function, `dependency_libraries`. Resolved dependencies come back as synthetic `project::LibraryEntry` values and get unioned into the manifest's `[lib]` map, so neither the VM nor the AOT import resolver ever learns what a dependency is. Both keep resolving `[lib]` entries exactly as before, which is how the two backends are kept from drifting on imports.

## What each file does

- **`mod.rs`** — resolution and materialization. `dependency_libraries` is the one public seam into the rest of the compiler. Also defines `LIBS_DIR` (`libs/`, gitignored — `jade.lock` is what travels) and `ANY_PLATFORM`, the artifact key for a URL with no `{platform}` placeholder.
- **`lock.rs`** — the `jade.lock` format: `LockedPackage`, `LockedArtifact`, digests per platform. Meant to be committed.
- **`fetch.rs`** — artifact acquisition and integrity, behind a `Fetcher` trait. Everything in this module takes a `&dyn Fetcher` rather than calling out directly, so the whole package manager is testable offline against a map of canned responses.
- **`manifest.rs`** — format-preserving edits to `jade.toml` via `toml_edit`. `jade add` and `jade remove` rewrite a file a person wrote by hand; a parse-and-reserialize round-trip would silently discard every comment and all the original layout.
- **`cshim.rs`** — generates a Jade-ABI binding shim for a plain C library. The loader requires a `jade_pkg_init` symbol, which something like `libz` does not have. Rather than teach the runtime to dispatch arbitrary C signatures — a `libffi` dependency and *two* marshalling implementations, one per engine — this emits a small C wrapper and compiles it with `cc`. The result is an ordinary Jade-ABI package both backends already know how to load. The type vocabulary is exactly the FFI's: `int`, `float`, `bool`, `str`, and `nil` for returns.
- **`tests.rs`** — package manager tests, all offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`.

*Used by:* `cli/pkg.rs` for the commands. Indirectly, `vm/chunk.rs` and `aot/imports.rs` consume the `[lib]` entries this module contributes, without knowing they came from a dependency.

## Gotchas

`cshim.rs` cannot bind a C function that takes a struct pointer. That is a real limit, but it is the same limit the ABI imposes on any Jade package, not one this shim invents.

Tests must never hit the network — use the `Fetcher` trait.

## Building and testing

```sh
cargo test pkg::
```
