# `src/pkg/` — the package manager

## What this subtree is

The machinery behind `jade pkg add` / `remove` / `install` / `update` / `list`. It turns `[dependencies]` in `jade.toml` into a pinned `jade.lock` and a populated `libs/` directory.

```
jade.toml [dependencies] → jade.lock → libs/
```

For a plain C dependency there is a step before that one, and it is the step that makes the rest usable at scale:

```
<library>.h  →  binding generation  →  jade.toml [symbols]  →  generated shim  →  libs/
```

That step is **not a command you have to know about**, and usually not a flag or a path you have to supply either. `jade pkg add <name> --path <lib>` is the whole thing for either kind of library: it reads the artifact to see which it is, finds the header if it needs one, generates the table, and builds the shim. `--c-abi` and `--header` remain as overrides for when there is nothing local to read or the guess would miss.

**What kind of library it is comes from the artifact, not a flag.** A Jade package exports `jade_pkg_init`; a plain C library does not. That is not a heuristic — it is the same symbol the loader requires at run time, so anything detected as a Jade package is exactly what `use` will later accept. Both kinds are a `.dylib`, so the extension says nothing and only the symbol table can tell them apart. A URL dependency has nothing to read at `add` time, which is what `--c-abi` is still for. `jade pkg install` fills in any dependency that names a header but has no symbols yet, and `jade pkg bind` remains for the cases with a real decision in them — re-running after a header changes, or narrowing a large one with `--only`.

**A `.so` cannot supply the header itself**, and it is worth being precise about why: a shared library carries an export table of *names*, and C does not mangle them, so `sqlite3_open` says nothing about its signature. Types survive only in DWARF, which release builds strip. So a header has to come from the filesystem — but the library still has the last word on *which* one. `libsqlite3.dylib` implies `sqlite3.h`, the search covers pkg-config, the usual include roots and the macOS SDK, and the candidate is accepted only if the library actually exports what it declares. A header describing some other library of the same name is refused before anything is written, rather than surfacing later as an undefined symbol from the linker.

That same export table gives the one number that says whether a binding is usable: **coverage**. "181 bound" reads as success whether the library has 190 entry points or 900, so the report says how many of the library's exports were covered.

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
- **`bindgen.rs`** — generates a dependency's `symbols` and `structs` tables from its C header, driven by `jade pkg add --header`, `jade pkg install`, and `jade pkg bind`. This is what makes "bind any `.so`" true in practice: the ABI could express handles, blobs and structs, but every signature still had to be transcribed by hand, and SQLite has around 200 entry points.

  It reads the header with **clang** — `clang -Xclang -ast-dump=json -fsyntax-only`, over a pipe. Parsing C by hand is a tar pit of macros, conditionals and typedef chains, and a home-grown parser would misread far more than it read. Shelling out rather than linking `libclang` keeps a large native dependency out of the shipped binary, and costs nothing in practice: `cc` is already required to bind a C library at all.

  **The skip report is the feature.** No generator binds everything, and one that quietly covers two thirds of an API is how the missing third is found at run time. So what it drops is named with a reason, grouped so one cause reads as one fact; and a binding resting on an inference — a non-const `T*` beside a count is *almost* always an out-buffer — is listed as *assumed* rather than buried. On the real `sqlite3.h` that is 181 bound, 2 assumed, 105 skipped, and every skip is a genuine limit of the ABI rather than a gap in the reader.
- **`design.md`** — the shim's rewrite rules: how a `bytes` argument becomes two C parameters, why an out-parameter consumes no Jade argument, how two results come back, and why `out_struct` requires the library's header rather than a declared layout. Read it before changing what a binding can express.
- **`tests.rs`** — package manager tests, all offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`.

*Also depends on:* `clang` on `PATH`, but only when a header is actually read — `add --header`, `bind`, or an `install` filling in missing symbols. A manifest that already carries its symbols installs without it. Nothing else in the package manager needs it, and its absence is reported with the workaround (write the table by hand) rather than as a crash.

*Used by:* `cli/pkg.rs` for the commands. Indirectly, `vm/chunk.rs` and `aot/imports.rs` consume the `[lib]` entries this module contributes, without knowing they came from a dependency.

## Gotchas

`cshim.rs` binds a C function that *fills* a struct through a pointer, but not one that reads a struct you hand it. The out direction is what the shim can be sure about, because the library owns the layout and the header proves it; passing one in would need the same guarantee from the other side and nothing has asked for it.

**A struct out-parameter needs the library's header, and that is not negotiable.** The shim declares a real local of the struct's type, so the layout comes from the C compiler. Taking it from a hand-written field list instead would put integer widths and padding in a TOML file, where one disagreement writes at the wrong offset with nothing to catch it — valid manifest, compiling shim, corrupted memory. Add `include_dirs` when the header is not on the default search path.

The generated C is checked by compiling it, not only by matching strings. A test that asserts the output *contains* `if (!(r))` passes just as happily on a file with an unbalanced brace or a missing `#include`, and that file fails at install time on a user's machine instead of here.

Tests must never hit the network — use the `Fetcher` trait.

**Binding runs on `add` and `install`, not only on `bind`.** A separate step is one the user has to learn about, and it has no decision in it — a header either binds or it does not. `install` only fills in a dependency whose `symbols` are *absent*, so a committed manifest already carries them and a fresh clone installs without needing clang at all. `--locked` never binds, because a reproducible install must not depend on what the local clang makes of a header.

**`jade pkg bind` merges, it does not replace.** Binding a large header a piece at a time with `--only` is a normal way to work, and replacing the table would make the second run delete what the first produced. Merging also leaves a hand-corrected entry alone unless that same symbol is regenerated.

**The generator and the shim have to agree, and nothing else checks that they do.** They are written against one vocabulary in two files, so a spelling added to `bindgen.rs` and not to `cshim.rs` passes every unit test on both sides and then fails at `jade pkg install` on a user's machine. `bindgen/tests.rs` closes the loop by driving a header through both halves and compiling the result.

**`include_dirs` is written absolute, on purpose.** The shim is compiled inside `libs/<dep>/` rather than where `jade pkg bind` ran, so a relative `-I` resolves against the wrong directory and surfaces as a "file not found" from cc at install time, well away from the cause.

**"It has the right name" is not "it is a library", and nothing between `add` and `dlopen` disagreed.** A dependency was checked for what it *exported* and never for whether it could be loaded at all, so a file that was not an object file passed through the manifest, `libs/`, resolution and the linker, and was refused by the dynamic loader in the finished program. `bindgen::is_loadable_object` reads the magic number, and it is called in two places on purpose: `jade pkg add`, which can then say what probably went wrong, and `materialize`, which is the one point every source passes through with the bytes in hand — a hand-written manifest and a fresh clone never touch `add`. Anything new that puts a file into `libs/` needs the same check.

**A present artifact is not a current artifact.** `materialize` compares `libs/` against the *lock*, so anything that changes the true source without changing the lock is invisible to it. That is exactly how a rebuilt `path` dependency used to keep running as the copy it was when it was added. `refresh_local` closes it for local sources; any future source kind that is mutable in place needs the same treatment, and adding one without it reintroduces the same silent staleness.

## Building and testing

```sh
cargo test pkg::
```
