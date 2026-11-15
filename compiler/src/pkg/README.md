# `src/pkg/`: the package manager

## What this subtree is

This is the machinery behind `jade pkg add`, `remove`, `install`, `update`, and `list`. It turns the `[dependencies]` section of `jade.toml` into a pinned `jade.lock` and a populated `libs/` directory, and hands every dependency to import resolution.

```
jade.toml [dependencies] → jade.lock → libs/
```

A dependency is one of two kinds, and `abi` in its entry says which:

```toml
[dependencies]
geometry = { path = "../geometry", abi = "source" }
shapes   = { git = "https://github.com/me/shapes", tag = "v1.2.0", abi = "source" }
vectors  = { url = "https://example.com/vectors-1.0.tar.gz", version = "1.0", abi = "source" }
imgtools = { path = "rust/imgtools", abi = "rust" }
zlib     = { path = "rust/zlib-jade", package = "zlib-jade", abi = "rust" }
```

*A source package* is Jade source: a directory, git repository or `.tar.gz` archive holding a project with a `[package]` section. Design decision 11 in `design/v2-static-values.md` makes this the way Jade packages are shared, because a prebuilt library cannot hold a generic function: the checker copies a function per set of argument types, and a library cannot know a caller's types. A source package's files join the program's import graph, and are checked and compiled with it. A path package is read where it is; a git or archive package is unpacked into `libs/<name>-<version>/`. The lock pins a hash of the package's `jade.toml` and every `.jde` file rather than the bytes it came in, so the same check covers all three sources, and a README edit does not move a pin.

*A Rust package* is a crate that marks what Jade may call with `#[jade::export]`. Cargo fetches and builds it with the program, in `crate::rustpkg`, so this module has nothing to install for it: it validates the entry, leaves it out of the lock, since cargo pins it in the generated crate's `Cargo.lock`, and hands its name to import resolution. `jade pkg add` builds it on the spot to report what it exports, and to refuse one that does not compile.

`abi` must be written; there is no default. Until 2.0.5 there were two more kinds, `jade` for a v1 package built with `jade build --lib` and `c` for a plain C library behind a generated shim, both prebuilt shared libraries the program opened with `dlopen`. `design/v2-rust-packages.md` says why Rust replaced both. A manifest naming either is refused where it says so, by `project::Abi`'s deserializer, with the replacement named.

## Why it was built this way

There is deliberately no package registry of Jade's own. As in Go, a source dependency names where it lives rather than an entry in a central index. A Rust package may name a crates.io version, since cargo has a registry.

That choice has one consequence worth stating plainly: *there is no version solving*. Each source package contributes exactly one tree, `jade.lock` is a flat list, and the only choice made is between two versions already named, where the higher wins (`compare_versions`).

`jade pkg add` merges a source package's own dependencies into the consuming project's `jade.toml`, Rust ones included. A transitive dependency is a real dependency, and the manifest is what a person reads to know what their project uses. A `path` dependency of a package is followed only when the package itself is local, since a path names a directory on the machine that wrote it.

A local `path` package is *re-hashed on every install*, while a git or archive package is not. A path points at a directory the user edits, the one source that can change underneath a lock that is otherwise still correct. A URL either serves the bytes the lock pins or it does not, and quietly re-pinning it would defeat the point of having a lock. So `refresh_local` runs before every `materialize`, and `verify_local_unchanged` is its `--locked` counterpart, turning the same drift into a CI failure rather than a fix.

The whole surface between this module and the rest of the compiler is one function, `resolved_libraries`. Dependencies come back as synthetic `project::LibraryEntry` values merged into the manifest's `[lib]` map: a source package's names its directory and entry module, and a Rust package's carries the dependency's name in `rust`. So the import resolver never learns what a dependency is beyond that one field.

## What each file does

- *`mod.rs`* handles resolution and materialization: `resolve`, which validates every entry and locks the source packages; `materialize`; `verify_in_sync`, which compares the lock with the manifest's source packages and ignores Rust ones; `ensure_ready`, which `jade run` and `jade build` call first; and `resolved_libraries`, the one seam into the rest of the compiler. It also holds the local-source group described above, meaning `refresh_local`, `verify_local_unchanged`, and `local_drift`. It defines `LIBS_DIR`, which is `libs/` and is gitignored because `jade.lock` is what travels, and `ANY_PLATFORM`, the artifact key a source package is locked under.
- *`source.rs`* handles source packages: reading a package's `[package]`, the tree hash the lock pins, checking out a git commit, unpacking an archive, installing into `libs/`, the `[lib]` entry a package becomes, and the dependencies a package declares, rewritten for the project adding it. It runs the `git` and `tar` programs, which only `jade pkg` needs.
- *`lock.rs`* defines the `jade.lock` format: `LockedPackage`, `LockedArtifact`, and a digest per artifact. The lock is meant to be committed.
- *`fetch.rs`* handles fetching archives and checking their integrity, behind a `Fetcher` trait. Everything in this module takes a `&dyn Fetcher` rather than reaching the network directly, so the whole package manager can be tested offline against a map of canned responses. `jade upgrade` uses its platform tag and HTTP client too.
- *`manifest.rs`* makes format-preserving edits to `jade.toml` through `toml_edit`. `jade pkg add` and `jade pkg remove` rewrite a file a person wrote by hand. Parsing and re-serializing it instead would silently discard every comment and all the original layout.
- *`tests.rs`* holds the package manager tests, all of which run offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`, and `rustpkg::dependencies` to list a manifest's Rust packages.

*Used by:* `cli/pkg.rs`, for the commands; `cli/build.rs`, which calls `ensure_ready`; and `aot/imports.rs` and `cli/check.rs`, which resolve imports against `resolved_libraries`.

## Gotchas

*The lock and the manifest have to agree on what a dependency is, not only on which ones exist.* `verify_in_sync` reports a name locked as one kind and declared as another. Since `ensure_ready` reads the lock rather than resolving again, a disagreement left unreported would build from a lock that describes some other package.

*A Rust package never appears in the lock.* Anything that counts dependencies for the lock has to filter with `is_locked`, or a project with only Rust packages would be told to run `jade pkg install` for nothing, and a correct lock would read as stale the moment a Rust package was added.

*A present tree is not the same as a current one.* `materialize` compares `libs/` against the lock, so anything that changes the true source without changing the lock is invisible to it. That is exactly how an edited `path` dependency would keep building as the copy it was when it was added. `refresh_local` closes that for local sources; any future kind of source that can change in place needs the same treatment.

Tests must never reach the network. Use the `Fetcher` trait.

## Building and testing

```sh
cargo test pkg::
```
