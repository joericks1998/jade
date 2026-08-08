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
- **`jade pkg update` reconciles, it does not discover.** It re-resolves the lock against the manifest and re-fetches. To move to a new version, edit `jade.toml` (or re-run `jade pkg add` with a new `--version`).

## Commands

| Command | Effect |
|---|---|
| `jade pkg add <name> --url <u> --version <v>` | Add a remote dependency and install it |
| `jade pkg add <name> --path <file>` | Add a local `.so`/`.dylib`, Jade or plain C |
| `jade pkg install` | Fetch and verify everything `jade.lock` pins |
| `jade pkg install --locked` | Same, but fail rather than update the lock (use in CI) |
| `jade pkg update [name]` | Re-resolve against `jade.toml` and rewrite the lock |
| `jade pkg bind <name> --header <h>` | Re-generate a C dependency's symbol table from its header |
| `jade pkg remove <name>` | Drop it from the manifest, the lock, and `libs/` |
| `jade pkg list` | Show what is locked and whether it is installed here |

`jade run`, `jade test`, and `jade build` install anything missing automatically, so a fresh clone needs no separate step.

:::warning The package commands are nested
Every one of them is `jade pkg <something>`. There is no bare `jade add`, `jade install`, or `jade update`.
:::

:::note
`jade pkg update` manages **dependencies**. `jade upgrade` updates the **jade toolchain itself**. They are unrelated. See the [CLI reference](cli#jade-upgrade).
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

## Local `path` dependencies

A `path` dependency points at a file you build, so it is the one source that legitimately changes while the lock stays correct. It is treated differently for that reason: **the source file is re-hashed on every install and every run**, and if it has changed, the lock is re-pinned and the new artifact copied into `libs/`.

`--version` is optional here, and the lock records `local` when you leave it out. A `--url` dependency must name one, because that is what makes its directory under `libs/` unique.

```
$ jade run main.jde
note: re-pinned engine (local source changed)
```

A URL dependency is never re-pinned. It either serves the bytes the lock names or it does not, and quietly re-pinning it would defeat the point of having a lock. Moving a URL dependency to different bytes is what `jade pkg update` is for.

`jade pkg list` marks a local dependency whose source has moved ahead of its pin:

```
engine 1.0.0  [jade]  installed (local source changed — run `jade pkg install`)
```

Under `--locked` the same drift is an error rather than a fixup, because a rebuilt library means the committed lock is stale:

```
$ jade pkg install --locked
error: dependency 'engine': the local source has changed since jade.lock was written
  locked 462fc9e8…
  on disk f2d2eb23…
--locked forbids rewriting the lock. Run `jade pkg install` and commit jade.lock,
or rebuild the source to match.
```

:::note
Before v1.1.35 a rebuilt local dependency was ignored: installing compared `libs/` against the lock, found a match, and kept loading the copy taken when the dependency was added. Only re-running `jade pkg add` picked up the new build.
:::

## Committing

Commit `jade.lock`. Do not commit `libs/` — `jade new` adds it to `.gitignore`. The lock is what travels; the binaries are rebuilt from it.

## Using a plain C library

A library like `libsqlite3` exports no `jade_pkg_init`, so the loader cannot take it directly. Jade generates a small binding shim that wraps it into an ordinary Jade package — and since v1.3.0 that is not a step you have to know about. Adding a C library is the same command as adding a Jade one:

:::note
Build the library from the `.c`, with `-dynamiclib` on macOS or `-shared` on Linux:

```sh
clang -dynamiclib -o libdemo.dylib demo.c    # macOS
cc -shared -fPIC -o libdemo.so demo.c        # Linux
```

Naming the header instead — `clang -o libdemo.dylib demo.h` — produces a precompiled header, which is a perfectly ordinary file with a perfectly ordinary name and nothing the loader can open. Jade refuses it when you add it and says so.
:::

```sh
jade pkg add demo --path libdemo.dylib
```

```
demo exports no jade_pkg_init, so it is a plain C library
found header /path/to/demo.h
covers 3 of the 5 symbols the library exports
3 bound, 1 assumed, 2 skipped
```

Three things happened there, and none of them needed a flag:

- **The kind of library came from the artifact.** A Jade package exports `jade_pkg_init` and a C library does not. Both are a `.dylib`, so the filename could never have told you — and this is the same symbol the loader requires at run time, so what is detected here is exactly what `use` will later accept.
- **The header was found.** `libdemo` implies `demo.h`, and the search covers pkg-config, the usual include roots, and the macOS SDK. The candidate is accepted only if the library actually exports what the header declares, so a header belonging to some other library of the same name is refused now rather than surfacing later as a linker error.
- **The symbol table was generated and the shim built.** `use demo` works immediately.

The manifest it writes is ordinary TOML you can read and edit:

```toml
[dependencies.demo]
path         = "libdemo.dylib"
abi          = "c"
headers      = ["demo.h"]
include_dirs = ["/path/to"]

[dependencies.demo.symbols.demo_open]
args       = ["str"]
ret        = "handle<demo_ctx>"
fails_when = "null"

[dependencies.demo.symbols.demo_read]
args = ["handle<demo_ctx>", "out_buffer:char", "int"]
ret  = "int"
```

### When there is no header at all

Sometimes there isn't one to find — a library someone handed you, or one whose headers were never installed. Jade still writes a manifest, because the library always says what it *exports*:

```
$ jade pkg add demo --path libdemo.dylib
demo exports no jade_pkg_init, so it is a plain C library
added demo to jade.toml
2 of its symbols are listed there with no signature: demo_add, demo_scale
```

```toml
[dependencies.demo.symbols]
demo_add = "?"
demo_scale = "?"
```

A `"?"` means *the name is known and the prototype is not*. Replace it with the real one and the dependency works:

```toml
[dependencies.demo.symbols.demo_add]
args = ["int", "int"]
ret  = "int"
```

Filling in blanks in a file that already lists every function beats going to look for a header, which is why Jade writes the names rather than nothing.

**Why it can't just read the types out of the library.** A shared library carries an export table of *names*. C keeps no argument or return types in a compiled artifact, so `demo_add` in that table says only "there is a `demo_add`". Types survive in DWARF, which release builds strip and which the macOS linker leaves behind in the `.o` files rather than the library. So the half that is missing is genuinely gone, and Jade will not guess at it: a wrong prototype is a corrupted stack several calls later, with nothing pointing back at the manifest.

`jade check`, `jade run` and `jade build` all refuse a dependency that still has a `"?"` in it, and name the symbols.

### When you do need a flag

| Situation | Flag |
|---|---|
| The header search missed, or you want a specific one | `--header <file.h>` |
| The header is not on the default search path | `-I <dir>` (repeatable) |
| The dependency comes from `--url`, so there is no local file to read | `--c-abi` |

### Where binding happens

Binding runs on `add` and on `install`, not only on `bind`:

- **`jade pkg add`** binds when it finds or is given a header.
- **`jade pkg install`** fills in any dependency that names a header but has no `symbols` yet. A manifest that already carries its symbols is left alone, so a fresh clone installs without clang.
- **`jade pkg install --locked`** never binds, because a reproducible install must not depend on what the local clang makes of a header.
- **`jade pkg bind`** is for the cases with a real decision in them: re-running after a header changes, or narrowing a large header with `--only`. It merges into the existing table rather than replacing it, and `--dry-run` shows the report without touching `jade.toml`.

Binding a C library needs `clang` on `PATH` to read the header, and a C compiler (`cc`) to build the shim.

### The report is the feature

No generator binds everything, and one that quietly covers two thirds of an API is how the missing third gets found at run time. So the output says what it dropped and why:

```
assumed (check these):
  demo_read: `void *` next to a length was read as a buffer the call fills;
             if the library reads it instead, change it to `bytes`

skipped:
  1 — returns an unsupported type `void *`
      demo_raw
  1 — takes varargs
      demo_printf
```

Coverage is quoted against the library's own export table — "covers 181 of the 194 symbols the library exports" — because a bare "181 bound" reads as success whether the library has 190 entry points or 900.

### The binding vocabulary

If you write or correct a symbol by hand, these are the spellings `args` and `ret` accept.

| Spelling | Meaning |
|---|---|
| `int`, `float`, `bool`, `str`, `nil` | Scalars. `nil` is a return only. |
| `bytes` | Binary data. As an argument it is one Jade value and the two C parameters `(const void*, size_t)`. |
| `handle<T>` | An opaque pointer the library owns — a `sqlite3*`, a `SNDFILE*`. Jade holds it, hands it back, and never looks inside. The type name is checked, so passing a statement where a connection belongs is a readable error rather than a crash inside the library. `T` is written the way C writes it, so a struct with no typedef of its own keeps the keyword: `handle<struct ZSTD_CCtx_s>`. |
| `out_buffer:<ctype>` | A buffer the call fills. It consumes **no** Jade argument: `x_read(handle, buf, n)` is called as `x_read(handle, n)` and hands back the bytes. Its size comes from the next declared argument, which must be an `int`. |
| `out_struct:<Type>` | A struct the call fills through a pointer. Needs the library's real header in `headers`. |
| `out_handle:<T>` | A handle written through a pointer — `sqlite3_open(path, &db)`. The C return value becomes the status, and the handle is what Jade gets. |
| `callback:<ret>(<args>)` | A Jade function the library may call while the call runs. The signature is written in the library's own C types, e.g. `callback:int(int, const char*)`. |

A whole symbol may also be written as the single string `"?"` — the name is known, the prototype is not. That is what `jade pkg add` writes when it finds no header, and every command that would use the binding refuses it by name.

A symbol may also declare `fails_when`, naming how it reports failure: `null`, `negative`, `nonzero`, or `never`. The shim then clears `errno`, tests the return, and turns a failure into a catchable Jade error carrying the reason. Without it a failed call gives back its raw sentinel and the reason the library already recorded is thrown away — the program sees `-1` and nothing else. The default is "cannot fail", because reading a convention that is not there would turn every legitimate `-1` into a raise.

Some rules worth knowing before you hand-write one:

- **At most one out-parameter per symbol.** Two would have to come back as a pair with no obvious names.
- **A symbol with both an out-parameter and a return value comes back as `.ret` and `.out`.** When the C function returns `void` there is no pair to make, and the filled value is the result directly.
- **Jade never closes a handle for you.** It reclaims its own wrapper and leaves the pointer alone, because it cannot know what the pointer is or which allocator made it. Closing is a call the binding exposes.
- **A handle cannot cross into a task.** Jade cannot tell a thread-safe library from an unsafe one, so this is refused at compile time rather than racing quietly. Open one inside the task and close it before returning.
- **A callback is live only while the call that passed it is running.** A library that stores one and invokes it later is not supported. A raise inside a callback is deferred: the library finishes cleanly and the error reaches your `catch` afterwards.

A symbol using anything outside this vocabulary is rejected **by name** at install time rather than silently marshalled to nil.

## Publishing a Jade package

`jade build --lib` compiles a Jade file to a shared library exporting `jade_pkg_init`:

```sh
jade build mathlib.jde --lib               # -> mathlib.dylib (or .so)
jade build mathlib.jde --lib --export add  # bind only `add`
```

Jade has no `pub` keyword — every top-level function is public — so the default is to export all of them. `--export` narrows that.

Publish the result wherever you like (GitHub Releases is the natural home), one build per platform, named so a `{platform}` URL finds them. Consumers then `jade pkg add` it like any other dependency.

### A package of several files

A package is not limited to one file. Every module the entry `use`s is compiled into the same artifact, each in its own namespace, so a package can be organized like any other program:

```jade
// mathlib.jde — the entry module
use geometry
use text

fn area(w, h) { return geometry.area(w, h) }
fn shout(s) { return text.shout(s) }
```

**The entry module is the package's API.** Only its top-level functions become bindings; everything the imported modules define stays internal, which is why `area` above is a one-line forwarder. That is the same rule as a single-file package, and it means adding a helper to `geometry.jde` never silently widens what consumers can call.

### Declaring the package in `jade.toml`

Rather than passing the entry and the exports on the command line every time, a package can describe itself:

```toml
[package]
name    = "mathlib"
version = "1.2.0"
entry   = "mathlib.jde"                                # optional; defaults to <name>.jde
sources = ["geometry.jde", "text.jde", "mathlib.jde"]  # optional
exports = ["area", "shout", "version"]                 # optional; defaults to all
```

Then, from anywhere in the project:

```sh
jade build --lib          # -> mathlib.dylib, exporting the three named functions
```

`name` becomes the artifact's filename and the name consumers `use`, so it has to be a usable identifier — letters, digits, and underscores.

`sources` is optional, and it is the reason to write a `[package]` at all rather than a shell alias. The build finds a package's files by following `use` from the entry, so the list is not what makes the build work. What it buys is the two errors the import graph cannot raise on its own:

- a file you meant to ship but forgot to import, which would silently vanish from the artifact;
- a file that got pulled in without you deciding to ship it.

Either one fails the build, naming the file:

```
error: [package] sources in jade.toml does not match what the package imports
  declared but never imported: orphan.jde
    nothing reaches these from 'mathlib.jde', so they would not be in the artifact
```

Omit `sources` and the import graph is taken at its word.

:::note
`[package]` describes a project that **is** a package. `[dependencies]` describes packages a project **uses**. A project can have both: a package that depends on another package.
:::

Nothing changes for consumers. The artifact is an ordinary Jade package, added and locked exactly as before:

```sh
jade pkg add mathlib --url 'https://example.com/mathlib-{platform}.so' --version 1.2.0
```

## The FFI's limits

The native ABI carries `int`, `float`, `bool`, `str`, and `nil`; arrays, dicts, and structs since v1.1.31; `bytes` since v1.2.2; and opaque handles since v1.3.0 — all in both directions. A struct crosses with its type name attached, so the receiving side can tell a `Config` from anything else shaped like one.

A **function** crosses in one direction only. You can pass one in as a callback, and the library invokes it while your call runs. A package cannot hand one back, because a C function is not something a Jade program can hold.

What still does not cross: **futures and prompts**, which arrive as `nil`.
