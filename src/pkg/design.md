# Binding shim rewrite rules

Status: shipped in v1.3.0. Governs `cshim.rs`.

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

**At most one out-parameter per symbol.** Two would have to come back as a pair
with no obvious names. Splitting the binding is clearer than inventing them.

**A symbol with an out-parameter and a return value comes back as `.ret` and
`.out`.** A C function that fills a struct *and* returns a status has two
results and Jade has one slot. Fixed names rather than configurable ones — there
is nothing to decide, and a name in a config file is a name that can be wrong.
When the C function returns `void` there is no pair to make, so the filled
struct is the result directly and the common case stays clean.

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

## Ownership at the boundary

A value **inside a container** is container-owned, so Jade's `ffi_free` frees
it. A struct field holding a string must therefore be `strdup`'d, not borrowed:
handing over a pointer into the shim's stack local would be a free of the stack,
and a pointer into the library's memory would be a free of the library's.

This is the one place the rule differs from a top-level return, where a string
is handed over borrowed and Jade copies it. Same tag, opposite ownership,
decided by where the value sits.

## What is deliberately not here

**Input structs.** A Jade struct crossing *into* a C function would need the
shim to build one from Jade fields, which needs the same layout guarantees in
the other direction. Nothing has asked for it yet.

**More than one out-parameter**, per the rule above.

**Callbacks.** They need a C-callable trampoline that re-enters Jade, which is a
different class of problem from marshalling a value. See the plan for v1.3.0.
