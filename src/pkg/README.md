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

**A `.so` cannot supply the header itself**, and it is worth being precise about why: a shared library carries an export table of *names*, and C does not mangle them, so `sqlite3_open` says nothing about its signature. Types survive only in DWARF, which release builds strip — and which the macOS linker leaves in the `.o` files rather than the library, so even an unstripped `-g` build usually carries none. So a header has to come from the filesystem — but the library still has the last word on *which* one. `libsqlite3.dylib` implies `sqlite3.h`, the search covers pkg-config, the usual include roots and the macOS SDK, and the candidate is accepted only if the library actually exports what it declares. A header describing some other library of the same name is refused before anything is written, rather than surfacing later as an undefined symbol from the linker.

**When no header is found, the half that *is* readable still gets written.** The export table names every function; only the types are gone. So `add` writes those names into `jade.toml` with `"?"` where the prototype belongs, and the user fills in blanks in a file that already lists the whole API rather than going to look for a header that may not exist on this machine. The placeholder is a legal manifest state on purpose — `list` and `remove` keep working — and every command that would *use* the binding refuses it by name. What it will not do is guess: a fabricated prototype is a corrupted stack several calls later, with nothing pointing back at the manifest, which is strictly worse than a blank.

That same export table gives the one number that says whether a binding is usable: **coverage**. "181 bound" reads as success whether the library has 190 entry points or 900, so the report says how many of the library's exports were covered.

## Why it was built this way

Dependencies are **prebuilt native shared libraries**, sourced from a local path or a URL. There is deliberately no package registry — like Go, a dependency names where it lives rather than an entry in a central index.

That choice has a consequence worth stating plainly: **there is no version solving**. Each dependency contributes exactly one artifact, `jade.lock` is a flat list, and "resolution" means picking the right platform build.

What *has* changed is the half of that which used to read "and no transitive resolution, because a `.so` carries no manifest of its own". A Jade package now carries one. `jade build --lib` emits `jade_pkg_deps`, a function whose whole body returns the `[[package]]` tables of the lock it was built against, and `declared_dependencies` reads it back — statically first, through `exported_symbols`, so a plain C library is never opened at all. `jade pkg add` merges what it finds into the consuming project's `jade.toml`, because a transitive dependency is a real dependency and the manifest is what a person reads to know what their project uses.

Two limits on that, and both are honest rather than temporary. Only a `url` dependency travels: a `path` names a file on the machine that built the package, which means nothing on another, so those are named for the user rather than written as a reference that resolves to the wrong file. And a name already present at a different version is refused rather than resolved — not only because there is no solver, but because two versions are two files, two paths, and therefore two loaded copies with their own state.

Reading the record runs none of the package's code, and that is load-bearing rather than incidental: a package runs its module top level from `jade_mod_init`, which `jade_pkg_init` calls and nothing else. Add a constructor anywhere in the AOT output and `jade pkg add` starts executing the package it is being asked to add, before the user has agreed to run it. There is a test asserting it by side effect, because reading the source is exactly what would stop being true.

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
  - `out_buffer:<ctype>` and `out_struct:<Type>` are out-parameters. They consume **no** Jade argument: the shim owns the memory, so `x_read(handle, buf, n)` is called from Jade as `x_read(handle, n)` and hands back the bytes. The rewrite rules are below.
  - `scalar:<ctype>` is the same value with the library's own C type named, for the case where the shim writes the declaration itself. See *A width the library never agreed to* below.

  A symbol may also declare `fails_when` — `null`, `negative`, `nonzero`, or `never`. The shim then clears `errno`, tests the return against that convention, and on failure hands back a `JADE_FFI_ERROR` carrying `strerror` text and the number, which both engines already turn into a catchable Jade raise. Without it a failed call returns its raw sentinel and the reason the library *had already recorded* is simply thrown away: the program sees `-1` and nothing else. There is no universal convention to infer, which is why the binding names the one its symbol uses; the default is "cannot fail", because reading a convention that is not there would turn every legitimate `-1` into a raise.
- **`bindgen.rs`** — generates a dependency's `symbols` and `structs` tables from its C header, driven by `jade pkg add --header`, `jade pkg install`, and `jade pkg bind`. This is what makes "bind any `.so`" true in practice: the ABI could express handles, blobs and structs, but every signature still had to be transcribed by hand, and SQLite has around 200 entry points.

  It reads the header with **clang** — `clang -Xclang -ast-dump=json -fsyntax-only`, over a pipe. Parsing C by hand is a tar pit of macros, conditionals and typedef chains, and a home-grown parser would misread far more than it read. Shelling out rather than linking `libclang` keeps a large native dependency out of the shipped binary, and costs nothing in practice: `cc` is already required to bind a C library at all.

  **The skip report is the feature.** No generator binds everything, and one that quietly covers two thirds of an API is how the missing third is found at run time. So what it drops is named with a reason, grouped so one cause reads as one fact; and a binding resting on an inference — a non-const `T*` beside a count is *almost* always an out-buffer — is listed as *assumed* rather than buried. On the real `sqlite3.h` that is 181 bound, 2 assumed, 105 skipped, and every skip is a genuine limit of the ABI rather than a gap in the reader.
- **`tests.rs`** — package manager tests, all offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`.

*Also depends on:* `clang` on `PATH`, but only when a header is actually read — `add --header`, `bind`, or an `install` filling in missing symbols. A manifest that already carries its symbols installs without it. Nothing else in the package manager needs it, and its absence is reported with the workaround (write the table by hand) rather than as a crash.

*Used by:* `cli/pkg.rs` for the commands. Indirectly, `vm/chunk.rs` and `aot/imports.rs` consume the `[lib]` entries this module contributes, without knowing they came from a dependency.

## How the shim rewrites a call

A C signature and a callable Jade signature are not the same shape, and the gap is most of what stops a real library from being bindable:

```c
int sf_read_short(SNDFILE* f, short* buf, int count);
int sf_open(const char* path, int mode, SF_INFO* info);
```

Neither can be called as written. The first wants a buffer the caller allocated and reports how much of it was filled; the second returns one thing and writes another through a pointer. A one-to-one mapping of parameters cannot express either, so the shim rewrites the call rather than merely forwarding it. The declared `args` list describes the **C** signature; the Jade signature is derived from it and is deliberately a different length.

### How much of a real library this reaches

Measured against seven Homebrew libraries, counting only what the header
declares *and* the artifact exports, because a header written for a newer
version is not a gap in the binding:

| library | bound | of | |
|---|---|---|---|
| zstd | 67 | 67 | 100% |
| liblzma | 111 | 114 | 97% |
| libfdt | 76 | 79 | 96% |
| capstone | 19 | 20 | 95% |
| brotlidec | 11 | 14 | 79% |
| brotlienc | 10 | 13 | 77% |
| c-ares | 54 | 74 | 73% |
| **all seven** | **348** | **381** | **91%** |

It was 223 of the same 381 before the v1.3.8 work, and several of those 223 were
bindings that ran and did nothing.

The refusals that remain are, with two exceptions, cases where the header does
not carry the answer — see *What the header does not say* below. Re-measure with
`jade pkg add <name> --path <lib> --header <hdr>` and read the `covers` line;
the skip report is grouped by reason and is the thing to read before adding a
rule.

### The rules

**A `bytes` argument is one Jade value and two C parameters.** It expands to
`(const void*, size_t)`, and the pointer is borrowed for the duration of the
call exactly as a `str` argument is. A nil blob passes `NULL, 0` rather than
dereferencing.

**An out-parameter consumes no Jade argument at all.** That is the rewrite that
makes `x_read(handle, buf, n)` callable as `x_read(handle, n)`.

**An `out_buffer` is the shim's memory, never Jade's.** A Jade `bytes` is
immutable — three methods, none of which writes — and letting a C library
scribble into one would break that for the FFI's convenience. So the shim
allocates the scratch, the library fills it, and Jade only ever sees the
finished blob.

Its size comes from **the next declared argument**, which must be an `int`. That
is the shape essentially every buffer-filling C function has (`read(fd, buf, n)`,
`gzread`, `fread`, `sf_read_short`), and the shim has to know how much to
allocate before it can call anything. The alternative — a separate key naming
which argument holds the count — buys nothing for the cases that exist.

**The return value of an `out_buffer` symbol is the element count, and it sizes
the blob.** It does not also come back separately, because a counted buffer
already carries its length: `b.len()` is `written * sizeof(elem)`. The count is
clamped to what was allocated, so a library reporting more than it was given
cannot make the copy read past the scratch.

**A scalar written through a pointer is an out-parameter too.** `int
*nextoffset`, `uint64_t *progress` — C's way of returning a second value, and
everywhere. `out_scalar:<ctype>` carries the library's own C type rather than a
Jade one, for the same reason `out_buffer` and `callback` do: the shim declares
a real local, so widening `uint32_t` to `int64_t` would take the address of a
differently-sized object and let the library write past it.

Some of those are read *and* written — a position the caller sets and the
library advances, `size_t *out_pos`. A zeroed local is right for one call and
wrong on the second, which shows up as corrupt output rather than as an error.
Nothing in C distinguishes the two, so `inout_scalar:<ctype>` exists for the
second, the generator emits `out_scalar` and lists it as an *assumption* naming
the fix. That mirrors the out_buffer guess exactly: the generator does not get
to dress a guess as certainty.

**More than one out-parameter is allowed, and then each carries a name.** The
rule used to be one, on the grounds that two would come back as a pair with no
obvious names. They are not nameless — the header already names them, and clang
hands the parameter names over with the types. `out_scalar:uint64_t@progress_in`
says what key the value comes back under. A symbol whose header does *not* name
its parameters is skipped rather than given invented `out0`/`out1` keys, which
was the real objection.

**How many things come back decides the shape.** Count the out-parameters, plus
the C return value when nothing has consumed it — an `out_buffer` reads it as an
element count, an `out_handle` folds it into the failure convention. One thing
is the result directly. Two or more become a struct: `ret` first when it is a
key, then one key per out-parameter in declaration order.

That counting reproduces every shape that existed before rather than replacing
it. A lone out-parameter with a `void` return is still the bare value; a lone
out-parameter beside a real return is still `.ret` and `.out`, and keeps the
name `out` because there is nothing to tell it apart from.

### Why `out_struct` requires a header

The shim has to declare a real local of the struct's type. It could synthesize
one from the declared field list — and that is exactly the wrong answer.

A synthesized layout lives in a hand-written TOML file. One wrong integer width,
one missed padding byte, one field listed out of order, and the shim reads and
writes at offsets the library does not agree with. Nothing catches it: the
manifest is valid, the shim compiles, and the program returns plausible garbage
or corrupts memory that belongs to the library.

Including the real header moves the layout to the only place it can be correct —
the C compiler. The field list then carries only names and Jade types, and a
field the struct does not have becomes a compile error naming the field.

The same reasoning is why a symbol is **not** re-declared when headers are
present. A hand-written prototype that disagrees with the real one — `int` where
the library says `long` — truncates silently at run time; letting the header win
turns that into a compile error. If you are going to require a header, requiring
it to be authoritative is the only consistent position.

The cost is real and worth stating: a dependency using `out_struct` needs the
library's development headers present at install time, and `include_dirs` when
they are not on the default search path. Anyone who has the library has them.

### Ownership at the boundary

A value **inside a container** is container-owned, so Jade's `ffi_free` frees
it. A struct field holding a string must therefore be `strdup`'d, not borrowed:
handing over a pointer into the shim's stack local would be a free of the stack,
and a pointer into the library's memory would be a free of the library's.

This is the one place the rule differs from a top-level return, where a string
is handed over borrowed and Jade copies it. Same tag, opposite ownership,
decided by where the value sits.

### Handles

Three forms, and the third is the one that matters.

`handle<T>` as an **argument** unwraps to the `T*` the library issued, checking
the type name first. The check is why the name is carried at all: two handles
are structurally identical, so passing a statement where a connection belongs
would otherwise be a dereference of the wrong object inside the library, with
nothing for Jade to report.

`handle<T>` as a **return** wraps the pointer back up.

`out_handle:T` is a handle written through a pointer — `sqlite3_open(path,
&db)`. Without it the generator could bind SQLite's entire surface *except* the
call that produces a connection, which is the same as binding none of it. The
C return value of such a symbol is a status, so the handle is what Jade gets and
the status feeds `fails_when`.

### Callbacks

`callback:<ret>(<arg>,…)`, and the signature is written in the library's **own C
types** — `callback:int(int, const char*)`, not Jade's widened ones. That is not
a detail: the shim declares a function pointer the library will store and call,
so `int` widened to `int64_t` is not a truncation but an incompatible function
pointer, and a call through the wrong ABI.

No `libffi` is involved, and that is the whole payoff of generating the shim from
a declaration rather than dispatching at run time: the signature is known when
the C is written, so a real static function of that shape can just be declared.

Two rules follow from where the callback runs:

**The registration outlives the call that made it, and is not thread-local.**
Both used to be the other way round, and both had to change together for a
library that *stores* a callback: it invokes it from a later call entirely, and
under the VM each native call runs on its own worker thread, so a thread-local
slot set during one would read empty in the next even if nothing cleared it.
The Jade function behind it is kept alive by `native::CallbackBus` for the life
of the VM — nothing in C says when a library is finished with a stored callback,
so there is no moment at which releasing it would be safe.

**There is one slot per symbol, which is not always enough.** Two outstanding
registrations on one symbol collide: the second takes the first's answers. Where
the library offers a context parameter beside the callback, `callback_data`
fills it with the callback's own pointer and the trampoline reads it back, so
each registration reaches its own function. Never inferred, for the reason
`null_ptr` is not — a library that puts something else in that slot would have
it dereferenced as a `JadeFn`.

**Every wrapper checks whether a callback raised, not only the ones that
register one.** Once a registration outlives its call, the symbol that
registered is not the symbol that was running when the raise happened: a
function given to `ares_search` raises during `ares_process`, and that is the
call that has to report it. So the flag is one per shim.

**A raise is deferred, never unwound.** The trampoline records the failure and
returns; the wrapper turns it into a Jade error *after* the library has returned
normally. Letting the raise out would unwind through the library's frames
mid-operation.

A callback may only give back a scalar, for the same reason an out-buffer is the
shim's memory: anything else would have to be released inside a C frame by code
that has no idea it is holding a Jade value.

### Structs going the other way

**`in_struct:<Type>` is the mirror of `out_struct`.** Jade builds the struct,
the shim copies it into a real local of the library's own type and passes its
address. Nothing owns anything across the boundary, because the library reads it
and forgets it. It needs the header for the reason `out_struct` does, and a
header is symmetric.

A field the caller left out stays as the zero the `memset` put there, which is
what the C it stands in for does: declare, zero, set what matters.
`lzma_stream_flags` carries fifteen reserved fields the library requires to be
zero, and demanding all seventeen would make the shape unusable. A field the
caller wrote that the type does not *have* is refused by name — that is the
mistake worth catching, because without the check a misspelling and an omission
are the same thing, and both become a zero the caller believed they had set.

Only for a struct every field of which survives the trip. Losing an output is
visible in what comes back; losing an input is not.

**`struct:<Type>` in return position** is a struct handed back by value. Nothing
crosses the boundary but the value — it arrives in registers or on the stack,
whichever the ABI says — so there is no allocation and no ownership to settle.
Only the declaration settles how it arrives, which is why this needs the header
too.

### A struct Jade holds

Some structs cannot be passed by value in either direction. `lzma_stream`,
`ZSTD_outBuffer`, `fd_set`: the caller allocates them and the library keeps them
between calls, so a shim declaring a fresh local every call would drop the
pointers a codec keeps its position in and hand back a zeroed struct instead of
the state the last call left.

`held = true` on a struct's table makes the generator write four bindings
alongside the library's own symbols — `<T>_new`, `<T>_free`, `<T>_get` and
`<T>_set`. The struct is allocated once on the C heap and every call gets the
same pointer, so what cannot travel stays exactly where the library put it. The
same shape answers the read-only case: a `const S*` whose fields do not all
survive the trip is not something the caller can build, so they hold one.

**The uncarryable pointers are the point, so they can be filled.** A held struct
with no way to set `next_in` is a handle you can make and never feed. With
buffer fields the allocation becomes a wrapper — the struct, then the memory its
pointers point at — and C guarantees a pointer to a struct is a pointer to its
first member, so the library still receives a plain `T*` and knows nothing about
the rest. The shim owns that memory because the library expects it to still be
there on the next call, and a Jade blob is the caller's and may be collected the
moment the call returns.

A read-only field is *set* from a blob you have. A writable one is *allocated*
to a size and then *taken* from once the library has filled it — two calls
rather than one, because how much of the buffer became real is something only
the caller can work out. `lzma` counts down through `avail_out` and `zstd`
counts up through `pos`, and no rule reads both.

The buffer fields are found by the rule the parameter list already uses, a byte
pointer then the count declared next to it, because C encodes the idiom the same
way in a struct definition. Fields named `reserved*` are excluded: `lzma_stream`
ends in four `void *reserved_ptr` and several `reserved_int`, two of which sit
in exactly that order, and a setter for one would offer a way to write where the
library requires a zero.

### Blobs without a length beside them

**`bytes_ptr` is `bytes` without the count.** Some libraries take a blob whose
extent is written *inside* it: every `libfdt` call takes `const void *fdt` alone
and reads the length out of the device tree's own header. Borrowed for the call,
exactly as a `str` is. Listed as an assumption, because Jade cannot check the
extent and a truncated blob reads past the end — which is the library's contract
with its caller and not something Jade can improve on.

**`inout_bytes` is for the buffers a library revises in place.** Every `libfdt`
writer edits the device tree where it sits, and a Jade blob is immutable, so
there is nothing to lend out to be scribbled on. The shim copies the caller's
bytes into scratch it owns, lets the library work on that, and hands the result
back as a fresh blob. The edit is a return rather than a mutation nothing
declared.

**`sized_buffer:<ctype>` is for the writes whose extent only the documentation
gives.** `lzma_stream_header_encode(const lzma_stream_flags *, uint8_t *out)`
writes exactly twelve bytes and says so nowhere a generator can read, so the
caller states it: the count is a Jade argument that reaches no further than the
shim, and the whole buffer comes back. Stating the size is what the C underneath
required of them anyway.

**`ret_len:<ctype>` is the mirror of `out_buffer`.** There the return value is
the count and the bytes went in through a parameter; here the bytes are the
return value and the count comes back through one. `fdt_getprop` is that shape
and is the main read call in libfdt. Only inferred when the header *names* the
parameter like a length, because nothing in the types tells `int *lenp` from the
second value a call happens to write back.

### A width the library never agreed to

`map_type` answers "how does Jade carry this", and its C spelling is Jade's own
width: `int` is `int64_t`, `float` is `double`, `bool` is `uint8_t`. With a
header that is only a marshalling tag, because the header's prototype is what
the compiler generates code against and a hand-written `int` that disagrees with
it is a compile error. `declare` returns an empty string in that case, and the
whole of this section is about the other one.

Without a header the shim writes the `extern` itself, and there the same three
spellings *are* the prototype. Someone hand-binding `g_uri_escape_string` got

```c
extern char* g_uri_escape_string(const char*, const char*, int64_t);
```

against glib's real third parameter, a 32-bit `gboolean`. Nothing catches that:
the manifest is valid, the shim compiles, the program runs.

**The return is the dangerous half.** Passing a value that is too wide usually
survives, because the callee reads only the part it wants. Reading one is the
reverse — the shim believes its own declaration and reads eight bytes where the
function wrote four, so the upper half is whatever was left in the register. And
a `float` declared as a `double` is not a slightly wrong number but a
meaningless one, because the two are different representations rather than two
sizes of one, which makes it wrong on every machine rather than on unlucky ones.

So `int`, `float` and `bool` are refused in `args` and in `ret` when the
dependency has no header, and `scalar:<ctype>` is what gets past. The shim
declares the named C type and converts to and from Jade's width at the boundary;
Jade's side does not change, because the shim is the translation layer and it
should speak whatever C demands. This is the same reasoning the callback
signatures already ran on — `int` must be `int` and not the `int64_t` Jade
widens it to — reaching the one position that had not had it applied.

Only those three. Every other type in the vocabulary crosses as an address, and
an address is one width.

`parse_c_scalar` is the one place either position resolves a `<ctype>`, and
`check_declared_widths` is the one place either position is refused, so the two
cannot come to disagree — the rule `emit_owned_str` follows for the two
owned-string positions. The accepted set is exactly `c_scalar`'s, which is what
`out_scalar` and `inout_scalar` already resolve through, so `scalar:uint32_t`
and `out_scalar:uint32_t` name the same C type by construction.

One thing the spelling opens up and has to close again: **`fails_when =
"negative"` on a return the library declares unsigned can never fire.** `(r) < 0`
on an unsigned type compiles to `false`, so the symbol binds, compiles, runs, and
hands every failure back as an ordinary result — the exact shape of failure this
generator exists to refuse. A Jade `int` is always signed, so nothing could reach
it before. Refused by name, and plain `char` is refused too rather than allowed:
its signedness is the platform's choice, so the test would fire on x86 Linux and
not on ARM macOS.

The out-parameter shapes were never exposed to this, because each already
carries the library's own C type for the reason given above: `out_scalar` and
`inout_scalar` declare a real local, `out_buffer` and `sized_buffer` size an
allocation with `sizeof`, and `handle` and `out_handle` are pointers. What *was*
exposed is an `out_buffer`'s element **count**, which is an ordinary `int`
argument sitting next to the buffer — the refusal reaches it like any other.
The rule that a buffer is followed by an integer now asks the tag rather than
the variant, so `scalar:size_t` satisfies it exactly as `int` does; a rule
written against one spelling is a rule the second spelling of the same thing
stops satisfying, silently.

### What the header does not say

Three refusals survive on purpose, and each names the spelling that gets past it
rather than guessing.

**Who frees a string.** `const char **namep` and `char **str` are the same C and
opposite ownership. The first points into data the caller already had, so
nothing was allocated and nothing has to be released; that is `out_str`, and it
is inferred. The second was malloc'd for you; that is `out_alloc_str`, it needs
`frees_with` naming the library's own free function, and it is refused with the
spelling named. Guessing one way leaks on every call and the other frees memory
that was never allocated.

The *return value* is the same question and by far the bigger one: 125 of glib's
symbols come back as a `gchar *`, which is more than any other refusal in the
library. `g_basename` points into its argument and `g_strdup` mallocs, and both
are spelled that way, so `const` cannot decide it — glib is disciplined about the
qualifier and plenty of libraries are not. `str` is the borrowed answer and
`alloc_str` the owned one, the latter requiring `frees_with`; a non-const `char *`
return is refused with both spellings named. Until v1.3.14 only `str` existed, so
the owning shape was reachable only by declaring it borrowed, which leaked the
allocation on every call.

`emit_owned_str` is the one place either spelling is emitted, so the two positions
cannot drift. Where the copy lands decides who owns it: inside a container it is
`strdup` and Jade's `ffi_free` reclaims it with the rest of the tree; at top level
the ABI says a string is borrowed, so it goes into `jade_shim_owned` — one buffer
per thread, grown to fit and reused by the next call, and released when the thread
exits. That buffer used to be a fixed 4096 and truncated, which is the worst answer
available: a URL-escaped path came back silently short and nothing anywhere said so.

A copy that fails is a failed call, and it says so: `out` comes back as a
`JADE_FFI_ERROR` naming the symbol and the cause. It used to be a bare status with
`out` left as it was found, which reads as "returned a non-zero status" in a
compiled binary and "returned error code 1" under the VM — neither of which is
"out of memory". The error goes on `out` rather than on wherever the string was
headed, because with two results the string's target is a field of the result
struct and a failure is always reported through the top-level `out`.

`frees_with` names a function the shim calls directly, and it is deliberately not
required to be a bound symbol — it usually cannot be one. A call taking a lone
`void *` and reporting nothing is refused as a binding, because that is the shape
of a call that frees what it is given, and that is exactly `g_free`. With headers
the header declares it; without them the shim writes its own `extern`.

**A pointer that cannot be carried at all.** Brotli's allocator hooks hand back
`void *`, which Jade cannot produce, and passing null is what tells brotli to
fall back on `malloc`. `null_ptr` says so, and is never inferred: a library that
*requires* a real pointer there gets a null dereference with no diagnostic,
which is the worst failure this generator can produce, so the decision belongs
to whoever read the documentation.

**A call that frees what it is given.** `ares_free_string(void *str)` takes a
lone `void *` and reports nothing. Handing one shim-owned scratch would have the
library free it and the shim free it again on the way out. Returning nothing is
what marks it — a lone `void *` on a call that reports a status is an in-place
edit, which `fdt_pack` is.

### Fixed-size array fields

A field like `char mnemonic[32]` is a row, and a row of things Jade has maps to
an array of them. The element type decides what they are — plain `char` is text
and everything else is data, the same rule a pointer parameter follows — so
`int reserved[4]` and `uint8_t bytes[24]` need no separate cases.

Nothing is trimmed on the way out. Thirty-two characters arrive, NUL padding
included, because trimming would guess where the text stops; `int(c)` exists so
a program can find that itself. On the way in, a row longer than the field is
refused by name rather than truncated, and a shorter one zero-fills.

**A `char` element is cast through `unsigned char` before widening.** `char` is
signed on x86 Linux and unsigned on ARM macOS, so without the cast a byte of
`0x80` sign-extends to `0xFFFFFF80`, which is not a Unicode scalar — and the far
side raises, on one platform and not the other.

**A struct holding nothing but rows stays held.** `fd_set` is one `int
fds_bits[32]`, filled by `ares_fds` and read by `ares_process`. Rows made it
carryable, which stopped it being lossy and turned it into an out-parameter — a
zeroed local every call, so `ares_process` would have received an empty set and
done nothing. A bag of rows is a buffer rather than a record: there are no named
values to read out, and the thing it is for is surviving between calls.

**Field types resolve through their own function, not `map_type`.** That one
also serves `args` and `ret`, so teaching it about rows would make
`array<char>:32` legal in an argument list where the wrapper has nothing to do
with it. One resolver per position, each refusing by name.

### What names decide, and why

Three questions cannot be answered from types alone, and all three are settled
by the parameter's own name — taken from the names that actually appear in these
headers rather than invented.

- *Does this integer count the thing before it?* `names_a_count`. A leading `n`
  or a count word, unless the name also says *where*: `nodeoffset` is the single
  most common name to follow a byte pointer in the set, and it counts nothing.
- *Does this pointer hold a position rather than a thing?* `names_a_position`.
  `size_t *in_pos, size_t in_size` has exactly the shape of a buffer and its
  count and is neither.
- *Which parameter sizes the returned pointer?* `names_a_length`, the strictest
  of the three, because sizing a blob from an unrelated number is unrecoverable.

The mistakes are not symmetric, and that is what makes a name worth trusting.
Reading a real length as an ordinary argument costs nothing, because the integer
is still passed and the caller supplies it. Reading an offset as a length
*drops* it and hands the library a size it never computed.

## Gotchas

`cshim.rs` binds a C function that *fills* a struct through a pointer, but not one that reads a struct you hand it. The out direction is what the shim can be sure about, because the library owns the layout and the header proves it; passing one in would need the same guarantee from the other side and nothing has asked for it.

**A struct out-parameter needs the library's header, and that is not negotiable.** The shim declares a real local of the struct's type, so the layout comes from the C compiler. Taking it from a hand-written field list instead would put integer widths and padding in a TOML file, where one disagreement writes at the wrong offset with nothing to catch it — valid manifest, compiling shim, corrupted memory. Add `include_dirs` when the header is not on the default search path.

The generated C is checked by compiling it, not only by matching strings. A test that asserts the output *contains* `if (!(r))` passes just as happily on a file with an unbalanced brace or a missing `#include`, and that file fails at install time on a user's machine instead of here.

Tests must never hit the network — use the `Fetcher` trait.

**Binding runs on `add` and `install`, not only on `bind`.** A separate step is one the user has to learn about, and it has no decision in it — a header either binds or it does not. `install` only fills in a dependency whose `symbols` are *absent*, so a committed manifest already carries them and a fresh clone installs without needing clang at all. `--locked` never binds, because a reproducible install must not depend on what the local clang makes of a header.

**`jade pkg bind` merges, it does not replace — and that has to include the header list.** Binding a large header a piece at a time with `--only` is a normal way to work, and replacing the table would make the second run delete what the first produced. Merging also leaves a hand-corrected entry alone unless that same symbol is regenerated.

The `headers` list was the half that still replaced, and it produced the worst failure in the set. Binding `archive_entry.h` after `archive.h` dropped the first header while keeping the symbols that came from it, so the shim declared none of them — and C lets an undeclared function be called, assuming it returns `int`. A call that really returns a pointer came back truncated to 32 bits, and the crash landed several calls later with nothing pointing at the manifest. It compiled clean, with no diagnostic anywhere. `compile_shim` now passes `-Werror=implicit-function-declaration`, so the same gap arriving by any other route is a named error instead.

**A symbol may have several out-parameters now, and the scratch locals are what breaks first.** `wrapper` used fixed names — `obuf`, `ostruct`, `ohandle` — so a second out-struct emitted the same declaration twice and the shim did not compile. Each is suffixed with the parameter's position. `Parsed.out_at: Option<usize>` became `outs: Vec<usize>` for the same reason, and the single-out assumption was threaded through four places.

**`produces_result` is not the negation of `takes_jade_arg`.** An `inout_scalar` does both: the caller seeds it and the library writes it back. The out-parameter list used to be derived from "takes no Jade argument", which silently dropped it from the result — the symbol bound, compiled, ran, and handed back the bare return value. Two predicates, because there are two questions.

**How many things come back decides the result's shape, and the counting has to reproduce the old shapes exactly.** `builds_result_struct` counts the out-parameters plus the C return value when nothing consumed it — an `out_buffer` reads it as an element count, an `out_handle` folds it into `fails_when`. One thing is the value directly; two or more is a keyed struct. Every binding that worked before regenerates byte-identical, which is what the untouched cshim tests check.

**The generator and the shim have to agree, and nothing else checks that they do.** They are written against one vocabulary in two files, so a spelling added to `bindgen.rs` and not to `cshim.rs` passes every unit test on both sides and then fails at `jade pkg install` on a user's machine. `bindgen/tests.rs` closes the loop by driving a header through both halves and compiling the result.

**`include_dirs` is written absolute, on purpose.** The shim is compiled inside `libs/<dep>/` rather than where `jade pkg bind` ran, so a relative `-I` resolves against the wrong directory and surfaces as a "file not found" from cc at install time, well away from the cause.

**The lock and the manifest have to agree on *what a dependency is*, not just on which ones exist.** `verify_in_sync` compared names, so a lock recording `abi = "jade"` survived a manifest corrected to `abi = "c"`. `ensure_ready` reads the lock rather than re-resolving — that is the point of a lock — so the build skipped the shim and installed the raw C library, and the first complaint came from `dlopen` in the finished program. Any field the lock copies from the manifest and the build then trusts belongs in that comparison.

**A `continue` in a loop over dependencies is a decision to install something unusable.** `build_c_shims` skipped a C dependency with no symbol table, which is the one combination that cannot work: nothing was bound, and a plain C library is exactly what the loader refuses. `resolve` rejects it, but `ensure_ready` does not re-resolve, so a lock written while the table was there outlives a manifest edit that removed it. It is an error now.

**"It has the right name" is not "it is a library", and nothing between `add` and `dlopen` disagreed.** A dependency was checked for what it *exported* and never for whether it could be loaded at all, so a file that was not an object file passed through the manifest, `libs/`, resolution and the linker, and was refused by the dynamic loader in the finished program. `bindgen::is_loadable_object` reads the magic number, and it is called in two places on purpose: `jade pkg add`, which can then say what probably went wrong, and `materialize`, which is the one point every source passes through with the bytes in hand — a hand-written manifest and a fresh clone never touch `add`. Anything new that puts a file into `libs/` needs the same check.

**A count returned beside a handle is not a status.** `infer_failure` read any
`int` return beside an `out_handle` as a status code, reasoning that the handle
is the result so the return can only be one. `size_t cs_disasm(…, cs_insn
**insn)` returns how many instructions it wrote, and a successful disassembly of
three raised. The discrimination is the *C* spelling — a status is an `int`, a
count is a `size_t` — and both collapse to Jade's `int` before the old test ever
saw them, which is why the predicate now takes both. Enums arrive as `int`
through `build_env`, which is right: `cs_err` and `lzma_ret` really are statuses.

**And an `out_handle` only swallows the return when something is testing it.**
`ret_is_a_key` discarded it unconditionally, so even with the inference fixed the
count had nowhere to go. A failure convention is what makes a return a status;
without one it is a value, and a pointer to a row whose length the caller cannot
know is not much of a result.

**A writable pointer to a complete struct is three different things.** Every one of them used to become `out_struct`. A type the library *hands out* — returned as `T*`, or written through a `T**` — is a handle, which is what the return position already called it. A type the caller allocates and the library keeps between calls cannot be an out-parameter at all, because the shim zeroes a fresh local every call: `lzma_code` bound, compiled, installed, ran, and did nothing, and `ZSTD_compressStream` would have written through a NULL `dst`. That one is a *held* struct, allocated once on the C heap and reached through a handle. Only a record one call fills stays an out-parameter.

**A generated binding that runs and does nothing is worse than a refusal, and both have happened here.** Every rule in this file that looks over-cautious is one of them. `lzma_code` bound against a struct zeroed on every call. `void *fdt` bound as scratch sized by a node offset, so fourteen of libfdt's writers handed the library uninitialised memory as the device tree. `uint8_t *out` bound as a one-byte local for a library that writes twelve. `const uint8_t *` lost its `const` through a typedef and became a buffer the shim allocated, so the caller's data never arrived. None of these failed at bind time, at compile time, or at link time. When a coverage number falls after a change here, check which of the two directions it moved in before treating it as a regression.

The discriminator needs *both* signals, and getting that wrong in either direction is silent. Refusing on "loses a field" alone takes `SF_INFO`-shaped records that carry one `void*`; refusing on "appears in several functions" alone takes `SF_INFO` itself, which three `sf_open` variants fill. `struct_loses_a_field` and `struct_param_counts` are ANDed for that reason, and the two existing tests that pin each half — `an_unrepresentable_field_is_dropped_rather_than_the_whole_struct` and `a_writable_struct_pointer_is_an_out_parameter_and_the_table_follows` — both still pass unedited, which is the check that the rule did not overreach.

**clang and `cc` have to be given the same include directories, and they were not.** `header_locations` computed the manifest's `include_dirs` as "the user's `-I`s plus the header's parent", while `bind_header` handed `from_header` only the user's `-I`s. So the shim compile could find a neighbouring header that reading the header could not, and the symptom was "clang could not parse" on a header that compiles fine. `bindgen::include_roots` is now the single source of both. It is called from *inside* `from_header` rather than at the call sites, which is what fixes `discover_header` — that one passed no directories at all, so any candidate needing one was silently demoted to a fallback.

Two directories, for two different includes: `libfdt.h` does `#include <libfdt_env.h>` from its own directory, which an angled include does not search; `brotli/encode.h` does `#include <brotli/port.h>`, which resolves against the directory above. Each cost a library outright. A directory the caller named is searched first, because a guessed root can be wide enough to shadow the header they meant.

**The export table decides what the library really has.** A header is written for the newest version while the artifact may have been configured without part of it. libbrotlienc's header declares two functions no brotli dylib on this machine exports; binding them produced a shim that compiled and then failed to *link*, and the linker refuses the whole dependency rather than the two symbols. The coverage check did not catch it because it only fires when *nothing* matches. Symbols are now filtered against the export table when one can be read, which is the same authority the umbrella-header case already leans on.

**Types come from the whole translation unit; functions come only from the header you named.** The two need different scopes and used to share one. A library splits its types into `git2/types.h` and declares functions against them in twenty other files, so an environment built from a single file reported every one of those functions as taking an unsupported type. Types are safe to take from everywhere because nothing is emitted for a type on its own — one is recorded only because a bound function reached it. Functions are not, or binding `archive.h` would bind `stdio.h` with it.

**What a header includes is scoped by the export table, not by a path heuristic.** `bindable` binds the named header's own declarations plus every declaration in the translation unit that the artifact also exports. That is an exact test — `fopen` is in that translation unit and is not in liblzma — where "which directories are system ones" would have been a guess that breaks the moment a library lives in `/opt/homebrew/include` alongside its own dependencies.

It started as a rule for umbrella headers alone. `lzma.h`, `git2.h` and `alsa/asoundlib.h` declare nothing and exist to include the files that do; pointing at one reported "no declarations found", and pointing at a sub-header failed differently because a sub-header usually does not compile alone. But the rule was all-or-nothing — a header declaring anything of its own bound only its own — and plenty of libraries do both. `ares.h` declares seventy-odd symbols and includes `ares_dns_record.h`, which declares sixty-three more; the whole modern DNS record API was invisible, and nothing reported it, because a symbol never reached is a symbol with nothing to refuse. The same exact test settles both cases, so it runs for both. Own declarations are kept unconditionally, which makes it additive: they are what the user pointed at, and an exported-only rule would drop one the artifact happens not to export.

Without an artifact there is nothing to test against: a header with its own declarations binds those alone, and an umbrella is refused with a message naming `--path`.

**One unbindable symbol must not take the dependency with it.** `from_header` resolved a symbol's structs inside a nested loop and `continue`d on failure — which continued the *inner* loop, so the symbol was emitted anyway while its field table was dropped. `cshim` refuses an `out_struct:` naming a table that is not there, and it refuses the whole dependency rather than the one symbol, so a single struct of unrepresentable fields made an otherwise fine library uninstallable. `sqlite3_snapshot_free` and `zip_file_attributes_init` are both that shape. The structs are resolved together now and the symbol is skipped as a unit, with the reason in the report.

**A C `enum` is an `int`, and recording that belongs in the type environment.** Putting it in the mapper would have to be repeated at every site a type is looked up — return, parameter, struct field — so it goes in `build_env` as an alias, and every path resolves through `expand`. Both spellings clang gives have to be checked: for `typedef enum { ... } lzma_ret;` the `qualType` is `enum lzma_ret` while the `desugaredQualType` is the bare `lzma_ret`, and `underlying` prefers the desugared one — which is the spelling with the keyword already gone. Missing this cost 60 of liblzma's 114 symbols, `lzma_code` among them.

**A normalized type name is a lookup key, not source text.** `normalize` drops `struct`, `union` and `enum` so a type can be found however it was written, and the stripped name was then also written into the generated shim. For `typedef struct sqlite3 sqlite3;` that is harmless, because the bare name really is a type — and every fixture in the suite used that shape, which is why nothing caught it. For the far more common `typedef struct X_s X;`, or a bare `struct X_s;`, `X_s` alone is not a type in C and the shim would not compile. `TypeEnv::c_name` puts the keyword back; `TypeEnv::tagged` is what knows whether it is needed. Anything new that turns a resolved type into shim text needs the same treatment.

**The two hints that tell a user what to write have to agree with what the generator accepts.** `unresolved_report` and `jade pkg add`'s note both showed `args = ["int", "int"]` as the shape to replace a `"?"` with — and a `"?"` means no header was read, which is exactly where `int` is now refused. Following the hint landed on a second error. Both lead with `--header` and then show `scalar:<ctype>`; `unresolved_report` still shows plain `int` when the dependency *does* carry headers, which is the case where it is the easier spelling and is correct.

**A placeholder has to be refused everywhere the binding is used, not just where it is generated.** `"?"` passes `resolve` — the table is non-empty, which is all resolution asks — so the refusal lives in `build_c_shims` and in `ensure_ready` ahead of the lock read. The second one matters: without it `jade run` on a fresh project answers "there is no jade.lock, run `jade pkg install`", and the user spends a command to arrive at the message they should have had first. `cli/check.rs` runs the same check against the manifest alone, which costs a read it was already doing and keeps `jade check` an honest predictor of `jade run` without installing anything.

**`CSymbol` deserializes by hand rather than with `#[serde(untagged)]`.** Untagged reports every failure as "data did not match any variant", so accepting the `"?"` string that way would have cost every *table* its "missing field `ret`". The visitor takes the string case itself and delegates the map case to a derived struct, which leaves those messages exactly as they were. A test pins it.

**A thread-exit hook cannot read a thread-local, and the version that does still runs.** The per-thread buffer behind `jade_shim_owned` is released through a `pthread_key_create` destructor, because C11's `_Thread_local` has none and both engines retire idle pool workers after ten seconds — threads come and go for as long as the program does, so the last buffer each one held accumulates rather than staying capped at one per pool slot. The trap is in how the destructor finds the buffer. Written to read a `_Thread_local`, it compiles, it is called once per thread, and it frees nothing: on macOS the thread's thread-local storage is already torn down by the time key destructors run, so the pointer reads back null. Peak RSS was identical with the hook and without it, and only measuring across a few thousand threads showed it. So the buffer travels in the key's own value, which is what the destructor is handed — and that value is a small holder rather than the buffer itself, because `realloc` moves the buffer and a key naming a released block would be a double free at thread exit. `-pthread` is passed by `compile_shim` for this.

**A present artifact is not a current artifact.** `materialize` compares `libs/` against the *lock*, so anything that changes the true source without changing the lock is invisible to it. That is exactly how a rebuilt `path` dependency used to keep running as the copy it was when it was added. `refresh_local` closes it for local sources; any future source kind that is mutable in place needs the same treatment, and adding one without it reintroduces the same silent staleness.

## Building and testing

```sh
cargo test pkg::
```
