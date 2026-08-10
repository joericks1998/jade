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

### Libraries split across several headers

Plenty of libraries do not have one header. libarchive declares its readers in `archive.h` and its entries in `archive_entry.h`; libgit2 puts its types in `git2/types.h` and its functions in twenty other files. Two things make that work.

**Bind each header in turn.** `jade pkg bind` merges, so a second run adds to the table rather than replacing it, and the header list grows with it.

```sh
jade pkg add archive --path libarchive.dylib --header /opt/homebrew/include/archive.h
jade pkg bind archive --header /opt/homebrew/include/archive_entry.h
```

```toml
headers = ["archive.h", "archive_entry.h"]
```

Every header in that list is `#include`d by the shim. If one were missing while its symbols stayed in the table, C would let the shim call them undeclared — assuming each returns `int` — and a call that really returns a pointer would come back truncated. The shim is compiled with `-Werror=implicit-function-declaration` so that cannot happen quietly; you get an error naming the missing header instead.

**Types are read from the whole include tree, and so are functions the library exports.** A function in `archive.h` written in terms of a type from `archive_entry.h` binds fine. `archive.h` also includes `stdio.h`, and binding `fopen` along with it would be wrong — so what a header includes is filtered by the library's own export table. `fopen` belongs to nobody; `archive_entry_new` is in the artifact, so it is bound. The header you name always contributes its own declarations, export table or not.

Point at the top header of a library that splits its API up and you get the whole library. `ares.h` declares seventy-odd symbols and includes `ares_dns_record.h`, which declares sixty-three more.

### Umbrella headers

Some libraries only have a header that declares nothing at all. `lzma.h`, `git2.h` and `alsa/asoundlib.h` exist to include the twenty files that do the declaring. Point at one and there is nothing in it to bind, while pointing at a sub-header usually fails because a sub-header on its own does not compile.

The same rule covers them, which is why they work: everything is swept in from the includes, and the umbrella stays the header the shim includes. It is the one case that *needs* the artifact — with no export table there is nothing exact to test an include against, so a header with declarations of its own binds those alone and an umbrella is an error naming `--path`.

```
$ jade pkg add lzma --path /opt/homebrew/lib/liblzma.dylib --header /opt/homebrew/include/lzma.h
covers 49 of the 114 symbols the library exports
49 bound, 8 assumed, 65 skipped; 3 struct(s)

that header declares nothing itself, so the 114 declarations it includes that
the library also exports were bound instead.
```

This needs the artifact, so `--path` has to be there too. An export table is an exact test rather than a guess about which directories count as system ones: `fopen` is declared in that translation unit and is not in liblzma, so it is not bound.

### Headers that include their neighbours

Almost no header stands alone, and the two ways one reaches its neighbours need two different directories. Both are searched for you.

```c
/* libfdt.h — the file sits right beside this one */
#include <libfdt_env.h>

/* brotli/encode.h — resolved against the directory above this one */
#include <brotli/port.h>
```

An angled include does not search the including file's own directory, so the header's directory is passed explicitly; and the second form needs the parent as well. Both are recorded in `include_dirs`, so the shim compile gets exactly what reading the header got.

### When you do need a flag

| Situation | Flag |
|---|---|
| The header search missed, or you want a specific one | `--header <file.h>` |
| A header lives somewhere neither rule above finds | `-I <dir>` (repeatable) |
| The dependency comes from `--url`, so there is no local file to read | `--c-abi` |

A directory you name with `-I` is searched before either guessed one, since a wide root can otherwise shadow the header you meant.

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

A symbol the header declares but the library does not export is dropped too. A header is written for the newest version while the artifact you have may have been built without some of it — libbrotlienc's header declares two such functions. Binding one produces a shim that compiles and then fails to *link*, and the linker takes the whole dependency down over it.

A symbol that cannot be bound is dropped on its own. It used to be able to take the whole dependency with it: the generator would emit a symbol filling a struct while dropping that struct's field table, and the shim refuses a reference to a table that is not there — so one opaque blob among two hundred good symbols made a library uninstallable. `sqlite3_snapshot_free` and `zip_file_attributes_init` are both that shape.

### The binding vocabulary

If you write or correct a symbol by hand, these are the spellings `args` and `ret` accept.

| Spelling | Meaning |
|---|---|
| `int`, `float`, `bool`, `str`, `nil` | Scalars. `nil` is a return only. A C `enum` is an `int` — status-code enums are how most libraries report failure, and on liblzma alone they account for 60 of 114 symbols. |
| `bytes` | Binary data. As an argument it is one Jade value and the two C parameters `(const void*, size_t)`. |
| `handle<T>` | An opaque pointer the library owns — a `sqlite3*`, a `SNDFILE*`. Jade holds it, hands it back, and never looks inside. The type name is checked, so passing a statement where a connection belongs is a readable error rather than a crash inside the library. `T` is written the way C writes it, so a struct with no typedef of its own keeps the keyword: `handle<struct ZSTD_CCtx_s>`. |
| `out_buffer:<ctype>` | A buffer the call fills. It consumes **no** Jade argument: `x_read(handle, buf, n)` is called as `x_read(handle, n)` and hands back the bytes. Its size comes from the next declared argument, which must be an `int`. |
| `bytes_ptr` | The same, without the count, for a library that takes a blob whose extent is written inside it — every `libfdt` call takes `const void *fdt` alone. Borrowed for the call, like a `str`. |
| `inout_bytes` | A buffer the call revises in place. Your blob is copied into scratch the shim owns, and the edited copy comes back as a result — a Jade blob is immutable, so there is nothing to lend out to be written into. |
| `sized_buffer:<ctype>` | A buffer the call fills whose size only the documentation gives. You pass the count, the shim allocates it, and the whole buffer comes back. `lzma_stream_header_encode` writes exactly twelve bytes and says so nowhere a generator can read. |
| `in_struct:<Type>` | A struct the call only reads. You build it, the shim copies it into a real local of the library's type. Needs the header. A field you leave out is zero, as in C; a field the type does not have is an error. |
| `out_struct:<Type>` | A struct the call fills through a pointer. Needs the library's real header in `headers`. Only for a record *one call* fills — a struct the caller allocates and the library keeps between calls is a `held` struct instead. |
| `struct:<Type>` | A return only: the call hands the struct back by value. Needs the header. |
| `out_scalar:<ctype>` | A single value the call writes through a pointer — `int *count`. Consumes no Jade argument; comes back as part of the result. |
| `inout_scalar:<ctype>` | The same, but the caller supplies the starting value — a position the library advances. Consumes one Jade argument *and* comes back. |
| `out_handle:<T>` | A handle written through a pointer — `sqlite3_open(path, &db)`. When the symbol declares a `fails_when`, the C return is that status and the handle is the whole result; without one the return is a value and comes back beside the handle, which is how a count like `cs_disasm`'s survives. |
| `array<elem>:<count>` | A struct *field* only: a fixed-size row. `array<char>:32` reads as characters, `array<int>:24` as numbers. Not legal in an `args` list. |
| `out_str:<ctype>` | A string the call points at inside data you already gave it — `fdt_getprop_by_offset`'s `const char **namep`. Nothing was allocated, so nothing has to be released. |
| `out_alloc_str:<ctype>` | A string the library allocated and you now own. Requires `frees_with` on the symbol, naming the function that releases it. |
| `alloc_str` | A return only: the same thing as the return value rather than through a pointer — `g_strdup`, `curl_easy_escape`. Requires `frees_with`. The shim copies the string out and hands the original straight back to that function, so nothing accumulates. |
| `ret_len:<ctype>` | Marks the parameter that says how long a returned pointer is. The return type is then `bytes`. `fdt_getprop` is this shape. |
| `callback:<ret>(<args>)` | A Jade function the library may call **while the call runs**. A parameter may be written `category:spelling` — `int:ares_bool_t` — where the spelling is what the library declared and the category is what Jade marshals it as. A pointer written `bytes:<ctype>` takes the next parameter as its length and arrives as one blob. The signature is written in the library's own C types, e.g. `callback:int(int, const char*)`. A `void *` in it is the user-data slot C uses instead of closures; the shim accepts it and does not pass it on, because a Jade function carries its own environment. |
| `callback_data` | The library's own context slot, filled with the callback's own pointer so two outstanding registrations do not collide. Needs a `callback:` beside it. |
| `null_ptr` | A null pointer, always. For a parameter the FFI cannot carry in a position the library documents as optional — brotli's allocator hooks, where null means "use malloc". Never inferred, because a library that needs a real pointer there crashes with no diagnostic. |

A symbol may have more than one out-parameter. When it does, each needs a name to come back under, written as an `@` suffix — `out_scalar:uint64_t@progress_in`. The generator takes those from the header's own parameter names. With one out-parameter the name is optional, since there is nothing to tell it apart from.

How many things come back decides the shape of the result. Count the out-parameters, plus the C return value unless something consumed it — an `out_buffer` reads it as an element count, an `out_handle` folds it into `fails_when`. One thing is the result directly; two or more come back as a struct, with `ret` first when it is a key and then one key per out-parameter.

```jade
let d = lib.divmod(17, 5)     // int divmod(int, int, int *quot, int *rem)
print(d.ret)                   // 0
print(d.quot)                  // 3
print(d.rem)                   // 2
```

A whole symbol may also be written as the single string `"?"` — the name is known, the prototype is not. That is what `jade pkg add` writes when it finds no header, and every command that would use the binding refuses it by name.

### Who frees a string

This is the one thing a C header genuinely cannot tell you, and it is the largest single class of symbol the generator declines to guess at — 125 of glib's, `g_strdup` and `g_uri_escape_string` among them.

Compare two functions that are written identically:

```c
const gchar *g_basename  (const gchar *file_name);   // points into its argument
gchar       *g_strdup    (const gchar *str);         // mallocs a new one for you
```

The first hands back a pointer into memory you already had. Nothing was allocated, so nothing has to be released, and Jade copies the text and walks away. The second allocated it for you, and if nobody hands it back to `g_free` it is gone for the life of the process. Both are a pointer to characters. Only the documentation says which is which, and `const` is a convention here rather than a rule.

So Jade asks you, and the two answers are one word apart:

```toml
[dependencies.glib.symbols.g_basename]
args = ["str"]
ret  = "str"                 # the library keeps it

[dependencies.glib.symbols.g_strdup]
args = ["str"]
ret  = "alloc_str"           # you now own it
frees_with = "g_free"
```

With `alloc_str`, the shim copies the string before returning and hands the original straight to `frees_with`. Nothing leaks and nothing is held past the call. `frees_with` names any function that takes a pointer — `free` when the library documents plain malloc, `g_free`, `curl_free`, `ares_free_string`. It does not have to be a symbol Jade bound; the shim calls it directly.

Guessing either way is worse than asking. Reading `g_strdup` as borrowed leaks its allocation on every call, and reading `g_basename` as owned frees memory the library never gave you.

### Fixed-size array fields

A C struct often holds a fixed-size row rather than a pointer — `char mnemonic[32]`, `uint8_t bytes[24]`, `int reserved[4]`. Those come back as a Jade array, and the element type decides what is in it: plain `char` is characters, everything else is numbers.

Nothing is trimmed. A `char[32]` holding `push` arrives as thirty-two characters, the NUL padding included, because trimming would be guessing where the text stops. `int(c) == 0` is how you find that yourself:

```jade
fn text(row) {
    let s = ""
    for ch in row {
        if int(ch) == 0 {
            break
        }
        s = s + ch
    }
    return s
}
```

Writing one back is bounded. A row longer than the field is an error naming the field rather than a silent truncation; a shorter one fills the rest with zeros, which is what leaving a field out already does. A character that does not fit in a byte is refused too — every byte is a character, but not every character is a byte.

### Reading a row of structs

A library that produces many structs hands back a pointer to the first and says how many. `<T>_at(handle, i)` reads one of them:

```jade
use capstone

let h = capstone.cs_open(3, 8)                       // x86, 64-bit
let r = capstone.cs_disasm(h.out, code, 0x1000, 0)

let i = 0
while i < r.ret {
    let insn = capstone.cs_insn_at(r.out, i)
    print(f"{text(insn.mnemonic)} {text(insn.op_str)}")
    i = i + 1
}
capstone.cs_free(r.out, r.ret)
```

The index is not checked against the count, and cannot be — the count came back on the Jade side, and reading past it is the same trust the library already asks of a C caller.

### A struct Jade holds

Some structs cannot be passed by value in either direction. `lzma_stream`, `ZSTD_outBuffer` and `fd_set` are allocated by the caller and kept by the library between calls, so a fresh zeroed local each time would throw away the state the last call left.

Marking one `held = true` in its struct table gives you four extra calls in the package: `<T>_new`, `<T>_free`, `<T>_get` and `<T>_set`. The struct is allocated once and every call gets the same pointer.

The pointer fields such a struct keeps its position in are the reason it exists, so they can be filled too. A read-only one gets `<T>_set_<field>`, taking a blob. A writable one gets `<T>_alloc_<field>`, taking a size, and `<T>_take_<field>`, taking how many bytes to read back — two calls, because how much of the buffer became real is something only you can work out from the fields.

```jade
use lzma

let s = lzma.lzma_stream_new()
lzma.lzma_easy_encoder(s, 6, 0)
lzma.lzma_stream_set_next_in(s, data)
lzma.lzma_stream_alloc_next_out(s, 4096)
lzma.lzma_code(s, 3)                        // LZMA_FINISH

let st = lzma.lzma_stream_get(s)
let packed = lzma.lzma_stream_take_next_out(s, 4096 - st.avail_out)
lzma.lzma_end(s)
lzma.lzma_stream_free(s)
```

A symbol may also declare `fails_when`, naming how it reports failure: `null`, `negative`, `nonzero`, `zero`, or `never`. The shim then clears `errno`, tests the return, and turns a failure into a catchable Jade error carrying the reason. Without it a failed call gives back its raw sentinel and the reason the library already recorded is thrown away — the program sees `-1` and nothing else. The default is "cannot fail", because reading a convention that is not there would turn every legitimate `-1` into a raise.

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

## Shipping what you build

`jade build` writes a `libs/` directory beside the artifact holding the dependencies it needs, and the pair travels together:

```sh
jade build main.jde -o dist/app
# built: dist/app (with fastmath-1.2.0 in dist/libs/)
```

Move `dist/` anywhere, onto any machine of the same platform, and it runs. Copy `dist/app` on its own and it will not — the dependencies are beside it, not inside it.

That is worth stating plainly because `-o` names a file and now produces a directory's worth of them. If your release process copies one path, copy the directory.

A program with no dependencies writes no `libs/` and is a single file, exactly as before.

### One copy of a dependency, per program

Two packages that both use `fastmath` share one copy of it. Not as an optimisation — as a rule.

A second copy would be a second instance: its own globals, its own module top level run a second time. For a library that owns a device, a graphics context, or a connection pool, two instances is two devices, and the resulting bug lives in the operating system rather than in your program.

Jade guarantees one copy by giving the whole program one libraries directory to resolve against, rather than letting each package look beside itself. The program's host — a compiled binary, or the `jade` CLI — decides which directory that is before anything loads.

Two consequences follow, and both are deliberate:

- **Two versions of one dependency is an error**, not a silent pick. `jade.lock` records one version per name, and a program that somehow reaches two copies raises rather than loading both.
- **A dependency your program cannot find fails loudly**, naming the directory it searched and where that directory came from. There is no second place to look, because a second place is a second copy.

### A package brings its own dependencies

Adding a package adds what it needs:

```sh
jade pkg add plotting --url https://example.com/plotting.dylib --version 2.1.0
# added plotting
# plotting also needs fastmath
```

A `jade build --lib` artifact carries the lock it was built against, so a package can say what it depends on and `jade pkg add` reads it. The entries go into your `jade.toml` as ordinary dependencies — a transitive dependency is a real dependency, and the manifest is what you read to know what your project uses.

### When two packages disagree about a version

One version of a dependency is loaded per program, so two packages naming different versions have to become one. The higher of the two wins:

```sh
jade pkg add charts --url https://example.com/charts.dylib --version 1.0.0
# added charts
# using fastmath 2.1.0 over the 1.9.0 this project had
```

That is the only choice available without a registry. There is no third version to go and fetch — Jade has never been told one exists — so the pick is between the two already named. Go resolves versions the same way and for the same reason.

It is always said out loud, never done quietly, because one of the two packages is now running against something other than what it asked for. If that version removed something the package uses, you get a missing symbol when it loads. Naming the substitution is what makes that traceable back here.

Two versions are only ordered when both come from a URL and both are written as dotted numbers. `2.0-beta` orders against nothing, and neither does the `local` a path dependency carries — those are refused, naming both, and you decide.

This is a *choice between two*, not version solving. Solving searches a space of candidates to satisfy a set of ranges, which needs ranges and a registry to enumerate. Jade has neither, and a range in a `version` is rejected outright.

Only a `url` dependency travels this way. A `path` names a file on the machine that built the package, and that path means nothing on yours — those are named for you to add yourself, rather than written as a reference that resolves to the wrong file or to none.

Reading the record does not run any of the package's code. A Jade package runs its module top level from `jade_pkg_init`, and `jade pkg add` never calls it.

### A library can keep your callback

A Jade function given to a C library stays valid after the call that handed it over, so a library that *stores* it and calls back later works — an async request, a watcher, an event handler:

```jade
use cares

cares.ares_library_init(1)
let ch = cares.ares_init()

fn on_answer(status, timeouts, answer) {
    print(f"got {answer.len()} bytes")
}

cares.ares_search(ch, "example.com", 1, 1, on_answer)

// The answer arrives during a later call entirely.
let r = cares.fd_set_new()
let w = cares.fd_set_new()
while cares.ares_fds(ch, r, w) > 0 {
    cares.ares_process(ch, r, w)
}
```

Three things are worth knowing about it.

**Your callback runs while some native call is in flight.** The interpreter services it from the call it is parked in — `ares_process` above. A library that calls back from a thread of its own, with no Jade call running, gets a neutral answer instead: that is not supported, and it fails rather than hanging.

**A registration lasts until the program ends.** Nothing in C says when a library is finished with a stored callback, so there is no moment at which releasing it would be safe. The cost is one small allocation per call that passes a function, not per invocation.

**One registration per symbol, unless the library offers somewhere to put a cookie.** Calling `ares_search` twice with two different Jade functions sends both answers to the second. Where the library has a context parameter beside the callback — most do — write `callback_data` for it in place of `null_ptr` and each registration gets its own function back. The binding report says so against any symbol taking a callback.

A callback registered in one task is not serviced in another: a spawned task has its own registrations, so a cross-task callback finds nothing and gets the neutral answer rather than running against another task's variables.

### `JADE_LIBS`

Set it to point a program at a different libraries directory:

```sh
JADE_LIBS=/opt/jade-libs ./app
```

A value you set always wins, and nothing overwrites it. That matters for the case with no Jade program in it at all: a C or Python process that loads a Jade package has no `jade` host to decide a root, so setting the variable is the only way to give that process one.

The cost of winning is that it also has to be right. A `JADE_LIBS` missing a dependency fails rather than quietly falling back to the bundle — falling back would mean two directories in play, which is the two-copies bug.

## The FFI's limits

The native ABI carries `int`, `float`, `bool`, `str`, and `nil`; arrays, dicts, and structs since v1.1.31; `bytes` since v1.2.2; and opaque handles since v1.3.0 — all in both directions. A struct crosses with its type name attached, so the receiving side can tell a `Config` from anything else shaped like one.

A **function** crosses in one direction only. You can pass one in as a callback, and the library invokes it while your call runs. A package cannot hand one back, because a C function is not something a Jade program can hold.

What still does not cross: **futures and prompts**, which arrive as `nil`.
