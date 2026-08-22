# `src/pkg/`: the package manager

## What this subtree is

This is the machinery behind `jade pkg add`, `remove`, `install`, `update`, and `list`. It turns the `[dependencies]` section of `jade.toml` into a pinned `jade.lock` and a populated `libs/` directory.

```
jade.toml [dependencies] → jade.lock → libs/
```

A plain C dependency has one more step before that, and it is the step that makes the rest usable at scale:

```
<library>.h  →  binding generation  →  jade.toml [symbols]  →  generated shim  →  libs/
```

That step is *not a command you have to know about*, and usually not a flag or a path you have to supply either. `jade pkg add <name> --path <lib>` does the whole thing for either kind of library. It reads the artifact to see which kind it is, finds the header if it needs one, generates the table, and builds the shim. `--c-abi` and `--header` remain as overrides, for when there is nothing local to read or the guess would miss.

*What kind of library it is comes from the artifact, not from a flag.* A Jade package exports `jade_pkg_init`, and a plain C library does not. That is not a guess. It is the same symbol the loader requires at run time, so anything detected as a Jade package is exactly what `use` will later accept.

Both kinds are a `.dylib`, so the file extension says nothing, and only the symbol table can tell them apart. A URL dependency has nothing local to read at `add` time, which is what `--c-abi` is still for.

`jade pkg install` fills in any dependency that names a header but has no symbols yet. `jade pkg bind` remains for the cases with a real decision in them: re-running after a header changes, or narrowing a large header with `--only`.

*A `.so` cannot supply the header itself*, and the reason is worth being precise about. A shared library carries an export table of *names*, and C does not mangle them, so `sqlite3_open` in that table says nothing about its signature. Types survive only in DWARF debug information, which release builds strip. The macOS linker also leaves DWARF in the `.o` files rather than in the library, so even an unstripped `-g` build usually carries none.

So a header has to come from the filesystem. The library still has the last word on *which* header. `libsqlite3.dylib` implies `sqlite3.h`, and the search covers pkg-config, the usual include roots, and the macOS SDK. A candidate is accepted only if the library actually exports what that header declares. A header describing some other library of the same name is refused before anything is written, rather than surfacing later as an undefined symbol from the linker.

*When no header is found, the half that* is *readable still gets written.* The export table names every function, and only the types are gone. So `add` writes those names into `jade.toml` with `"?"` where each prototype belongs. The user then fills in blanks in a file that already lists the whole API, rather than hunting for a header that may not exist on this machine.

The placeholder is a legal manifest state on purpose, so `list` and `remove` keep working. Every command that would *use* the binding refuses it by name. What Jade will not do is guess. A made-up prototype means a corrupted stack several calls later, with nothing pointing back at the manifest, which is strictly worse than a blank.

That same export table gives the one number that says whether a binding is usable: *coverage*. "181 bound" reads as success whether the library has 190 entry points or 900, so the report says how many of the library's exports were covered.

## Why it was built this way

A dependency is a *prebuilt native shared library*, sourced from a local path or a URL. There is deliberately no package registry. As in Go, a dependency names where it lives rather than an entry in a central index.

That choice has one consequence worth stating plainly: *there is no version solving*. Each dependency contributes exactly one artifact, `jade.lock` is a flat list, and "resolution" means picking the right platform build.

What *has* changed is the part that used to read "and no transitive resolution, because a `.so` carries no manifest of its own". A Jade package now carries one.

`jade build --lib` emits `jade_pkg_deps`, a function whose entire body returns the `[[package]]` tables of the lock it was built against. `declared_dependencies` reads that back. It checks statically first, through `exported_symbols`, so a plain C library is never opened at all.

`jade pkg add` merges what it finds into the consuming project's `jade.toml`. A transitive dependency is a real dependency, and the manifest is what a person reads to know what their project uses.

There are two limits on that, and both are permanent rather than temporary.

Only a `url` dependency travels. A `path` names a file on the machine that built the package, which means nothing on another machine, so Jade names those for the user rather than writing a reference that would resolve to the wrong file.

And a name already present at a different version is refused rather than resolved. That is not only because there is no solver. Two versions are two files at two paths, and therefore two loaded copies, each with its own state.

Reading that record runs none of the package's code, and that matters rather than being incidental. A package runs its module top level from `jade_mod_init`, which only `jade_pkg_init` calls. Add a constructor anywhere in the AOT output and `jade pkg add` starts executing the package it is being asked to add, before the user has agreed to run it. A test asserts this by watching for a side effect, because reading the source is exactly what would stop being true.

The second decision is that the lock records an artifact *for every platform*, not only the current one. Cargo locks portable source, so it does not need this. A lock naming one artifact would be valid only on the machine that generated it. A macOS developer would commit a lock that Linux CI could not install, and with no registry to ask, could not even *verify*.

The third decision is that a local `path` dependency is *re-hashed on every install*, while a URL dependency is not. A path points at a file the user builds and rebuilds. It is the one source that can legitimately change underneath a lock that is otherwise still correct.

A URL is the opposite. It either serves the bytes the lock pins or it does not, and quietly re-pinning it would defeat the point of having a lock. So `refresh_local` runs before every `materialize`, and `verify_local_unchanged` is its `--locked` counterpart, turning the same drift into a CI failure rather than a fix.

The whole surface between this module and the rest of the compiler is one function, `dependency_libraries`. Resolved dependencies come back as synthetic `project::LibraryEntry` values and are merged into the manifest's `[lib]` map. So neither the VM nor the AOT import resolver ever learns what a dependency is. Both keep resolving `[lib]` entries exactly as before, which is how the two backends are kept from drifting on imports.

## What each file does

- *`mod.rs`* handles resolution and materialization. `dependency_libraries` is the one public seam into the rest of the compiler. It also holds the local-source group described above, meaning `refresh_local`, `verify_local_unchanged`, and `local_drift`. It defines `LIBS_DIR`, which is `libs/` and is gitignored because `jade.lock` is what travels. It also defines `ANY_PLATFORM`, the artifact key for a URL with no `{platform}` placeholder.
- *`lock.rs`* defines the `jade.lock` format: `LockedPackage`, `LockedArtifact`, and one digest per platform. The lock is meant to be committed.
- *`fetch.rs`* handles fetching artifacts and checking their integrity, behind a `Fetcher` trait. Everything in this module takes a `&dyn Fetcher` rather than reaching the network directly, so the whole package manager can be tested offline against a map of canned responses.
- *`manifest.rs`* makes format-preserving edits to `jade.toml` through `toml_edit`. `jade pkg add` and `jade pkg remove` rewrite a file a person wrote by hand. Parsing and re-serializing it instead would silently discard every comment and all the original layout.
- *`cshim.rs`* generates a Jade-ABI binding shim for a plain C library. The loader requires a `jade_pkg_init` symbol, which something like `libz` does not have.

  Teaching the runtime to dispatch arbitrary C signatures would mean a `libffi` dependency and *two* marshalling implementations, one per engine. Instead this file emits a small C wrapper and compiles it with `cc`. The result is an ordinary Jade-ABI package both backends already know how to load.

  The type vocabulary is the FFI's own: `int`, `float`, `bool`, `str`, `bytes`, and `nil` for returns. Two more forms make the shim *rewrite* a call rather than forward it, which is what lets a real C signature be callable at all:

  - `bytes` as an argument expands to the C pair `(const void*, size_t)`.
  - `out_buffer:<ctype>` and `out_struct:<Type>` are out-parameters. They consume *no* Jade argument, because the shim owns the memory. So `x_read(handle, buf, n)` is called from Jade as `x_read(handle, n)` and hands back the bytes. The rewrite rules are below.
  - `scalar:<ctype>` is the same value with the library's own C type named, for the case where the shim writes the declaration itself. See *A width the library never agreed to* below.

A symbol may also declare `fails_when`, whose values are `null`, `negative`, `nonzero`, and `never`. The shim then clears `errno`, tests the return against that convention, and on failure hands back a `JADE_FFI_ERROR` carrying the `strerror` text and the number. Both engines already turn that into a catchable Jade raise.

Without `fails_when`, a failed call returns its raw sentinel and throws away the reason the library *had already recorded*. The program sees `-1` and nothing else.

There is no universal convention to infer, which is why each binding names the one its symbol uses. The default is "cannot fail", because assuming a convention that is not there would turn every legitimate `-1` into a raise.
- *`bindgen.rs`* generates a dependency's `symbols` and `structs` tables from its C header. `jade pkg add --header`, `jade pkg install`, and `jade pkg bind` all drive it. This is what makes "bind any `.so`" true in practice. The ABI could already express handles, blobs, and structs, but every signature still had to be typed in by hand, and SQLite has around 200 entry points.

It reads the header with *clang*, running `clang -Xclang -ast-dump=json -fsyntax-only` over a pipe. Parsing C by hand means dealing with macros, conditionals, and typedef chains, and a home-grown parser would misread far more than it read.

Shelling out rather than linking `libclang` keeps a large native dependency out of the shipped binary, and it costs nothing in practice, because `cc` is already required to bind a C library at all.

*The skip report is the feature.* No generator binds everything, and one that quietly covers two thirds of an API is how the missing third gets found at run time.

So whatever it drops is named with a reason, grouped so that one cause reads as one fact. And a binding resting on an inference is listed as *assumed* rather than buried. A non-const `T*` beside a count is *almost* always an out-buffer, for example.

On the real `sqlite3.h` that comes to 181 bound, 2 assumed, and 105 skipped, where every skip is a genuine limit of the ABI rather than a gap in the reader.
- *`tests.rs`* holds the package manager tests, all of which run offline.

## Who uses it

*Depends on:* `project/` for `ProjectManifest`, `DependencyEntry`, and `LibraryEntry`.

*Also depends on:* `clang` on your `PATH`, but only when a header is actually read. That means `add --header`, `bind`, or an `install` filling in missing symbols. A manifest that already carries its symbols installs without clang. Nothing else in the package manager needs it, and a missing clang is reported along with the workaround, which is to write the table by hand, rather than as a crash.

*Used by:* `cli/pkg.rs`, for the commands. Indirectly, `vm/chunk.rs` and `aot/imports.rs` consume the `[lib]` entries this module contributes, without knowing they came from a dependency.

## How the shim rewrites a call

A C signature and a callable Jade signature are not the same shape, and that gap is most of what stops a real library from being bindable:

```c
int sf_read_short(SNDFILE* f, short* buf, int count);
int sf_open(const char* path, int mode, SF_INFO* info);
```

Neither can be called as written. The first wants a buffer the caller allocated, and reports how much of it was filled. The second returns one thing and writes another through a pointer. A one-to-one mapping of parameters cannot express either shape.

So the shim rewrites the call rather than merely forwarding it. The declared `args` list describes the *C* signature. The Jade signature is derived from it, and is deliberately a different length.

### How much of a real library this reaches

These numbers come from seven Homebrew libraries. They count only what the header declares *and* the artifact exports, because a header written for a newer version is not a gap in the binding:

| library | bound | of | |
|---|---|---|---|
| zstd | 67 | 67 | 100% |
| liblzma | 111 | 114 | 97% |
| libfdt | 76 | 79 | 96% |
| capstone | 19 | 20 | 95% |
| brotlidec | 11 | 14 | 79% |
| brotlienc | 10 | 13 | 77% |
| c-ares | 54 | 74 | 73% |
| *all seven* | *348* | *381* | *91%* |

Before the v1.3.8 work, the number was 223 out of the same 381, and several of those 223 were bindings that ran and did nothing.

With two exceptions, the refusals that remain are cases where the header does not carry the answer. See *What the header does not say* below.

To measure again, run `jade pkg add <name> --path <lib> --header <hdr>` and read the `covers` line. The skip report is grouped by reason, and it is the thing to read before adding a rule.

### The rules

*A `bytes` argument is one Jade value and two C parameters.* It expands to `(const void*, size_t)`, and the pointer is borrowed for the duration of the call, exactly as a `str` argument is. A nil blob passes `NULL, 0` rather than dereferencing anything.

*An out-parameter consumes no Jade argument at all.* That is the rewrite which makes `x_read(handle, buf, n)` callable as `x_read(handle, n)`.

*An `out_buffer` is the shim's memory, never Jade's.* A Jade `bytes` is immutable, with three methods and none of them writing. Letting a C library write into one would break that guarantee for the FFI's convenience. So the shim allocates the scratch memory, the library fills it, and Jade only ever sees the finished blob.

Its size comes from *the next declared argument*, which must be an `int`. Nearly every buffer-filling C function has that shape, including `read(fd, buf, n)`, `gzread`, `fread`, and `sf_read_short`. The shim has to know how much to allocate before it can call anything. The alternative, a separate key naming which argument holds the count, buys nothing for the cases that actually exist.

*The return value of an `out_buffer` symbol is the element count, and it sizes the blob.* It does not also come back separately, because a counted buffer already carries its length: `b.len()` is `written * sizeof(elem)`. The count is clamped to what was allocated, so a library reporting more than it was given cannot make the copy read past the scratch memory.

*A scalar written through a pointer is an out-parameter too.* `int *nextoffset` and `uint64_t *progress` are C's way of returning a second value, and they are everywhere.

`out_scalar:<ctype>` carries the library's own C type rather than a Jade one, for the same reason `out_buffer` and `callback` do. The shim declares a real local variable, so widening `uint32_t` to `int64_t` would take the address of a differently-sized object and let the library write past it.

Some of those are read *and* written, such as a position the caller sets and the library advances, written `size_t *out_pos`. A zeroed local is right for the first call and wrong on the second, which shows up as corrupt output rather than as an error.

Nothing in C distinguishes the two cases. So `inout_scalar:<ctype>` exists for the second, and the generator emits `out_scalar` while listing it as an *assumption* that names the fix. That mirrors the out_buffer guess exactly. The generator does not get to dress a guess as certainty.

*More than one out-parameter is allowed, and then each one carries a name.* The rule used to be one out-parameter, on the grounds that two would come back as a pair with no obvious names.

They are not nameless. The header already names them, and clang hands the parameter names over along with the types. `out_scalar:uint64_t@progress_in` says which key the value comes back under. A symbol whose header does *not* name its parameters is skipped, rather than given invented `out0` and `out1` keys. That was the real objection all along.

*How many things come back decides the shape.* Count the out-parameters, and add the C return value when nothing has already consumed it. An `out_buffer` reads the return as an element count, and an `out_handle` folds it into the failure convention.

If one thing comes back, that is the result. If two or more come back, they arrive as a struct, with `ret` first when it is a key, then one key per out-parameter in the order they were declared.

That counting reproduces every shape that existed before, rather than replacing it. A lone out-parameter with a `void` return is still the bare value. A lone out-parameter beside a real return is still `.ret` and `.out`, and it keeps the name `out` because there is nothing to tell it apart from.

### Why `out_struct` requires a header

The shim has to declare a real local variable of the struct's type. It could build one from the declared field list, and that is exactly the wrong answer.

A layout built that way lives in a hand-written TOML file. One wrong integer width, one missed padding byte, or one field listed out of order, and the shim reads and writes at offsets the library does not agree with. Nothing catches it. The manifest is valid, the shim compiles, and the program returns plausible garbage or corrupts memory belonging to the library.

Including the real header moves the layout to the only place it can be correct, which is the C compiler. The field list then carries only names and Jade types, and a field the struct does not have becomes a compile error naming that field.

The same reasoning is why a symbol is *not* re-declared when headers are present. A hand-written prototype that disagrees with the real one, such as `int` where the library says `long`, truncates silently at run time. Letting the header win turns that into a compile error instead. If you are going to require a header, requiring it to be authoritative is the only consistent position.

The cost is real and worth stating. A dependency using `out_struct` needs the library's development headers present at install time, plus `include_dirs` when those headers are not on the default search path. Anyone who has the library has them.

### Ownership at the boundary

A value *inside a container* is owned by that container, so Jade's `ffi_free` frees it. A struct field holding a string must therefore be copied with `strdup` rather than borrowed. Handing over a pointer into the shim's stack local would mean freeing the stack, and a pointer into the library's memory would mean freeing the library's.

This is the one place the rule differs from a top-level return, where a string is handed over borrowed and Jade copies it. The same tag, the opposite ownership, decided by where the value sits.

### Handles

Three forms, and the third is the one that matters.

`handle<T>` as an *argument* unwraps to the `T*` the library issued, checking the type name first. That check is why the name is carried at all. Two handles look identical in structure, so passing a statement where a connection belongs would otherwise dereference the wrong object inside the library, with nothing for Jade to report.

`handle<T>` as a *return* wraps the pointer back up.

`out_handle:T` is a handle written through a pointer, as in `sqlite3_open(path, &db)`. Without it, the generator could bind SQLite's entire surface *except* the call that produces a connection, which amounts to binding none of it. The C return value of such a symbol is a status, so Jade gets the handle and the status feeds `fails_when`.

### Callbacks

A callback is written `callback:<ret>(<arg>,…)`, and the signature uses the library's *own C types*. Write `callback:int(int, const char*)`, not Jade's widened types.

That is not a detail. The shim declares a function pointer the library will store and call. So an `int` widened to `int64_t` is not a truncation. It is an incompatible function pointer, and a call through the wrong ABI.

No `libffi` is involved. That is the whole payoff of generating the shim from a declaration rather than dispatching at run time. The signature is known when the C is written, so the shim can simply declare a real static function of that shape.

Two rules follow from where the callback runs:

*The registration outlives the call that made it, and it is not thread-local.* Both used to be the other way round, and both had to change together for a library that *stores* a callback. Such a library invokes the callback from a later call entirely. Under the VM, each native call runs on its own worker thread, so a thread-local slot set during one call would read empty in the next, even if nothing cleared it.

`native::CallbackBus` keeps the Jade function alive for the life of the VM. Nothing in C says when a library is finished with a stored callback, so there is no moment at which releasing it would be safe.

*There is one slot per symbol, which is not always enough.* Two outstanding registrations on one symbol collide, and the second takes the first's answers.

Where the library offers a context parameter beside the callback, `callback_data` fills that parameter with the callback's own pointer, and the trampoline reads it back. Each registration then reaches its own function. It is never inferred, for the same reason `null_ptr` is not. A library that puts something else in that slot would have it dereferenced as a `JadeFn`.

*Every wrapper checks whether a callback raised, not only the wrappers that register one.* Once a registration outlives its call, the symbol that registered is not the symbol that was running when the raise happened. A function given to `ares_search` raises during `ares_process`, and `ares_process` is the call that has to report it. So there is one flag per shim.

*A raise is delayed, never unwound.* The trampoline records the failure and returns. The wrapper turns it into a Jade error *after* the library has returned normally. Letting the raise out would unwind through the library's frames in the middle of an operation.

A callback may only give back a scalar, for the same reason an out-buffer is the shim's memory. Anything else would have to be released inside a C frame, by code that has no idea it is holding a Jade value.

### Structs going the other way

*`in_struct:<Type>` is the mirror of `out_struct`.* Jade builds the struct, and the shim copies it into a real local of the library's own type and passes that local's address. Nothing owns anything across the boundary, because the library reads the struct and forgets it. It needs the header for the same reason `out_struct` does, and a header serves both directions.

A field the caller left out keeps the zero that `memset` put there. That matches what the C it stands in for does: declare, zero, then set what matters. `lzma_stream_flags` carries fifteen reserved fields the library requires to be zero, and demanding all seventeen would make the shape unusable.

A field the caller wrote that the type does not *have* is refused by name. That is the mistake worth catching, because without the check a misspelling and an omission look the same, and both become a zero the caller believed they had set.

Use it only for a struct whose every field survives the trip. Losing an output is visible in what comes back. Losing an input is not.

*`struct:<Type>` in return position* is a struct handed back by value. Nothing crosses the boundary except the value itself, which arrives in registers or on the stack, whichever the ABI says. So there is no allocation and no ownership to settle. Only the declaration settles how it arrives, which is why this needs the header too.

### A struct Jade holds

Some structs cannot be passed by value in either direction. `lzma_stream`, `ZSTD_outBuffer`, and `fd_set` are all examples. The caller allocates them and the library keeps them between calls. A shim declaring a fresh local on every call would drop the pointers a codec keeps its position in, and hand back a zeroed struct instead of the state the last call left.

`held = true` on a struct's table makes the generator write four extra bindings alongside the library's own symbols: `<T>_new`, `<T>_free`, `<T>_get`, and `<T>_set`. The struct is allocated once on the C heap, and every call gets the same pointer, so what cannot travel stays exactly where the library put it.

The same shape answers the read-only case. A `const S*` whose fields do not all survive the trip is not something the caller can build, so the caller holds one instead.

*The pointers that cannot be carried are the point, so they can be filled.* A held struct with no way to set `next_in` is a handle you can make and never feed.

With buffer fields, the allocation becomes a wrapper: the struct first, then the memory its pointers point at. C guarantees that a pointer to a struct is a pointer to its first member, so the library still receives a plain `T*` and knows nothing about the rest.

The shim owns that memory because the library expects it to still be there on the next call. A Jade blob belongs to the caller and may be collected the moment the call returns.

A read-only field is *set* from a blob you already have. A writable one is *allocated* to a size and then *taken* from once the library has filled it. That takes two calls rather than one, because only the caller can work out how much of the buffer became real. `lzma` counts down through `avail_out`, `zstd` counts up through `pos`, and no single rule reads both.

The buffer fields are found by the same rule the parameter list already uses: a byte pointer, then the count declared next to it. C writes the pattern the same way inside a struct definition.

Fields named `reserved*` are excluded. `lzma_stream` ends in four `void *reserved_ptr` fields and several `reserved_int` fields, two of which sit in exactly that order. A setter for one of those would offer a way to write where the library requires a zero.

### Blobs without a length beside them

*`bytes_ptr` is `bytes` without the count.* Some libraries take a blob whose length is written *inside* it. Every `libfdt` call takes `const void *fdt` alone and reads the length out of the device tree's own header.

It is borrowed for the call, exactly as a `str` is. It is listed as an assumption, because Jade cannot check the length, and a truncated blob reads past the end. That is the library's contract with its caller, and not something Jade can improve on.

*`inout_bytes` is for the buffers a library revises in place.* Every `libfdt` writer edits the device tree where it sits. A Jade blob is immutable, so there is nothing to lend out to be written into.

The shim copies the caller's bytes into scratch memory it owns, lets the library work on that, and hands the result back as a fresh blob. The edit comes back as a return value, rather than as a mutation nothing declared.

*`sized_buffer:<ctype>` is for the writes whose size only the documentation gives.* `lzma_stream_header_encode(const lzma_stream_flags *, uint8_t *out)` writes exactly twelve bytes, and says so nowhere a generator can read.

So the caller states the size. The count is a Jade argument that reaches no further than the shim, and the whole buffer comes back. Stating the size is what the underlying C required of them anyway.

*`ret_len:<ctype>` is the mirror of `out_buffer`.* With `out_buffer`, the return value is the count and the bytes went in through a parameter. Here the bytes are the return value and the count comes back through a parameter. `fdt_getprop` has that shape, and it is the main read call in libfdt.

It is only inferred when the header *names* the parameter like a length, because nothing in the types distinguishes `int *lenp` from the second value a call happens to write back.

### A width the library never agreed to

`map_type` answers the question "how does Jade carry this value", and its C spelling uses Jade's own widths: `int` is `int64_t`, `float` is `double`, and `bool` is `uint8_t`.

With a header present, that is only a marshalling tag. The header's prototype is what the compiler generates code against, so a hand-written `int` disagreeing with it is a compile error. `declare` returns an empty string in that case. The rest of this section is about what happens without a header.

Without a header, the shim writes the `extern` itself, and there those same three spellings *are* the prototype. Someone hand-binding `g_uri_escape_string` got:

```c
extern char* g_uri_escape_string(const char*, const char*, int64_t);
```

That was written against glib's real third parameter, a 32-bit `gboolean`. Nothing catches the mismatch. The manifest is valid, the shim compiles, and the program runs.

*The return value is the dangerous half.* Passing a value that is too wide usually survives, because the callee reads only the part it wants. Reading one is the reverse. The shim believes its own declaration and reads eight bytes where the function wrote four, so the upper half is whatever was left in the register.

A `float` declared as a `double` is not a slightly wrong number. It is a meaningless one, because the two are different representations rather than two sizes of the same thing. That makes it wrong on every machine rather than on unlucky ones.

So `int`, `float`, and `bool` are refused in both `args` and `ret` when the dependency has no header, and `scalar:<ctype>` is what gets past. The shim declares the named C type and converts to and from Jade's width at the boundary. Jade's side does not change, because the shim is the translation layer and it should speak whatever C demands.

That is the same reasoning the callback signatures already used, where `int` must be `int` and not the `int64_t` Jade widens it to. It simply reaches the one position that had not had it applied.

Only those three types are affected. Every other type in the vocabulary crosses as an address, and an address has one width.

`parse_c_scalar` is the one place either position resolves a `<ctype>`, and `check_declared_widths` is the one place either position is refused. So the two cannot come to disagree. `emit_owned_str` follows the same rule for the two owned-string positions.

The accepted set is exactly `c_scalar`'s, which is what `out_scalar` and `inout_scalar` already resolve through. So `scalar:uint32_t` and `out_scalar:uint32_t` name the same C type by construction.

The spelling opens up one hole that has to be closed again. *`fails_when = "negative"` on a return the library declares unsigned can never fire.* The test `(r) < 0` on an unsigned type compiles to `false`, so the symbol binds, compiles, runs, and hands every failure back as an ordinary result. That is exactly the shape of failure this generator exists to refuse.

A Jade `int` is always signed, so nothing could reach that case before. It is now refused by name. Plain `char` is refused too rather than allowed, because its signedness is the platform's choice, so the test would fire on x86 Linux and not on ARM macOS.

The out-parameter shapes were never exposed to this problem, because each already carries the library's own C type, for the reason given above. `out_scalar` and `inout_scalar` declare a real local. `out_buffer` and `sized_buffer` size an allocation with `sizeof`. `handle` and `out_handle` are pointers.

What *was* exposed is an `out_buffer`'s element *count*, which is an ordinary `int` argument sitting next to the buffer. The refusal reaches it like any other.

The rule that a buffer is followed by an integer now asks the tag rather than the variant, so `scalar:size_t` satisfies it exactly as `int` does. A rule written against one spelling is a rule that the second spelling of the same thing stops satisfying, silently.

### What the header does not say

Three refusals survive on purpose. Each one names the spelling that gets past it rather than guessing.

*Who frees a string.* `const char **namep` and `char **str` are the same C with opposite ownership.

The first points into data the caller already had, so nothing was allocated and nothing has to be released. That is `out_str`, and it is inferred.

The second was allocated with malloc for you. That is `out_alloc_str`. It needs a `frees_with` naming the library's own free function, and it is refused with the spelling named. Guessing one way leaks on every call, and guessing the other frees memory that was never allocated.

The *return value* raises the same question, and it is by far the bigger case. 125 of glib's symbols come back as a `gchar *`, which is more than any other refusal in that library.

`g_basename` points into its argument and `g_strdup` allocates, and both are spelled the same way, so `const` cannot decide between them. glib is disciplined about that qualifier, and plenty of libraries are not.

`str` is the borrowed answer and `alloc_str` is the owned one, and the owned one requires `frees_with`. A non-const `char *` return is refused with both spellings named. Until v1.3.14 only `str` existed, so the owning shape was reachable only by declaring it borrowed, which leaked the allocation on every call.

`emit_owned_str` is the one place either spelling is emitted, so the two positions cannot drift apart. Where the copy lands decides who owns it.

Inside a container, the copy is a `strdup`, and Jade's `ffi_free` reclaims it along with the rest of the tree. At top level, the ABI says a string is borrowed, so the copy goes into `jade_shim_owned`. That is one buffer per thread, grown to fit, reused by the next call, and released when the thread exits.

That buffer used to be a fixed 4096 bytes and truncated, which is the worst answer available. A URL-escaped path came back silently short, and nothing anywhere said so.

A copy that fails is a failed call, and it says so. `out` comes back as a `JADE_FFI_ERROR` naming both the symbol and the cause. It used to be a bare status with `out` left as it was found, which reads as "returned a non-zero status" in a compiled binary and "returned error code 1" under the VM. Neither of those says "out of memory".

The error goes on `out` rather than on wherever the string was headed. With two results, the string's target is a field of the result struct, and a failure is always reported through the top-level `out`.

`frees_with` names a function the shim calls directly, and it deliberately does not have to be a bound symbol. It usually cannot be one. A call taking a lone `void *` and reporting nothing is refused as a binding, because that is the shape of a call which frees what it is given, and `g_free` is exactly that. With headers present, the header declares it. Without them, the shim writes its own `extern`.

*A pointer that cannot be carried at all.* Brotli's allocator hooks hand back a `void *`, which Jade cannot produce, and passing null is what tells brotli to fall back on `malloc`.

`null_ptr` says exactly that, and it is never inferred. A library that *requires* a real pointer there gets a null dereference with no diagnostic, which is the worst failure this generator can produce. So the decision belongs to whoever read the documentation.

*A call that frees what it is given.* `ares_free_string(void *str)` takes a lone `void *` and reports nothing. Handing it shim-owned scratch memory would have the library free that memory, and the shim free it again on the way out.

Returning nothing is what marks such a call. A lone `void *` on a call that does report a status is an in-place edit instead, and `fdt_pack` is that shape.

### Fixed-size array fields

A field like `char mnemonic[32]` is a row, and a row of things Jade has maps to an array of them. The element type decides what those things are: plain `char` is text, and everything else is data. That is the same rule a pointer parameter follows, so `int reserved[4]` and `uint8_t bytes[24]` need no separate cases.

Nothing is trimmed on the way out. Thirty-two characters arrive, NUL padding included, because trimming would mean guessing where the text stops. `int(c)` exists so a program can find the end itself. On the way in, a row longer than the field is refused by name rather than truncated, and a shorter one is padded with zeros.

*A `char` element is cast through `unsigned char` before widening.* `char` is signed on x86 Linux and unsigned on ARM macOS. Without the cast, a byte of `0x80` sign-extends to `0xFFFFFF80`, which is not a Unicode scalar. The far side then raises, on one platform and not the other.

*A struct holding nothing but rows stays held.* `fd_set` is a single `int fds_bits[32]`, filled by `ares_fds` and read by `ares_process`.

Supporting rows made it carryable, which stopped it being lossy and turned it into an out-parameter. That would have meant a zeroed local on every call, so `ares_process` would have received an empty set and done nothing.

A collection of rows is a buffer rather than a record. There are no named values to read out, and the whole point of it is surviving between calls.

*Field types resolve through their own function, not through `map_type`.* `map_type` also serves `args` and `ret`, so teaching it about rows would make `array<char>:32` legal in an argument list, where the wrapper has nothing to do with it. There is one resolver per position, and each refuses by name.

### What names decide, and why

Three questions cannot be answered from types alone. All three are settled by the parameter's own name, taken from the names that actually appear in these headers rather than invented.

- *Does this integer count the thing before it?* That is `names_a_count`. It looks for a leading `n` or a count word, unless the name also says *where*. `nodeoffset` is the single most common name to follow a byte pointer in this set, and it counts nothing.
- *Does this pointer hold a position rather than a thing?* That is `names_a_position`. The pair `size_t *in_pos, size_t in_size` has exactly the shape of a buffer and its count, and is neither.
- *Which parameter sizes the returned pointer?* That is `names_a_length`, the strictest of the three, because sizing a blob from an unrelated number cannot be recovered from.

The two possible mistakes are not equally bad, and that is what makes a name worth trusting. Reading a real length as an ordinary argument costs nothing, because the integer is still passed and the caller supplies it. Reading an offset as a length *drops* the offset and hands the library a size it never computed.

## Gotchas

`cshim.rs` binds a C function that *fills* a struct through a pointer, and not one that reads a struct you hand it. The out direction is what the shim can be certain about, because the library owns the layout and the header proves it. Passing a struct in would need the same guarantee from the other side, and nothing has asked for it.

*A struct out-parameter needs the library's header, and that is not negotiable.* The shim declares a real local variable of the struct's type, so the layout comes from the C compiler.

Taking the layout from a hand-written field list instead would put integer widths and padding in a TOML file, where one disagreement writes at the wrong offset with nothing to catch it. The manifest stays valid, the shim compiles, and memory is corrupted. Add `include_dirs` when the header is not on the default search path.

The generated C is checked by compiling it, not only by matching strings. A test asserting that the output *contains* `if (!(r))` passes just as happily on a file with an unbalanced brace or a missing `#include`. That file then fails at install time on a user's machine rather than here.

Tests must never reach the network. Use the `Fetcher` trait.

*Binding runs during `add` and `install`, not only during `bind`.* A separate step is one the user has to learn about, and it holds no decision anyway, because a header either binds or it does not.

`install` only fills in a dependency whose `symbols` are *absent*. A committed manifest already carries them, so a fresh clone installs without needing clang at all. `--locked` never binds, because a reproducible install must not depend on what the local clang makes of a header.

*A header that was read is recorded, whether or not a single symbol survived it.* The skip report ends by telling the user to write the table by hand. A table written against a dependency with no `headers` is a *headerless* binding, where `int` means Jade's 64-bit width standing in for whatever the library declared. That is the exact trap the width refusal exists to catch, reached by following Jade's own instructions. So `set_bindings` runs before the "nothing bound" refusal rather than after it.

So `jade pkg add` can no longer roll its entry back on every failure. Undoing the write would delete the header that the same message says was recorded.

`BindFailure` separates the two cases. A header that could not be *read* wrote nothing, and takes the entry with it. A header that was read but bound nothing keeps the entry.

Nothing is locked or installed in the second case, because a C dependency with no symbols does not resolve. The table the user is being asked to write is the missing half, and the note says so and names `jade pkg install` as the step after it.

*`jade pkg bind` merges rather than replaces, and that has to include the header list.* Binding a large header a piece at a time with `--only` is a normal way to work, and replacing the table would make the second run delete what the first produced. Merging also leaves a hand-corrected entry alone, unless that same symbol is regenerated.

The `headers` list was the half that still replaced, and it produced the worst failure in the set. Binding `archive_entry.h` after `archive.h` dropped the first header while keeping the symbols that came from it, so the shim declared none of them.

C lets you call an undeclared function, assuming it returns `int`. A call that really returns a pointer came back truncated to 32 bits, and the crash landed several calls later, with nothing pointing at the manifest. It compiled clean, with no diagnostic anywhere.

`compile_shim` now passes `-Werror=implicit-function-declaration`, so the same gap arriving by any other route is a named error instead.

*A symbol may have several out-parameters now, and the scratch locals are what breaks first.* `wrapper` used fixed names, meaning `obuf`, `ostruct`, and `ohandle`. So a second out-struct emitted the same declaration twice and the shim did not compile. Each name now carries the parameter's position as a suffix.

`Parsed.out_at: Option<usize>` became `outs: Vec<usize>` for the same reason, and the assumption of a single out-parameter ran through four places.

*`produces_result` is not the opposite of `takes_jade_arg`.* An `inout_scalar` does both: the caller seeds it and the library writes it back.

The out-parameter list used to be derived from "takes no Jade argument", which silently dropped an `inout_scalar` from the result. The symbol bound, compiled, ran, and handed back the bare return value. There are two predicates now, because there are two questions.

*How many things come back decides the result's shape, and the counting has to reproduce the old shapes exactly.* `builds_result_struct` counts the out-parameters, plus the C return value when nothing has consumed it. An `out_buffer` reads the return as an element count, and an `out_handle` folds it into `fails_when`.

One thing coming back is the value directly. Two or more become a keyed struct. Every binding that worked before regenerates byte-identical, which is what the untouched cshim tests check.

*The generator and the shim have to agree, and nothing else checks that they do.* They are written against one vocabulary spread across two files. So a spelling added to `bindgen.rs` and not to `cshim.rs` passes every unit test on both sides, then fails at `jade pkg install` on a user's machine. `bindgen/tests.rs` closes that loop by driving a header through both halves and compiling the result.

*`include_dirs` is written as an absolute path, on purpose.* The shim is compiled inside `libs/<dep>/` rather than where `jade pkg bind` ran. So a relative `-I` resolves against the wrong directory, and shows up as a "file not found" from cc at install time, well away from the cause.

*The lock and the manifest have to agree on* what a dependency is, *not only on which ones exist.* `verify_in_sync` compared names, so a lock recording `abi = "jade"` survived a manifest corrected to `abi = "c"`.

`ensure_ready` reads the lock rather than resolving again, which is the point of having a lock. So the build skipped the shim and installed the raw C library, and the first complaint came from `dlopen` in the finished program. Any field the lock copies from the manifest, and the build then trusts, belongs in that comparison.

*A `continue` in a loop over dependencies is a decision to install something unusable.* `build_c_shims` skipped a C dependency with no symbol table, which is the one combination that cannot work. Nothing was bound, and a plain C library is exactly what the loader refuses.

`resolve` rejects that case, but `ensure_ready` does not resolve again. So a lock written while the table was present outlives a manifest edit that removed it. It is an error now.

*"It has the right name" is not the same as "it is a library", and nothing between `add` and `dlopen` disagreed.* A dependency was checked for what it *exported*, and never for whether it could be loaded at all. So a file that was not an object file passed through the manifest, `libs/`, resolution, and the linker, and was refused by the dynamic loader in the finished program.

`bindgen::is_loadable_object` reads the magic number. It is called in two places on purpose. `jade pkg add` calls it so it can say what probably went wrong. `materialize` calls it because that is the one point every source passes through with the bytes in hand, and a hand-written manifest or a fresh clone never touches `add`. Anything new that puts a file into `libs/` needs the same check.

*A count returned beside a handle is not a status.* `infer_failure` read any `int` return beside an `out_handle` as a status code, on the reasoning that the handle is the result so the return can only be a status.

`size_t cs_disasm(…, cs_insn **insn)` returns how many instructions it wrote, and a successful disassembly of three raised an error.

The distinction is the *C* spelling. A status is an `int` and a count is a `size_t`. Both collapse to Jade's `int` before the old test ever saw them, which is why the predicate now takes both spellings. Enums arrive as `int` through `build_env`, which is right, because `cs_err` and `lzma_ret` really are statuses.

*An `out_handle` only swallows the return when something is testing it.* `ret_is_a_key` discarded the return unconditionally, so even with the inference fixed, the count had nowhere to go.

A failure convention is what makes a return a status. Without one it is a value, and a pointer to a row whose length the caller cannot know is not much of a result.

*A writable pointer to a complete struct is three different things.* Every one of them used to become an `out_struct`.

A type the library *hands out*, whether returned as a `T*` or written through a `T**`, is a handle. That is what the return position already called it.

A type the caller allocates and the library keeps between calls cannot be an out-parameter at all, because the shim zeroes a fresh local on every call. `lzma_code` bound, compiled, installed, ran, and did nothing. `ZSTD_compressStream` would have written through a NULL `dst`. That one is a *held* struct, allocated once on the C heap and reached through a handle.

Only a record that one call fills stays an out-parameter.

*A generated binding that runs and does nothing is worse than a refusal, and both have happened here.* Every rule in this file that looks over-cautious came from one of them.

`lzma_code` bound against a struct zeroed on every call. `void *fdt` bound as scratch memory sized by a node offset, so fourteen of libfdt's writers handed the library uninitialised memory as the device tree. `uint8_t *out` bound as a one-byte local, for a library that writes twelve. `const uint8_t *` lost its `const` through a typedef and became a buffer the shim allocated, so the caller's data never arrived.

None of those failed at bind time, at compile time, or at link time. So when a coverage number falls after a change here, check which direction it moved in before treating it as a regression.

The test needs *both* signals, and getting it wrong in either direction fails silently. Refusing on "loses a field" alone would take `SF_INFO`-shaped records that carry one `void*`. Refusing on "appears in several functions" alone would take `SF_INFO` itself, which three `sf_open` variants fill.

So `struct_loses_a_field` and `struct_param_counts` are combined with a logical AND. The two existing tests that pin each half still pass unedited, which is the check that the rule did not overreach. Those tests are `an_unrepresentable_field_is_dropped_rather_than_the_whole_struct` and `a_writable_struct_pointer_is_an_out_parameter_and_the_table_follows`.

*clang and `cc` have to be given the same include directories, and they were not.* `header_locations` computed the manifest's `include_dirs` as the user's `-I` directories plus the header's parent, while `bind_header` handed `from_header` only the user's `-I` directories.

So the shim compile could find a neighbouring header that reading the header could not, and the symptom was "clang could not parse" on a header that compiles fine.

`bindgen::include_roots` is now the single source of both. It is called from *inside* `from_header` rather than at the call sites, which is what fixes `discover_header`. That one passed no directories at all, so any candidate needing one was silently demoted to a fallback.

Two directories, for two different includes: `libfdt.h` does `#include <libfdt_env.h>` from its own directory, which an angled include does not search; `brotli/encode.h` does `#include <brotli/port.h>`, which resolves against the directory above. Each cost a library outright. A directory the caller named is searched first, because a guessed root can be wide enough to shadow the header they meant.

*Exactly one directory is dropped again on the way to `cc`: the bound header's own.* A header recorded by its bare name is only findable by putting the directory it sits in on the include path, and that directory belongs to the library.

`libnl-3-dev` ships a `netlink/errno.h`. So binding `netlink/netlink.h` put `/usr/include/libnl3/netlink` on the `-I` path, and the shim's own `#include <errno.h>`, five lines from the top, bound to a file of `NLE_*` constants. The shim failed to compile on `errno`, and the compiler's note said to include `<errno.h>`, which was already there.

Recording the header the way the user wrote it fixes the whole class of problem. Writing `#include <netlink/netlink.h>` against `/usr/include/libnl3` means the leaf directory never goes on the path.

`cli::pkg::nested_spelling` is where that spelling comes from, and it uses the user's own words either way. A relative `--header inc/mylib.h` is taken as written. An absolute one is spelled relative to the deepest directory they named with `-I`. A path derived from the header rather than named by the user is not a candidate, because `/opt/homebrew/include/libfdt.h` would spell as `include/libfdt.h` and drop the directory `libfdt_env.h` is found in.

Two conditions have to hold before that fires, which is why most libraries escape it. There has to be a shadowing header on the path, and one symbol whose failure is reported through `errno`, which is what pulls in `jade_shim_errmsg`. So a library binds cleanly until the day someone binds a symbol that can fail. On an ordinary Linux image, nineteen headers can shadow one the shim includes, including `linux/errno.h` and all of `bsd/`.

*The export table decides what the library really has.* A header is written for the newest version, while the artifact may have been configured without part of it.

libbrotlienc's header declares two functions that no brotli dylib on this machine exports. Binding them produced a shim that compiled and then failed to *link*, and the linker refuses the whole dependency rather than the two symbols. The coverage check did not catch it, because that check only fires when *nothing* matches.

Symbols are now filtered against the export table whenever one can be read. That is the same authority the umbrella-header case already leans on.

*A name in the export table is spelled the way the object format spells it, and two of those spellings include things that are not part of the name.* The table is the authority described above, so reading a name wrongly is the same as the library not having it. Both mistakes here were doing that at scale.

A library built with a version script exports `lzma_version_number@@XZ_5.0`. A double `@@` marks the default version and a single `@` an older one, and both forms sit in the same table.

`dlsym` and the linker resolve the plain name, and `@` cannot appear in a C identifier, so the suffix is nothing anything downstream can use. A `"?"` placeholder written with one produced a shim that would not compile. Cutting at the first `@` is safe whatever the format.

On an `ubuntu:24.04` image, this meant 56 libraries binding nothing at all, including `libc`, `libcrypto`, and `libcurl`. Another 14, zlib among them, bound *successfully* while dropping exactly their versioned half. That second outcome is worse, because the exit status says nothing about it.

The leading underscore is Mach-O's alone: there a C function `foo` is `_foo` in the table, and ELF adds no prefix. Stripping it everywhere was wrong in both directions. `__gmpz_init` stopped matching the header declaring it, so `jade pkg add gmp` skipped all 371 declarations against a library exporting every one. And a library exporting `_alpha` started matching a header declaring `alpha`, which is the direction with no diagnostic anywhere: nothing skipped, nothing assumed, `install` clean, and `undefined symbol: alpha` when the program runs.

*Which format that is comes from the artifact's first four bytes, not from `cfg!`.* The two answer different questions. `cfg!` says what this build of `jade` runs on, and the question here is what the *file* is.

Jade is Unix-only, and a Mac reading a `.so` is ordinary, so the host is not a safe stand-in. `object_format` reads the magic number, which `is_loadable_object` was already reading for its own reasons.

`cfg!` survives as the fallback for a file whose magic number names no format, which in practice means a static archive. An archive on this machine was built for this machine.

*A leading underscore is not evidence that a name is private.* `placeholder_symbols` filtered on one. That was the Mach-O rule applied a second time, in a place where it means something else, because on ELF `__gmpz_init` is public API.

A placeholder does nothing on its own. It lands in `jade.toml` as `"?"`, and every command that would use it refuses it by name. So an extra entry costs one line in a list, while a missing one produces the false "not exported by the library" again.

What is left out now is only what no prototype could rescue: a C++ mangled name, and the loader's own `_init`, `_fini`, and `_start`.

*Types come from the whole translation unit, while functions come only from the header you named.* The two need different scopes, and they used to share one.

A library splits its types into `git2/types.h` and declares functions against them across twenty other files. So an environment built from a single file reported every one of those functions as taking an unsupported type.

Types are safe to take from everywhere, because nothing is emitted for a type on its own. A type is recorded only because a bound function reached it. Functions are not safe to take from everywhere, or binding `archive.h` would bind `stdio.h` along with it.

*What a header includes is scoped by the export table, not by guessing from paths.* `bindable` binds the named header's own declarations, plus every declaration in the translation unit that the artifact also exports.

That is an exact test. `fopen` is in that translation unit and is not in liblzma. Asking "which directories are system ones" instead would have been a guess, and it breaks the moment a library lives in `/opt/homebrew/include` alongside its own dependencies.

This started as a rule for umbrella headers alone. `lzma.h`, `git2.h`, and `alsa/asoundlib.h` declare nothing themselves and exist to include the files that do. Pointing at one reported "no declarations found", and pointing at a sub-header failed differently, because a sub-header usually does not compile alone.

But the rule was all or nothing. A header declaring anything of its own bound only its own declarations, and plenty of libraries do both. `ares.h` declares about seventy symbols and includes `ares_dns_record.h`, which declares sixty-three more. The whole modern DNS record API was invisible, and nothing reported it, because a symbol never reached is a symbol with nothing to refuse.

The same exact test settles both cases, so it now runs for both. A header's own declarations are kept unconditionally, which makes the rule additive. They are what the user pointed at, and an exported-only rule would drop one the artifact happens not to export.

Without an artifact there is nothing to test against: a header with its own declarations binds those alone, and an umbrella is refused with a message naming `--path`.

*One symbol that cannot be bound must not take the dependency with it.* `from_header` resolved a symbol's structs inside a nested loop and called `continue` on failure. That continued the *inner* loop, so the symbol was emitted anyway while its field table was dropped.

`cshim` refuses an `out_struct:` naming a table that is not there, and it refuses the whole dependency rather than the one symbol. So a single struct of unrepresentable fields made an otherwise fine library impossible to install. `sqlite3_snapshot_free` and `zip_file_attributes_init` are both that shape.

The structs are resolved together now, and the symbol is skipped as a unit, with the reason recorded in the report.

*A C `enum` is an `int`, and recording that belongs in the type environment.* Putting it in the mapper would mean repeating it at every place a type is looked up: return, parameter, and struct field. So it goes in `build_env` as an alias, and every path resolves through `expand`.

Both spellings clang gives have to be checked. For `typedef enum { ... } lzma_ret;`, the `qualType` is `enum lzma_ret` while the `desugaredQualType` is the bare `lzma_ret`. `underlying` prefers the desugared one, which is the spelling with the keyword already gone. Missing this cost 60 of liblzma's 114 symbols, `lzma_code` among them.

*A normalized type name is a lookup key, not source text.* `normalize` drops `struct`, `union`, and `enum` so a type can be found however it was written. The stripped name was then also written into the generated shim.

For `typedef struct sqlite3 sqlite3;` that is harmless, because the bare name really is a type. Every fixture in the suite used that shape, which is why nothing caught the bug.

For the far more common `typedef struct X_s X;`, or a bare `struct X_s;`, `X_s` on its own is not a type in C, and the shim would not compile.

`TypeEnv::c_name` puts the keyword back, and `TypeEnv::tagged` is what knows whether it is needed. Anything new that turns a resolved type into shim text needs the same treatment.

*The two hints that tell a user what to write have to agree with what the generator accepts.* `unresolved_report` and the note from `jade pkg add` both showed `args = ["int", "int"]` as the shape to replace a `"?"` with. But a `"?"` means no header was read, which is exactly where `int` is now refused. Following the hint landed the user on a second error.

Both now lead with `--header` and then show `scalar:<ctype>`. `unresolved_report` still shows plain `int` when the dependency *does* carry headers, because that is the case where `int` is both the easier spelling and the correct one.

*A placeholder has to be refused everywhere the binding is used, not only where it is generated.* A `"?"` passes `resolve`, because the table is non-empty and that is all resolution asks. So the refusal lives in `build_c_shims`, and in `ensure_ready` ahead of the lock read.

The second place matters. Without it, `jade run` on a fresh project answers "there is no jade.lock, run `jade pkg install`", and the user spends a whole command to arrive at the message they should have had first.

`cli/check.rs` runs the same check against the manifest alone. That costs a read it was already doing, and it keeps `jade check` an honest predictor of `jade run` without installing anything.

*`CSymbol` deserializes by hand rather than with `#[serde(untagged)]`.* An untagged enum reports every failure as "data did not match any variant". So accepting the `"?"` string that way would have cost every *table* its "missing field `ret`" message.

The visitor handles the string case itself and hands the map case to a derived struct, which leaves those messages exactly as they were. A test pins the behavior.

*A thread-exit hook cannot read a thread-local, and a version that tries still runs.* The per-thread buffer behind `jade_shim_owned` is released through a `pthread_key_create` destructor. C11's `_Thread_local` has no destructor, and both engines retire idle pool workers after ten seconds. Threads come and go for as long as the program runs, so the last buffer each one held accumulates rather than staying capped at one per pool slot.

The trap is in how the destructor finds the buffer. Written to read a `_Thread_local`, it compiles, it is called once per thread, and it frees nothing. On macOS, the thread's thread-local storage is already torn down by the time key destructors run, so the pointer reads back null. Peak resident memory was identical with the hook and without it, and only measuring across a few thousand threads revealed the problem.

So the buffer travels in the key's own value, which is what the destructor is handed. That value is a small holder rather than the buffer itself, because `realloc` moves the buffer, and a key naming a released block would be a double free at thread exit. `compile_shim` passes `-pthread` for this.

*A present artifact is not the same as a current artifact.* `materialize` compares `libs/` against the *lock*, so anything that changes the true source without changing the lock is invisible to it. That is exactly how a rebuilt `path` dependency used to keep running as the copy it was when it was added.

`refresh_local` closes that for local sources. Any future kind of source that can change in place needs the same treatment, and adding one without it brings back the same silent staleness.

## Building and testing

```sh
cargo test pkg::
```
