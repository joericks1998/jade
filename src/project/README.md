# `src/project/`: `jade.toml` and import resolution

## What this subtree is

This holds everything about a Jade *project*, as opposed to a single file: finding the project root, parsing `jade.toml`, and turning a `use` path into a file on disk.

## Why it exists

Import resolution has to give the same answer to both engines. The VM resolves imports at run time, and the AOT backend resolves them at compile time. When those two disagree, you get a program that runs one way and compiles another, and the failure stays silent until someone tries both. So the resolution *rules* live here, in one place, and both `vm/chunk.rs` and `aot/imports.rs` call into them.

The `[lib]` mechanism is why this module is more than a TOML parser. A `[lib.<name>]` section registers a directory and its modules under a name, anchored at the project root. So `use utils.math` works from anywhere in the tree, rather than only from a sibling directory.

The `files` list carries file extensions on purpose. An extension does two jobs: it separates modules from the other files in the directory, and it *selects how each one loads*. `.jde` means Jade source, while `.dylib`, `.so`, and `.dll` mean a native shared library loaded over the `jade_pkg_init` C ABI.

The package manager reuses that mechanism rather than adding a second one beside it. Resolved dependencies come back from `pkg/` as synthetic `LibraryEntry` values, which are merged into the `[lib]` map. So nothing downstream ever learns what a dependency is.

`[package]` covers the other direction: a project that *is* a package, rather than one that uses packages. It names the entry module, the file list, and the exported functions, so `jade build --lib` reads a package's shape from the manifest rather than from flags. `[dependencies]` and `[package]` can both appear, because a package can depend on another package.

## What each file does

- *`mod.rs`* holds the manifest types, which are `ProjectManifest`, `ProjectSection`, `PackageSection`, `LibraryEntry`, `DependencyEntry`, `Abi`, and `CSymbol`. It also holds the resolution API:
  - `PackageSection::validate` checks a `[package]` before a build reads it. The name has to be a valid identifier, because it becomes both a filename and the name `use` binds. An empty `sources` or `exports` is rejected, rather than silently meaning the opposite of the default.
  - `find_project_root` and `find_project_root_from` walk up the directory tree looking for `jade.toml`.
  - `load_project` parses the manifest.
  - `resolve_library_import` resolves a `use <lib>.<module>` path against the `[lib]` map.
  - `resolve_relative_import` resolves a sibling-file import.
  - `ambiguous_bare_import` detects a bare name that could mean two things, so the error can name the ambiguity rather than silently picking one.
  - `find_test_files` finds every `test_*.jde` and `*_test.jde` for `jade test`.
- *`imports.rs`* gives one answer to "what does this `use` name?", plus the graph walk `check` runs:
  - `resolve_import` is the single resolver, built on the three primitives above. It returns an `ImportTarget`, which is either a built-in `std::*` package, a native shared library, or a Jade source file. `vm/chunk.rs` reaches it through a thin adapter, so a `use` that `check` accepts and the VM then cannot find is a shape the code cannot express.
  - `walk_imports` resolves every import reachable from a file, following Jade modules through as many levels as they go, and returns the first one it cannot resolve along with its span. It loads nothing. A native module is checked for existence but never `dlopen`ed, because opening one runs its initializer, and `check` must not execute the program it is checking.
  - `reachable_jade_modules` is the same walk, except it keeps the set of modules it visits rather than discarding it. `jade build --lib` checks a `[package]`'s declared `sources` against that set. Asking the frontend rather than the LLVM backend keeps the check off the compile path, which is right: what a package contains is a property of the import graph, not of code generation.
  - `program_import_paths` returns the `use` and `from … use` paths of a parsed program.
- *`tests.rs`* holds the resolution tests, plus the import-walk tests covering missing modules, breakage several levels down, cycles, and per-module directory resolution.

## Who uses it

*Depends on:* `serde` and `toml`, plus `frontend/` and `builtins/` for the import walk. The walk needs the lexer and parser to find a module's own imports, and the built-in package list to know which names are compiled in. `mod.rs` on its own stays at the bottom of the stack.

*Used by:* `vm/chunk.rs` for import resolution at run time, `aot/imports.rs` for import resolution at compile time, `cli/check.rs` for the import walk, and `pkg/`, which contributes `LibraryEntry` values. Most of `cli/` uses it too, because `run`, `test`, `build`, `env`, and `pkg` all start by finding the project root.

## Gotchas

Any change to resolution behavior must be checked on both engines. `./src/scripts/backend-parity.sh` covers `examples/imports/`.

`CSymbol` accepts two shapes: a table, or the bare string `"?"` for a prototype nobody has written yet. It deserializes by hand to keep the two apart. Using `#[serde(untagged)]` would collapse every table's error into "data did not match any variant", losing the "missing field `ret`" that says what is actually wrong.

A placeholder is deliberately *not* a `validate` failure. It is a legal manifest state, and refusing to load one would take `jade pkg list` and `jade pkg remove` down with it. The refusal lives in `pkg::unresolved_report` instead.

A `[scripts]` entry is a shell string that `jade run <name>` executes. It is not Jade code.

## Building and testing

```sh
cargo test project::
```
