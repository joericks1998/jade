# `src/cache/`: the on-disk compilation cache

## What this subtree is

This is a content-addressed store of parsed and type-checked programs. `jade run`, `jade check`, and `jade build` all hash the source file first, and skip straight to the cached AST or TIR when the hash matches. That is what makes repeated runs of an unchanged file fast.

## Why it works the way it does

The cache key is a *SHA-256 of the file's raw bytes*, not a modification time. Hashing the content survives git checkouts and NFS-mounted filesystems, where modification times lie routinely.

There are two version guards, and they are separate on purpose:

- `JADE_VERSION` is the crate version, baked in at compile time. It invalidates everything when a release changes.
- `CACHE_FORMAT_VERSION` is a separate counter for the *shape* of the serialized types. It exists so a format change during development invalidates caches without a version bump in `Cargo.toml`. Version bumps are not free here, because merging one to `main` ships a release.

A cache written before format versioning existed has no `format_version` field. It deserializes as 0, which always fails the check. That is intended.

## What each file does

- *`mod.rs`* holds the whole cache: `file_hash`, the AST pair `read_ast_cache` and `write_ast_cache`, the TIR pair `read_tir_cache` and `write_tir_cache`, the crate-internal `cache_root`, and the maintenance API `list_entries` and `purge_entries` that `jade cache` uses.
- *`tests.rs`* holds the cache tests.

## Who uses it

*Depends on:* `frontend::ast::Program` and `compiler::tir::TProgram`, both of which are serde-serializable, plus `sha2` and `bincode`.

*Used by:* `cli/run.rs`, `cli/check.rs`, and `cli/build.rs` on the fast path, plus `cli/cache.rs` and `cli/env.rs` for statistics and cleanup.

## Gotchas

*Bump `CACHE_FORMAT_VERSION` whenever you change a serialized shape.* Adding a field to any AST or TIR type means stale caches would deserialize into the wrong struct. A tripwire test pins the constant. When that test fails after your change, it is doing its job, not flaking. The constant's doc comment records what each past bump was for, so add a line when you bump it.

*Do not use `std::env::set_var` in these tests.* `cargo test` is heavily parallel, so setting an environment variable races against every other thread calling `getenv`. That is a real data race, and it is why `set_var` is `unsafe` as of the 2024 edition. It made the cache tests fail intermittently for a while. Inject a path instead, or use a `#[cfg(test)]` thread-local with an RAII guard, so cleanup survives a failing assertion.

## Building and testing

```sh
cargo test cache::
./target/debug/jade cache info
./target/debug/jade cache clean
```
