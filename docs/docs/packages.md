---
id: packages
title: Packages
sidebar_label: Packages
---

A Jade project depends on *prebuilt native shared libraries*. You declare a dependency in `jade.toml`, Jade pins it in `jade.lock`, and installs it into a project-local `libs/` directory.

```sh
jade pkg add fastmath --url 'https://example.com/fastmath-{platform}.so' --version 1.2.0
```

```jade
use fastmath

print(fastmath.triple(14))
```

## There is no registry

A dependency names *where it lives*, either a URL or a local path, rather than an entry in a central index. That is deliberate, and it has three consequences worth knowing up front.

*No transitive resolution.* A `.so` carries no manifest, so Jade cannot discover that one package needs another. `jade.lock` is a flat list. A package with its own dependencies has to say so in its documentation.

*No version ranges.* With no index to resolve `^1.2` against, a dependency names one exact version. Writing `version = "^1.2"` is rejected when the file is parsed, rather than quietly treated as a literal string.

*`jade pkg update` reconciles, it does not discover.* It resolves the lock against the manifest again and re-fetches. To move to a new version, edit `jade.toml`, or run `jade pkg add` again with a new `--version`.

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

`jade run`, `jade test`, and `jade build` install anything missing on their own, so a fresh clone needs no separate step.

:::warning The package commands are nested
Every one of them is `jade pkg <something>`. There is no bare `jade add`, `jade install`, or `jade update`.
:::

:::note
`jade pkg update` manages *dependencies*. `jade upgrade` updates the *jade toolchain itself*. The two are unrelated. See the [CLI reference](cli#jade-upgrade).
:::

## Platforms

A shared library is built for one operating system and one architecture. A lockfile naming a single artifact would therefore only be valid on the machine that wrote it. So when Jade generates the lock, it expands a `{platform}` URL across every supported platform and records *all* of their checksums:

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

*Jade downloads only the artifact matching your machine.* The other entries are a few hundred bytes of text each. They are what lets a lock committed from a Mac be installed *and verified* on Linux CI, with no registry to ask for the Linux checksum at install time. A Homebrew formula's `bottle` block works the same way.

A package that ships for only some platforms is fine. The missing ones are simply absent from the lock, and installing on one of them fails with a message naming what *is* available.

Supported tags: `darwin-aarch64`, `darwin-x86_64`, `linux-aarch64`, `linux-x86_64`.

## Integrity

Jade verifies every artifact against its SHA-256 in `jade.lock` on *every* install, not only on the first download. Loading a `.so` means calling `dlopen`, which runs arbitrary code before any Jade code starts. So an artifact being *present* is not a reason to trust it. A checksum mismatch is refused, and nothing is written to `libs/`.

Checksums live in the lock rather than the manifest. `jade pkg add` computes them on the first fetch, exactly as Cargo does.

## Local `path` dependencies

A `path` dependency points at a file you build yourself. It is the one source that can legitimately change while the lock stays correct, so Jade treats it differently. *It re-hashes the source file on every install and every run.* If the file has changed, Jade re-pins the lock and copies the new artifact into `libs/`.

`--version` is optional here, and the lock records `local` when you leave it out. A `--url` dependency must name a version, because the version is what makes its directory under `libs/` unique.

```
$ jade run main.jde
note: re-pinned engine (local source changed)
```

A URL dependency is never re-pinned. It either serves the bytes the lock names or it does not, and quietly re-pinning it would defeat the point of a lock. Moving a URL dependency to different bytes is what `jade pkg update` is for.

`jade pkg list` marks a local dependency whose source has moved ahead of its pin:

```
engine 1.0.0  [jade]  installed (local source changed — run `jade pkg install`)
```

Under `--locked`, that same drift is an error rather than a fix, because a rebuilt library means the committed lock is stale:

```
$ jade pkg install --locked
error: dependency 'engine': the local source has changed since jade.lock was written
  locked 462fc9e8…
  on disk f2d2eb23…
--locked forbids rewriting the lock. Run `jade pkg install` and commit jade.lock,
or rebuild the source to match.
```

:::note
Before v1.1.35, Jade ignored a rebuilt local dependency. Installing compared `libs/` against the lock, found a match, and kept loading the copy taken when the dependency was first added. Only running `jade pkg add` again picked up the new build.
:::

## Committing

Commit `jade.lock`. Do not commit `libs/`, which `jade new` already adds to `.gitignore`. The lock is what travels between machines, and the binaries are fetched again from it.

## Using a plain C library

A library such as `libsqlite3` exports no `jade_pkg_init`, so the loader cannot take it directly. Jade generates a small binding shim that wraps it into an ordinary Jade package. Since v1.3.0 that happens on its own, so adding a C library uses the same command as adding a Jade one:

:::note
Build the library from the `.c`, with `-dynamiclib` on macOS or `-shared` on Linux:

```sh
clang -dynamiclib -o libdemo.dylib demo.c    # macOS
cc -shared -fPIC -o libdemo.so demo.c        # Linux
```

Naming the header instead, as in `clang -o libdemo.dylib demo.h`, produces a precompiled header. That is an ordinary file with an ordinary name, and nothing the loader can open. Jade refuses it when you add it, and says why.
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

Three things happened there, and none of them needed a flag.

*The kind of library came from the artifact.* A Jade package exports `jade_pkg_init` and a C library does not. Both are a `.dylib`, so the filename could never tell you which is which. This is also the same symbol the loader requires at run time, so what Jade detects here is exactly what `use` will accept later.

*The header was found.* The name `libdemo` implies `demo.h`, and the search covers pkg-config, the usual include roots, and the macOS SDK. Jade accepts a candidate only if the library really exports what the header declares. So a header belonging to some other library of the same name is refused now, rather than showing up later as a linker error.

*The symbol table was generated and the shim was built.* `use demo` works right away.

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

Sometimes there is no header to find, either because someone handed you the library or because its headers were never installed. Jade still writes a manifest, because a library always lists what it *exports*:

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

A `"?"` means *the name is known and the prototype is not*. Replace it with the real prototype and the dependency works:

```toml
[dependencies.demo.symbols.demo_add]
args = ["scalar:int", "scalar:int"]
ret  = "scalar:int"
```

Filling in blanks in a file that already lists every function is easier than hunting for a header. That is why Jade writes the names rather than writing nothing.

*Write the C type here, not the Jade one.* Use `scalar:int` rather than plain `int`. That is the one thing this case asks of you that the header case does not.

With a header, `int` means only "carry this as a Jade integer", and the header's own prototype settles how wide the value is. With no header there is no such prototype, so the shim has to write one. In that case `int` would become `int64_t`, `float` would become `double`, and `bool` would become `uint8_t`. Those are Jade's widths, not the library's.

That is why Jade refuses all three outright in a dependency with no header. Nothing would catch a width the library never agreed to. The manifest would be valid, the shim would compile, and the program would run.

Reading is where a wrong width hurts most. A function that returned four bytes, declared as returning eight, hands back whatever was left in the upper half of the register. A `float` read as a `double` is not an approximate number, it is a meaningless one, and that happens on every machine rather than only on unlucky ones.

So `scalar:<ctype>` takes the library's own spelling, such as `int`, `unsigned`, `long`, `size_t`, `int32_t`, `uint64_t`, `float`, `double`, or `bool`. The shim converts to and from Jade's width at the boundary. Your side of the call does not change, and an argument is still an ordinary Jade int, float, or bool. Everything else in the vocabulary crosses as an address, and an address has one width, so nothing else needs to be spelled out.

```
$ jade run app.jde
dependency 'demo': symbol 'demo_add' declares `int` in args and `int` as its return, and demo
has no header. …
  Point at the library's header, which settles every symbol at once:
    jade pkg bind demo --header <its header.h>
  Or name the C type this one really has:
    [dependencies.demo.symbols.demo_add]
    args = ["scalar:<ctype>", "scalar:<ctype>"]
    ret  = "scalar:<ctype>"
```

*Why Jade cannot just read the types out of the library.* A shared library carries an export table of *names*. C keeps no argument or return types in a compiled artifact, so `demo_add` in that table says only that a `demo_add` exists. Types survive in DWARF debug information, which release builds strip, and which the macOS linker leaves in the `.o` files rather than in the library. The missing half is therefore genuinely gone, and Jade will not guess. A wrong prototype shows up as a corrupted stack several calls later, with nothing pointing back at the manifest.

`jade check`, `jade run`, and `jade build` all refuse a dependency that still holds a `"?"`, and they name the symbols involved.

### Calling a symbol that is not there

The `symbols` table is the complete list of what the shim binds, so `jade check` and `jade build` use it to check your calls. A name the table does not declare is a compile error, reported with the line and a suggestion:

```
$ jade check main.jde
main.jde: [4:7] 'gfx' has no symbol 'jade_gfx_key_press' — did you mean 'jade_gfx_key_pressed'?
```

When no declared name is close enough to suggest, the message names the manifest instead of guessing:

```
[4:7] 'gfx' has no symbol 'jade_gfx_render'. Add it to [dependencies.gfx.symbols] in
jade.toml, or re-run `jade pkg bind gfx --header <h>`
```

The same check covers `from gfx use <name>`, which is reported at the import line.

:::note Why this needs its own check
A mistyped symbol is not a link error, because nothing links it. The shim binds the names in the table and no others, so a name that is not in the table is simply missing at run time. Until v1.3.24 that meant a typo compiled, built, linked, packaged, shipped, and then failed the first time that line ran, reported as "dict has no key or method". The manifest lists what the library provides, so the answer was always there. Nothing was reading it.
:::

The check applies to `abi = "c"` dependencies, which are the ones whose manifest declares a symbol table. A Jade package declares its exports inside its own project, which your manifest cannot see, so its calls are not checked this way.

### Libraries split across several headers

Plenty of libraries have more than one header. libarchive declares its readers in `archive.h` and its entries in `archive_entry.h`. libgit2 puts its types in `git2/types.h` and its functions across twenty other files. Two things make those work.

*Bind each header in turn.* `jade pkg bind` merges its results, so a second run adds to the table rather than replacing it, and the header list grows alongside.

```sh
jade pkg add archive --path libarchive.dylib --header /opt/homebrew/include/archive.h
jade pkg bind archive --header /opt/homebrew/include/archive_entry.h
```

```toml
headers = ["archive.h", "archive_entry.h"]
```

The shim `#include`s every header in that list. Suppose one went missing while its symbols stayed in the table. C would let the shim call them undeclared, assuming each returns `int`, and a call that really returns a pointer would come back truncated. The shim is compiled with `-Werror=implicit-function-declaration`, so that cannot happen quietly. You get an error naming the missing header instead.

*Types are read from the whole include tree, and so are functions the library exports.* A function in `archive.h` written in terms of a type from `archive_entry.h` binds without trouble. `archive.h` also includes `stdio.h`, and binding `fopen` along with it would be wrong. So Jade filters everything a header includes through the library's own export table. `fopen` belongs to no library here, while `archive_entry_new` is in the artifact and gets bound. The header you name always contributes its own declarations, export table or not.

Point at the top header of a library that splits its API across files, and you get the whole library. `ares.h` declares about seventy symbols and includes `ares_dns_record.h`, which declares sixty-three more.

### Umbrella headers

Some libraries offer only a header that declares nothing at all. `lzma.h`, `git2.h`, and `alsa/asoundlib.h` exist to include the twenty files that do the real declaring. Point at one of those and there is nothing in it to bind. Pointing at a sub-header usually fails too, because a sub-header on its own does not compile.

The same rule handles them, which is why they work. Everything is swept in from the includes, and the umbrella stays the header the shim includes. This is the one case that *needs* the artifact. With no export table, there is nothing exact to test an include against, so a header with declarations of its own binds only those, and an umbrella header gives an error asking for `--path`.

```
$ jade pkg add lzma --path /opt/homebrew/lib/liblzma.dylib --header /opt/homebrew/include/lzma.h
covers 49 of the 114 symbols the library exports
49 bound, 8 assumed, 65 skipped; 3 struct(s)

that header declares nothing itself, so the 114 declarations it includes that
the library also exports were bound instead.
```

This needs the artifact, so `--path` has to be there too. An export table is an exact test, rather than a guess about which directories count as system ones. `fopen` is declared in that translation unit and is not in liblzma, so Jade does not bind it.

### Headers that include their neighbours

Almost no header stands alone. A header reaches its neighbours in two ways, and each way needs a different directory. Jade searches both for you.

```c
/* libfdt.h: the file sits right beside this one */
#include <libfdt_env.h>

/* brotli/encode.h: resolved against the directory above this one */
#include <brotli/port.h>
```

An angled include does not search the including file's own directory, so Jade passes that directory explicitly. The second form needs the parent directory as well. Both go into `include_dirs`, so compiling the shim sees exactly what reading the header saw.

### When you do need a flag

| Situation | Flag |
|---|---|
| The header search missed, or you want a specific one | `--header <file.h>` |
| A header lives somewhere neither rule above finds | `-I <dir>` (repeatable) |
| The dependency comes from `--url`, so there is no local file to read | `--c-abi` |

A directory you name with `-I` is searched before either of the guessed ones, because a broad include root can otherwise hide the header you meant.

### Where binding happens

Binding runs during `add` and `install`, not only during `bind`.

- *`jade pkg add`* binds whenever it finds a header or you give it one.
- *`jade pkg install`* fills in any dependency that names a header but has no `symbols` yet. It leaves a manifest that already carries its symbols alone, so a fresh clone installs without needing clang.
- *`jade pkg install --locked`* never binds, because a reproducible install must not depend on what the local clang makes of a header.
- *`jade pkg bind`* is for the cases with a real decision behind them: re-running after a header changes, or narrowing a large header with `--only`. It merges into the existing table rather than replacing it, and `--dry-run` shows the report without touching `jade.toml`.

Binding a C library needs `clang` on your `PATH` to read the header, and a C compiler called `cc` to build the shim.

### The report is the feature

No generator binds everything. One that quietly covers two thirds of an API is how the missing third gets discovered at run time. So the output says what it dropped, and why:

```
assumed (check these):
  demo_read: `void *` next to a length was read as a buffer the call fills;
             if the library reads it instead, change it to `bytes`

skipped:
  1: returns an unsupported type `void *`
      demo_raw
  1: takes varargs
      demo_printf
```

Coverage is quoted against the library's own export table, as in "covers 181 of the 194 symbols the library exports". A bare "181 bound" would read as success whether the library has 190 entry points or 900.

A symbol the header declares but the library does not export is dropped too. A header is written for the newest version, while the artifact you have may have been built without part of it. libbrotlienc's header declares two such functions. Binding one produces a shim that compiles and then fails to *link*, and the linker takes the whole dependency down with it.

A symbol that cannot be bound is dropped by itself. It used to take the whole dependency down with it. The generator would emit a symbol that fills a struct while dropping that struct's field table, and the shim refuses a reference to a table that is not there. So one opaque blob among two hundred good symbols made a library impossible to install. `sqlite3_snapshot_free` and `zip_file_attributes_init` are both that shape.

### The binding vocabulary

If you write or correct a symbol by hand, these are the spellings that `args` and `ret` accept.

| Spelling | Meaning |
|---|---|
| `int`, `float`, `bool`, `str`, `nil` | Scalars. `nil` is a return only. A C `enum` is an `int`. Status-code enums are how most libraries report failure, and on liblzma alone they account for 60 of 114 symbols. The first three say only how Jade carries the value, so they need a header to settle the width. Without a header they are refused, and `scalar:<ctype>` is what to write instead. |
| `scalar:<ctype>` | The same value, with the library's own C type named, such as `scalar:int`, `scalar:size_t`, or `scalar:float`. The shim declares that type and converts at the boundary. Your side of the call is still an ordinary Jade int, float, or bool. Required in a dependency with no header, and allowed in one with a header. Takes any numeric or boolean C spelling. |
| `bytes` | Binary data. As an argument it is one Jade value and the two C parameters `(const void*, size_t)`. |
| `handle<T>` | An opaque pointer the library owns, such as a `sqlite3*` or a `SNDFILE*`. Jade holds it, hands it back, and never looks inside. The type name is checked, so passing a statement where a connection belongs gives a readable error rather than a crash inside the library. Write `T` the way C writes it, so a struct with no typedef of its own keeps the keyword, as in `handle<struct ZSTD_CCtx_s>`. |
| `out_buffer:<ctype>` | A buffer the call fills. It consumes *no* Jade argument, so `x_read(handle, buf, n)` is called as `x_read(handle, n)` and hands back the bytes. Its size comes from the next declared argument, which must be an integer, written either as `int` or as a `scalar:<ctype>` naming one. |
| `bytes_ptr` | The same thing without the count, for a library that takes a blob whose length is written inside it. Every `libfdt` call takes `const void *fdt` alone. Borrowed for the call, like a `str`. |
| `inout_bytes` | A buffer the call revises in place. Your blob is copied into scratch memory the shim owns, and the edited copy comes back as a result. A Jade blob is immutable, so there is nothing to lend out to be written into. |
| `sized_buffer:<ctype>` | A buffer the call fills whose size only the documentation gives. You pass the count, the shim allocates it, and the whole buffer comes back. `lzma_stream_header_encode` writes exactly twelve bytes and says so nowhere a generator can read. |
| `in_struct:<Type>` | A struct the call only reads. You build it, the shim copies it into a real local of the library's type. Needs the header. A field you leave out is zero, as in C; a field the type does not have is an error. |
| `out_struct:<Type>` | A struct the call fills through a pointer. Needs the library's real header in `headers`. Use it only for a record that *one call* fills. A struct the caller allocates and the library keeps between calls is a `held` struct instead. |
| `inout_struct:<Type>` | A struct the call reads *and* writes. This is the `init`, `update`, `final` shape, where each call carries forward the last one's work. It consumes one Jade argument and comes back, exactly as `inout_scalar` does. Needs the header. Binding one of these as `out_struct` hands every call a fresh zeroed state and throws the previous call's work away, while every call still reports success. |
| `struct:<Type>` | A return only: the call hands the struct back by value. Needs the header. |
| `out_scalar:<ctype>` | A single value the call writes through a pointer, such as `int *count`. It consumes no Jade argument and comes back as part of the result. |
| `inout_scalar:<ctype>` | The same, except the caller supplies the starting value, such as a position the library advances. It consumes one Jade argument *and* comes back. |
| `out_handle:<T>` | A handle written through a pointer, as in `sqlite3_open(path, &db)`. When the symbol declares a `fails_when`, the C return is that status and the handle is the whole result. Without one, the return is a value and comes back beside the handle, which is how a count like the one from `cs_disasm` survives. |
| `array<elem>:<count>` | A struct *field* only: a fixed-size row. `array<char>:32` reads as characters, `array<int>:24` as numbers. Not legal in an `args` list. |
| `out_str:<ctype>` | A string the call points at, inside data you already gave it. The `const char **namep` in `fdt_getprop_by_offset` is one. Nothing was allocated, so nothing has to be released. |
| `out_alloc_str:<ctype>` | A string the library allocated and you now own. Requires `frees_with` on the symbol, naming the function that releases it. |
| `alloc_str` | A return only. The same thing as the return value rather than through a pointer, as in `g_strdup` and `curl_easy_escape`. Requires `frees_with`. The shim copies the string out and hands the original straight back to that function, so nothing accumulates. |
| `ret_len:<ctype>` | Marks the parameter that says how long a returned pointer is. The return type is then `bytes`. `fdt_getprop` is this shape. |
| `callback:<ret>(<args>)` | A Jade function the library may call *while the call runs*. Write a parameter as `category:spelling`, such as `int:ares_bool_t`, where the spelling is what the library declared and the category is how Jade marshals it. A pointer written `bytes:<ctype>` takes the next parameter as its length and arrives as one blob. Write the signature in the library's own C types, such as `callback:int(int, const char*)`. A `void *` in that signature is the user-data slot C uses in place of closures. The shim accepts it and does not pass it on, because a Jade function carries its own environment. |
| `callback_data` | The library's own context slot, filled with the callback's own pointer so two outstanding registrations do not collide. Needs a `callback:` beside it. |
| `null_ptr` | Always a null pointer. Use it for a parameter the FFI cannot carry, in a position the library documents as optional. Brotli's allocator hooks are one, where null means "use malloc". Never inferred, because a library that needs a real pointer there crashes with no diagnostic. |

A symbol may have more than one out-parameter. When it does, each one needs a name to come back under, written as an `@` suffix, such as `out_scalar:uint64_t@progress_in`. The generator takes those names from the header's own parameter names. With a single out-parameter the name is optional, because there is nothing to tell it apart from.

How many things come back decides the shape of the result. Count the out-parameters, and add the C return value unless something already consumed it. An `out_buffer` reads the return as an element count, and an `out_handle` folds it into `fails_when`. If one thing comes back, that is the result. If two or more come back, they arrive as a struct, with `ret` first when it is a key, and then one key for each out-parameter.

```jade
let d = lib.divmod(17, 5)     // int divmod(int, int, int *quot, int *rem)
print(d.ret)                   // 0
print(d.quot)                  // 3
print(d.rem)                   // 2
```

A whole symbol may also be written as the single string `"?"`, meaning the name is known and the prototype is not. That is what `jade pkg add` writes when it finds no header, and every command that would use the binding refuses it by name.

### Who frees a string

This is the one thing a C header genuinely cannot tell you, and it is the largest single group of symbols the generator refuses to guess at. Glib alone has 125 of them, including `g_strdup` and `g_uri_escape_string`.

Compare two functions that are written identically:

```c
const gchar *g_basename  (const gchar *file_name);   // points into its argument
gchar       *g_strdup    (const gchar *str);         // mallocs a new one for you
```

The first hands back a pointer into memory you already had. Nothing was allocated, so nothing has to be released. Jade copies the text and walks away. The second allocated memory for you, and if nobody hands it back to `g_free`, it is lost for the life of the process. Both are a pointer to characters. Only the documentation says which is which, and `const` is a convention here rather than a rule.

So Jade asks you. The two answers are one word apart:

```toml
[dependencies.glib.symbols.g_basename]
args = ["str"]
ret  = "str"                 # the library keeps it

[dependencies.glib.symbols.g_strdup]
args = ["str"]
ret  = "alloc_str"           # you now own it
frees_with = "g_free"
```

With `alloc_str`, the shim copies the string before returning and hands the original straight to `frees_with`. Nothing leaks, and nothing is held past the call. `frees_with` names any function that takes a pointer, such as `free` when the library documents plain malloc, or `g_free`, `curl_free`, or `ares_free_string`. It does not have to be a symbol Jade bound, because the shim calls it directly.

Guessing either way is worse than asking. Reading `g_strdup` as borrowed leaks its allocation on every call. Reading `g_basename` as owned frees memory the library never gave you.

### Fixed-size array fields

A C struct often holds a fixed-size row rather than a pointer, such as `char mnemonic[32]`, `uint8_t bytes[24]`, or `int reserved[4]`. Those come back as a Jade array, and the element type decides what is inside. Plain `char` gives characters, and everything else gives numbers.

Nothing is trimmed. A `char[32]` holding `push` arrives as thirty-two characters, NUL padding included, because trimming would mean guessing where the text stops. Test `int(c) == 0` to find the end yourself:

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

Writing a row back is bounded. A row longer than the field gives an error naming the field, rather than a silent truncation. A shorter row fills the rest with zeros, which is what leaving a field out already does. A character that does not fit in one byte is refused too. Every byte is a character, but not every character is a byte.

### Reading a row of structs

A library that produces many structs hands back a pointer to the first one and says how many there are. `<T>_at(handle, i)` reads one of them:

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

Jade does not check the index against the count, and it cannot. The count came back on the Jade side, and reading past it asks for the same trust the library already expects from a C caller.

### A struct Jade holds

Some structs cannot be passed by value in either direction. `lzma_stream`, `ZSTD_outBuffer`, and `fd_set` are allocated by the caller and kept by the library between calls. A fresh zeroed local on each call would throw away the state the previous call left behind.

Marking one `held = true` in its struct table gives you four extra calls in the package: `<T>_new`, `<T>_free`, `<T>_get`, and `<T>_set`. The struct is allocated once, and every call gets the same pointer.

Such a struct exists to hold its position in a set of pointer fields, so you can fill those too. A read-only field gets `<T>_set_<field>`, which takes a blob. A writable field gets two calls instead: `<T>_alloc_<field>`, which takes a size, and `<T>_take_<field>`, which takes how many bytes to read back. It takes two calls because only you can work out, from the other fields, how much of the buffer became real.

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

A symbol may also declare `fails_when`, which names how it reports failure. The choices are `null`, `negative`, `nonzero`, `zero`, and `never`. The shim then clears `errno`, tests the return value, and turns a failure into a catchable Jade error carrying the reason.

Without `fails_when`, a failed call hands back its raw sentinel and throws away the reason the library already recorded. The program sees `-1` and nothing more. The default is "cannot fail", because assuming a convention that is not there would turn every legitimate `-1` into a raise.

Five rules are worth knowing before you write a symbol by hand.

- *At most one out-parameter per symbol.* Two would have to come back as a pair with no obvious names.
- *A symbol with both an out-parameter and a return value comes back as `.ret` and `.out`.* When the C function returns `void` there is no pair to make, so the filled value is the result directly.
- *Jade never closes a handle for you.* It reclaims its own wrapper and leaves the pointer alone, because it cannot know what the pointer is or which allocator made it. Closing is a call the binding exposes.
- *A handle cannot cross into a task.* Jade cannot tell a thread-safe library from an unsafe one, so it refuses at compile time rather than letting a race happen quietly. Open the handle inside the task and close it before returning.
- *A callback is live only while the call that passed it is running.* A library that stores one and invokes it later is not supported. A raise inside a callback is delayed, so the library finishes cleanly and the error reaches your `catch` afterwards.

A symbol using anything outside this vocabulary is rejected *by name* at install time, rather than quietly turned into nil.

## Publishing a Jade package

`jade build --lib` compiles a Jade file to a shared library exporting `jade_pkg_init`:

```sh
jade build mathlib.jde --lib               # -> mathlib.dylib (or .so)
jade build mathlib.jde --lib --export add  # bind only `add`
```

Jade has no `pub` keyword, and every top-level function is public, so the default is to export all of them. `--export` narrows the list.

Publish the result wherever you like, and GitHub Releases is the natural home. Ship one build per platform, named so a `{platform}` URL finds each of them. Users then run `jade pkg add` on it like any other dependency.

### A package of several files

A package is not limited to one file. Every module the entry `use`s is compiled into the same artifact, each in its own namespace. So you can organize a package like any other program:

```jade
// mathlib.jde is the entry module
use geometry
use text

fn area(w, h) { return geometry.area(w, h) }
fn shout(s) { return text.shout(s) }
```

*The entry module is the package's API.* Only its top-level functions become bindings. Everything the imported modules define stays internal, which is why `area` above is a one-line forwarder. That is the same rule a single-file package follows, and it means adding a helper to `geometry.jde` never silently widens what users can call.

### Declaring the package in `jade.toml`

Instead of passing the entry and the exports on the command line every time, a package can describe itself:

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

`name` becomes the artifact's filename and the name users write in `use`, so it has to be a usable identifier made of letters, digits, and underscores.

`sources` is optional, and it is the reason to write a `[package]` section at all rather than a shell alias. The build already finds a package's files by following `use` from the entry, so the list is not what makes the build work. What the list buys you is two errors the import graph cannot raise on its own:

- a file you meant to ship but forgot to import, which would silently vanish from the artifact;
- a file that got pulled in without you deciding to ship it.

Either one fails the build, naming the file:

```
error: [package] sources in jade.toml does not match what the package imports
  declared but never imported: orphan.jde
    nothing reaches these from 'mathlib.jde', so they would not be in the artifact
```

Leave `sources` out and Jade takes the import graph at its word.

:::note
`[package]` describes a project that *is* a package. `[dependencies]` describes packages a project *uses*. A project can have both, which is simply a package that depends on another package.
:::

Nothing changes for the people using it. The artifact is an ordinary Jade package, added and locked exactly as before:

```sh
jade pkg add mathlib --url 'https://example.com/mathlib-{platform}.so' --version 1.2.0
```

## Shipping what you build

`jade build` writes a `libs/` directory beside the artifact, holding the dependencies the program needs. The two travel together:

```sh
jade build main.jde -o dist/app
# built: dist/app (with fastmath-1.2.0 in dist/libs/)
```

Move `dist/` anywhere, onto any machine of the same platform, and it runs. Copy `dist/app` by itself and it will not, because the dependencies sit beside the binary rather than inside it.

That is worth saying plainly, because `-o` names a single file and now produces a directory's worth of them. If your release process copies one path, make it copy the directory.

A program with no dependencies writes no `libs/` at all, and stays a single file, exactly as before.

### One copy of a dependency, per program

Two packages that both use `fastmath` share one copy of it. That is a rule, not an optimisation.

A second copy would be a second instance, with its own globals and its own module top level run a second time. For a library that owns a device, a graphics context, or a connection pool, two instances means two devices. The resulting bug then lives in the operating system rather than in your program.

Jade guarantees one copy by giving the whole program a single libraries directory to resolve against, rather than letting each package look beside itself. The program's host, meaning either a compiled binary or the `jade` CLI, picks that directory before anything loads.

Two consequences follow, and both are deliberate:

- *Two versions of one dependency is an error*, not a silent pick. `jade.lock` records one version per name, and a program that somehow reaches two copies raises rather than loading both.
- *A dependency your program cannot find fails loudly*, naming the directory it searched and where that directory came from. There is no second place to look, because a second place would mean a second copy.

### A package brings its own dependencies

Adding a package adds what it needs:

```sh
jade pkg add plotting --url https://example.com/plotting.dylib --version 2.1.0
# added plotting
# plotting also needs fastmath
```

A `jade build --lib` artifact carries the lock it was built against, so a package can state what it depends on and `jade pkg add` reads that. The entries go into your `jade.toml` as ordinary dependencies. A transitive dependency is a real dependency, and the manifest is what you read to know what your project uses.

### When two packages disagree about a version

A program loads one version of each dependency, so two packages naming different versions have to be reduced to one. The higher of the two wins:

```sh
jade pkg add charts --url https://example.com/charts.dylib --version 1.0.0
# added charts
# using fastmath 2.1.0 over the 1.9.0 this project had
```

That is the only choice available without a registry. There is no third version to fetch, because nothing ever told Jade one exists, so the pick is between the two already named. Go resolves versions the same way, for the same reason.

Jade always says so out loud, and never makes the swap quietly, because one of the two packages is now running against something other than what it asked for. If that version removed something the package uses, you get a missing symbol when it loads. Naming the substitution is what lets you trace the failure back to this decision.

Jade can only order two versions when both come from a URL and both are written as dotted numbers. `2.0-beta` orders against nothing, and neither does the `local` marker a path dependency carries. Those cases are refused with both versions named, and you decide.

This is a *choice between two*, not version solving. Solving searches a space of candidates to satisfy a set of ranges, which needs both ranges and a registry to enumerate them. Jade has neither, and a range written in a `version` is rejected outright.

Only a `url` dependency travels this way. A `path` names a file on the machine that built the package, and that path means nothing on yours. So Jade names those for you to add yourself, rather than writing a reference that would resolve to the wrong file or to nothing.

Reading that record runs none of the package's code. A Jade package runs its module top level from `jade_pkg_init`, and `jade pkg add` never calls it.

### A library can keep your callback

A Jade function given to a C library stays valid after the call that handed it over. So a library that *stores* the function and calls back later works. An async request, a watcher, and an event handler are all this shape:

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

Three things are worth knowing about how that works.

*Your callback runs while some native call is in flight.* The interpreter services it from whichever call it is parked in, which is `ares_process` above. A library that calls back from a thread of its own, with no Jade call running, gets a neutral answer instead. That case is not supported, and it fails rather than hanging.

*A registration lasts until the program ends.* Nothing in C says when a library is finished with a stored callback, so there is no moment at which releasing it would be safe. The cost is one small allocation for each call that passes a function, not for each time the function runs.

*One registration per symbol, unless the library gives you somewhere to store a cookie.* Calling `ares_search` twice with two different Jade functions sends both answers to the second one. Most libraries have a context parameter beside the callback. Where one does, write `callback_data` for it in place of `null_ptr`, and each registration gets its own function back. The binding report notes this against any symbol that takes a callback.

A callback registered in one task is never serviced in another. A spawned task keeps its own registrations, so a cross-task callback finds nothing and gets the neutral answer, rather than running against another task's variables.

### `JADE_LIBS`

Set it to point a program at a different libraries directory:

```sh
JADE_LIBS=/opt/jade-libs ./app
```

A value you set always wins, and nothing overwrites it. That matters most when there is no Jade program involved at all. A C or Python process that loads a Jade package has no `jade` host to pick a root, so setting the variable is the only way to give that process one.

The cost of always winning is that the value also has to be right. A `JADE_LIBS` directory missing a dependency fails, rather than quietly falling back to the bundled one. Falling back would put two directories in play, which is exactly the two-copies bug.

## The FFI's limits

The native ABI carries `int`, `float`, `bool`, `str`, and `nil`. It has carried arrays, dicts, and structs since v1.1.31, `bytes` since v1.2.2, and opaque handles since v1.3.0. All of them cross in both directions. A struct crosses with its type name attached, so the receiving side can tell a `Config` from anything else shaped the same way.

A *function* crosses in one direction only. You can pass one in as a callback, and the library invokes it while your call runs. A package cannot hand a function back, because a C function is not something a Jade program can hold.

Two things still do not cross: *futures and prompts*. Both arrive as `nil`.
