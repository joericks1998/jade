# Binding shim rewrite rules

Status: shipped in v1.3.0, and extended in v1.3.7 — out-parameters for scalars,
more than one out-parameter per symbol, and the rule that a struct the caller
keeps between calls is not an out-parameter at all. Governs `cshim.rs`.

## The problem

A C signature and a callable Jade signature are not the same shape, and the gap
is not a detail — it is most of what stops a real library from being bindable.

```c
int sf_read_short(SNDFILE* f, short* buf, int count);
int sf_open(const char* path, int mode, SF_INFO* info);
```

Neither can be called from Jade as written. The first wants a buffer the caller
allocated and reports how much of it was filled. The second returns one thing
and writes another through a pointer. A one-to-one mapping of parameters cannot
express either, so the shim rewrites the call instead of merely forwarding it.

The declared `args` list therefore describes the **C** signature. The Jade
signature is derived from it, and is deliberately a different length.

## The rules

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

## Which directories a header is read from

A header is rarely self-contained, and the two ways it reaches its neighbours
need two different directories:

- `libfdt.h` does `#include <libfdt_env.h>`, which sits *beside* it. An angled
  include does not search the including file's own directory, so that directory
  has to be passed explicitly.
- `brotli/encode.h` does `#include <brotli/port.h>`, which resolves against the
  directory *above* the header.

Both are searched, in that order, after any directory the caller named. The
caller's wins because a guessed root can be wide enough — `/opt/homebrew/include`
— to shadow the header they meant.

The same list is what goes into the manifest's `include_dirs`, so the shim
compile is given exactly what reading the header was given. Computing the two
separately is what went wrong before: `cc` got the header's directory and clang
did not, and the failure was "clang could not parse" on a header that was fine.

## Why `out_struct` requires a header

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

## A writable struct pointer is three different things

`int f(S* s)` where the header defines `S` looks like one shape and is three,
and binding all of them as out-parameters is how twelve of liblzma's symbols
came to compile, install, run and do nothing.

**The library hands it out.** If some function returns `S*`, or writes one
through an `S**`, then the library owns the allocation and Jade should hold it.
That is a handle — the same answer the return position already gave the same
type, so reading it here removes an asymmetry rather than adding a rule.

**The caller allocates it and the library keeps it.** `lzma_stream`,
`ZSTD_outBuffer`, `fd_set`. An `out_struct` shim declares a *zeroed local* every
call and reads the carryable fields back out, which is right for a record one
call fills and ruinous for a struct threaded through a sequence: the encoder
initialises a stream and throws it away, and the next call runs against a
different zeroed one. `ZSTD_outBuffer` is worse still — its `void* dst` would be
zeroed to NULL and written through.

**It is a record one call fills.** `SF_INFO`. This is what out-parameters are
for, and it stays.

Two signals separate the second from the third, and *both* are required:

| | loses a field | threaded through several calls | verdict |
|---|---|---|---|
| `SF_INFO` | no | yes (three `sf_open` variants) | record |
| `S { int; void*; int }` | yes | no (one function) | record, field dropped |
| `lzma_stream` | yes | yes (thirteen functions) | caller-held state |

Neither alone is enough. Losing a field is not disqualifying, because a record
read once and discarded does not miss what was dropped — and refusing on that
alone would take `SF_INFO`-shaped structs carrying one `void*`. Appearing in
several functions is not disqualifying either, because filling the same record
from three entry points is ordinary. It is the *combination* — state that is
kept, and cannot be carried — that cannot work under any binding.

The cost is honest and worth stating: liblzma's reported coverage falls from 49
symbols to 36. Those thirteen were not working before; they were reporting
success.

## Ownership at the boundary

A value **inside a container** is container-owned, so Jade's `ffi_free` frees
it. A struct field holding a string must therefore be `strdup`'d, not borrowed:
handing over a pointer into the shim's stack local would be a free of the stack,
and a pointer into the library's memory would be a free of the library's.

This is the one place the rule differs from a top-level return, where a string
is handed over borrowed and Jade copies it. Same tag, opposite ownership,
decided by where the value sits.

## Handles

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

## Callbacks

`callback:<ret>(<arg>,…)`, and the signature is written in the library's **own C
types** — `callback:int(int, const char*)`, not Jade's widened ones. That is not
a detail: the shim declares a function pointer the library will store and call,
so `int` widened to `int64_t` is not a truncation but an incompatible function
pointer, and a call through the wrong ABI.

No `libffi` is involved, and that is the whole payoff of generating the shim from
a declaration rather than dispatching at run time: the signature is known when
the C is written, so a real static function of that shape can just be declared.

Two rules follow from where the callback runs:

**The registration lasts exactly one call.** The slot is `_Thread_local` and set
only for the duration of the native call. A library that stores the callback and
invokes it later finds an empty slot and gets the neutral answer, rather than a
stale pointer into an interpreter that has moved on. Asynchronous registration
is not supported and cannot be without keeping the interpreter available
indefinitely.

**A raise is deferred, never unwound.** The trampoline records the failure and
returns; the wrapper turns it into a Jade error *after* the library has returned
normally. Letting the raise out would unwind through the library's frames
mid-operation.

A callback may only give back a scalar, for the same reason an out-buffer is the
shim's memory: anything else would have to be released inside a C frame by code
that has no idea it is holding a Jade value.

## What is deliberately not here

**Input structs.** A Jade struct crossing *into* a C function would need the
shim to build one from Jade fields. Not built, but the stated reason for that —
"it needs the same layout guarantees in the other direction" — no longer holds:
the guarantee comes from including the real header, and a header is symmetric.
Nor is it unasked for; across four libraries surveyed for v1.3.7 it is sixteen
symbols, `lzma_stream_flags_compare` among them.

What it really needs is an answer to the mirror of the out-struct question:
what the shim writes into a field Jade did not supply. Zero is right for a
`reserved_*` field and wrong for a meaningful pointer, which is the same
distinction `struct_loses_a_field` already draws for the out direction.

**Caller-held mutable state**, and this one *is* a language limit rather than
unbuilt work. The shim could heap-allocate the struct and hand Jade an opaque
handle, and for a library whose caller never touches the fields that would be
enough. `lzma_stream` is not one: its protocol is field-poking — `next_in` and
`avail_in` point into the caller's input buffer and `next_out`/`avail_out` into
the output buffer, reset between every call. Jade has no raw pointers and its
`bytes` is immutable by design, so there is nothing to set them to. The blocker
is not "a struct cannot cross the boundary" but "a pointer into a buffer Jade
does not own cannot", which is deliberate.

**A callback taking a `void *`.** The usual `user_data` parameter names no type,
so there is nothing to hand Jade. That is the most common reason a real
callback still does not fit.
