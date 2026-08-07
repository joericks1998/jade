# `src/pkg/` — the package manager

## What this subtree is

The machinery behind `jade pkg add` / `remove` / `install` / `update` / `list`. It turns `[dependencies]` in `jade.toml` into a pinned `jade.lock` and a populated `libs/` directory.

```
jade.toml [dependencies] → jade.lock → libs/
```

## Why it was built this way

Dependencies are **prebuilt native shared libraries**, sourced from a local path or a URL. There is deliberately no package registry — like Go, a dependency names where it lives rather than an entry in a central index.

That choice has a consequence worth stating plainly: a `.so` carries no manifest of its own, so **there is no transitive resolution and no version solving**. Each dependency contributes exactly one artifact, `jade.lock` is a flat list, and "resolution" means picking the right platform build. A package that needs another package must say so in its documentation; Jade cannot discover it.

The second decision is that the lock records an artifact **for every platform**, not just the current one. Unlike Cargo, which locks portable source, a lock naming one artifact would only be valid on the machine that generated it — a macOS developer would commit a lock that Linux CI could not install, and with no registry to ask, could not even *verify*.

The third is that a local `path` dependency is **re-hashed on every install**, while a URL dependency is not. A path points at a file the user builds and rebuilds; it is the one source that legitimately changes underneath a lock that is otherwise still correct. A URL is the opposite — it either serves the bytes the lock pins or it does not, and quietly re-pinning it would defeat the point of having a lock. So `refresh_local` runs before every `materialize`, and `verify_local_unchanged` is its `--locked` counterpart, turning the same drift into a CI failure rather than a fixup.

The integration surface with the rest of the compiler is one function, `dependency_libraries`. Resolved dependencies come back as synthetic `project::LibraryEntry` values and get unioned into the manifest's `[lib]` map, so neither the VM nor the AOT import resolver ever learns what a dependency is. Both keep resolving `[lib]` entries exactly as before, which is how the two backends are kept from drifting on imports.

## What each file does

- **`mod.rs`** — resolution and materialization. `dependency_libraries` is the one public seam into the rest of the compiler. Also holds the local-source group (`refresh_local`, `verify_local_unchanged`, `local_drift`) described above, and defines `LIBS_DIR` (`libs/`, gitignored — `jade.lock` is what travels) and `ANY_PLATFORM`, the artifact key for a URL with no `{platform}` placeholder.
- **`lock.rs`** — the `jade.lock` format: `LockedPackage`, `LockedArtifact`, digests per platform. Meant to be committed.
- **`fetch.rs`** — artifact acquisition and integrity, behind a `Fetcher` trait. Everything in this module takes a `&dyn Fetcher` rather than calling out directly, so the whole package manager is testable offline against a map of canned responses.
- **`manifest.rs`** — format-preserving edits to `jade.toml` via `toml_edit`. `jade pkg add` and `jade pkg remove` rewrite a file a person wrote by hand; a parse-and-reserialize round-trip would silently discard every comment and all the original layout.
- **`cshim.rs`** — generates a Jade-ABI binding shim for a plain C library. The loader requires a `jade_pkg_init` symbol, which something like `libz` does not have. Rather than teach the runtime to dispatch arbitrary C signatures — a `libffi` dependency and *two* marshalling implementations, one per engine — this emits a small C wrapper and compiles it with `cc`. The result is an ordinary Jade-ABI package both backends already know how to load. The type vocabulary is the FFI's — `int`, `float`, `bool`, `str`, `bytes`, and `nil` for returns — plus two forms that make the shim *rewrite* a call rather than forward it, which is what lets a real C signature be callable at all:

  - `bytes` as an argument expands to the C pair `(const void*, size_t)`.
  - `out_buffer:<ctype>` and `out_struct:<Type>` are out-parameters. They consume **no** Jade argument: the shim owns the memory, so `x_read(handle, buf, n)` is called from Jade as `x_read(handle, n)` and hands back the bytes. `src/pkg/design.md` has the full rules.

  A symbol may also declare `fails_when` — `null`, `negative`, `nonzero`, or `never`. The shim then clears `errno`, tests the return against that convention, and on failure hands back a `JADE_FFI_ERROR` carrying `strerror` text and the number, which both engines already turn into a catchable Jade raise. Without it a failed call returns its raw sentinel and the reason the library *had already recorded* is simply thrown away: the program sees `-1` and nothing else. There is no universal convention to infer, which is why the binding names the one its symbol uses; the default is "cannot fail", because reading a convention that is not there would turn every legitimate `-1` into a raise.
- **`design.md`** — the shim's rewrite rules: how a `bytes` argument becomes two C parameters, why an out-parameter consumes no Jade argument, how two results come back, and why `out_struct` requires the library's header rather than a declared layout. Read it before changing what a binding can express.
- **`tests.rs`** — package manager tests, all offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`.

*Used by:* `cli/pkg.rs` for the commands. Indirectly, `vm/chunk.rs` and `aot/imports.rs` consume the `[lib]` entries this module contributes, without knowing they came from a dependency.

## Gotchas

`cshim.rs` binds a C function that *fills* a struct through a pointer, but not one that reads a struct you hand it. The out direction is what the shim can be sure about, because the library owns the layout and the header proves it; passing one in would need the same guarantee from the other side and nothing has asked for it.

**A struct out-parameter needs the library's header, and that is not negotiable.** The shim declares a real local of the struct's type, so the layout comes from the C compiler. Taking it from a hand-written field list instead would put integer widths and padding in a TOML file, where one disagreement writes at the wrong offset with nothing to catch it — valid manifest, compiling shim, corrupted memory. Add `include_dirs` when the header is not on the default search path.

The generated C is checked by compiling it, not only by matching strings. A test that asserts the output *contains* `if (!(r))` passes just as happily on a file with an unbalanced brace or a missing `#include`, and that file fails at install time on a user's machine instead of here.

Tests must never hit the network — use the `Fetcher` trait.

**A present artifact is not a current artifact.** `materialize` compares `libs/` against the *lock*, so anything that changes the true source without changing the lock is invisible to it. That is exactly how a rebuilt `path` dependency used to keep running as the copy it was when it was added. `refresh_local` closes it for local sources; any future source kind that is mutable in place needs the same treatment, and adding one without it reintroduces the same silent staleness.

## Building and testing

```sh
cargo test pkg::
```
