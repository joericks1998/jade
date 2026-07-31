# `src/project/` — `jade.toml` and import resolution

## What this subtree is

Everything about a Jade *project* as opposed to a single file: finding the project root, parsing `jade.toml`, and turning a `use` path into a file on disk.

## Why it exists

Import resolution has to give the same answer to both engines. The VM resolves imports at runtime and the AOT backend resolves them at compile time, and when those two disagree you get a program that runs one way and compiles another — a failure that stays silent until someone tries both. So the resolution *rules* live here, in one place, and both `vm/chunk.rs` and `aot/imports.rs` call into them.

The `[lib]` mechanism is the reason this module is more than a TOML parser. A `[lib.<name>]` section registers a directory and its modules under a name, anchored at the project root, so `use utils.math` works from anywhere in the tree rather than only from a sibling directory. The `files` allowlist carries extensions on purpose: the extension both disambiguates modules from other files in the directory and *selects how each is loaded* — `.jde` is Jade source, while `.dylib` / `.so` / `.dll` is a native shared library loaded over the `jade_pkg_init` C ABI.

The package manager reuses that mechanism rather than adding a parallel one. Resolved dependencies come back from `pkg/` as synthetic `LibraryEntry` values and are unioned into the `[lib]` map, so nothing downstream learns what a dependency is.

## What each file does

- **`mod.rs`** — the manifest types (`ProjectManifest`, `ProjectSection`, `LibraryEntry`, `DependencyEntry`, `Abi`, `CSymbol`) and the resolution API:
  - `find_project_root` / `find_project_root_from` — walk up looking for `jade.toml`.
  - `load_project` — parse the manifest.
  - `resolve_library_import` — a `use <lib>.<module>` path against the `[lib]` map.
  - `resolve_relative_import` — a sibling-file import.
  - `ambiguous_bare_import` — detects a bare name that could mean two things, so the error names the ambiguity instead of silently picking one.
  - `find_test_files` — discovers `test_*.jde` and `*_test.jde` for `jade test`.
- **`tests.rs`** — resolution tests.

## Who uses it

*Depends on:* `serde` and `toml` only. It is deliberately near the bottom of the stack.

*Used by:* `vm/chunk.rs` (runtime import resolution), `aot/imports.rs` (compile-time import resolution), `pkg/` (contributes `LibraryEntry` values), and most of `cli/` — `run`, `test`, `build`, `env`, and `pkg` all start by finding the project root.

## Gotchas

Any change to resolution behavior must be checked on both engines. `./src/scripts/backend-parity.sh` covers `examples/imports/`.

`[scripts]` entries are shell strings run by `jade run <name>`; they are not Jade code.

## Building and testing

```sh
cargo test project::
```
