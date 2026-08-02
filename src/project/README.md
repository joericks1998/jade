# `src/project/` — `jade.toml` and import resolution

## What this subtree is

Everything about a Jade *project* as opposed to a single file: finding the project root, parsing `jade.toml`, and turning a `use` path into a file on disk.

## Why it exists

Import resolution has to give the same answer to both engines. The VM resolves imports at runtime and the AOT backend resolves them at compile time, and when those two disagree you get a program that runs one way and compiles another — a failure that stays silent until someone tries both. So the resolution *rules* live here, in one place, and both `vm/chunk.rs` and `aot/imports.rs` call into them.

The `[lib]` mechanism is the reason this module is more than a TOML parser. A `[lib.<name>]` section registers a directory and its modules under a name, anchored at the project root, so `use utils.math` works from anywhere in the tree rather than only from a sibling directory. The `files` allowlist carries extensions on purpose: the extension both disambiguates modules from other files in the directory and *selects how each is loaded* — `.jde` is Jade source, while `.dylib` / `.so` / `.dll` is a native shared library loaded over the `jade_pkg_init` C ABI.

The package manager reuses that mechanism rather than adding a parallel one. Resolved dependencies come back from `pkg/` as synthetic `LibraryEntry` values and are unioned into the `[lib]` map, so nothing downstream learns what a dependency is.

`[package]` is the other direction: a project that *is* a package rather than one that uses packages. It names the entry module, the file list, and the exported functions, so `jade build --lib` reads a package's shape from the manifest instead of from flags. `[dependencies]` and `[package]` coexist — a package can depend on a package.

## What each file does

- **`mod.rs`** — the manifest types (`ProjectManifest`, `ProjectSection`, `PackageSection`, `LibraryEntry`, `DependencyEntry`, `Abi`, `CSymbol`) and the resolution API:
  - `PackageSection::validate` — checks a `[package]` before a build reads it. The name has to be an identifier because it becomes both a filename and the name `use` binds; an empty `sources` or `exports` is rejected rather than silently meaning the opposite of the default.
  - `find_project_root` / `find_project_root_from` — walk up looking for `jade.toml`.
  - `load_project` — parse the manifest.
  - `resolve_library_import` — a `use <lib>.<module>` path against the `[lib]` map.
  - `resolve_relative_import` — a sibling-file import.
  - `ambiguous_bare_import` — detects a bare name that could mean two things, so the error names the ambiguity instead of silently picking one.
  - `find_test_files` — discovers `test_*.jde` and `*_test.jde` for `jade test`.
- **`imports.rs`** — one answer to "what does this `use` name?", plus the check-time graph walk:
  - `resolve_import` — the single resolver, built on the three primitives above. Returns an `ImportTarget`: a built-in `std::*` package, a native shared library, or a Jade source file. `vm/chunk.rs` reaches it through a thin adapter, so a `use` that `check` accepts and the VM then cannot find is a shape the code cannot express.
  - `walk_imports` — resolves every import reachable from a file, following Jade modules transitively, and returns the first unresolvable one with its span. It loads nothing: a native module is checked for existence but never `dlopen`ed, since opening one runs its initializer and `check` must not execute the program it is checking.
  - `reachable_jade_modules` — the same walk, keeping the set it visits instead of discarding it. `jade build --lib` checks a `[package]`'s declared `sources` against it. Asking the frontend rather than the LLVM backend keeps that check off the compile path, which is right: what a package contains is a property of the import graph, not of code generation.
  - `program_import_paths` — the `use` / `from ... use` paths of a parsed program.
- **`tests.rs`** — resolution tests, and the import-walk tests covering missing modules, transitive breakage, cycles, and per-module directory resolution.

## Who uses it

*Depends on:* `serde` and `toml`, plus `frontend/` and `builtins/` for the import walk — it needs the lexer and parser to find a module's own imports, and the built-in package list to know which names are compiled in. `mod.rs` on its own stays at the bottom of the stack.

*Used by:* `vm/chunk.rs` (runtime import resolution), `aot/imports.rs` (compile-time import resolution), `cli/check.rs` (the import walk), `pkg/` (contributes `LibraryEntry` values), and most of `cli/` — `run`, `test`, `build`, `env`, and `pkg` all start by finding the project root.

## Gotchas

Any change to resolution behavior must be checked on both engines. `./src/scripts/backend-parity.sh` covers `examples/imports/`.

`[scripts]` entries are shell strings run by `jade run <name>`; they are not Jade code.

## Building and testing

```sh
cargo test project::
```
