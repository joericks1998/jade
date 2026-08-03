# `src/bytes/` — primitive methods on a binary blob

## What this subtree is

The method surface of the `bytes` type: `len`, `decode`, `slice`. That is the
whole list, and it is short on purpose.

The value itself is not here. A blob lives in `jade_runtime::bytesf::BytesObj`,
shared with the AOT heap, so both engines agree on what a `bytes` value *is*.
This directory only decides what the VM does when you call a method on one.

## Why `bytes` is not just a string

A Jade `str` is UTF-8 and NUL-terminated. Arbitrary octets are neither. A PNG
contains NUL bytes and sequences that are not valid UTF-8, so holding one in a
`str` truncates it at the first NUL and corrupts it on any operation that assumes
valid text. `bytes` exists so data can pass through a program unchanged — in from
a file or a socket, out to stdout — and not to be a second string type with a
parallel set of operations. That is why there are three methods rather than
thirty.

Conversion between the two is explicit in both directions and never implicit:
`s.encode()` gives the octets of a string, `b.decode()` gives the text of a blob.

## What each file does

- **`mod.rs`** — the three methods, the lookup `find_bytes_method`, and the type
  registration so `b.len()` checks before it runs.
- **`tests.rs`** — the behaviors below, plus the NUL and invalid-UTF-8 cases that
  are the reason the type exists.

## The three decisions worth knowing

**`decode` raises on invalid UTF-8** rather than substituting `�`. Silently
corrupting data is worse than a catchable error: a caller who wanted lossy
behavior can ask for it, but one who assumed the bytes were text needs to hear
that they were not.

**Trust travels with the octets.** A blob carries a trust byte the way a string
does, so `fs.read_bytes(p).decode()` yields a *tainted* string. Without that,
decoding would be a laundering step — data straight off the disk would walk past
the check in `sh.exec` that `fs.read(p)` cannot.

**`slice` clamps instead of raising.** Reading past the end is how you take the
tail of a buffer, and every caller would otherwise write the same `min()`.

## Who uses this

`src/vm/dispatch.rs` routes a method call on a `VmValue::Bytes` here. The
compiled backend does not: it lowers the same three methods to `jrt_bytes_*`
symbols in `jade_runtime::bytesf`, so a change to a method's behavior has to
land in both or the engines disagree.

Producers of `bytes` values live elsewhere — `std::fs` (`read_bytes`,
`read_stdin_bytes`), `std::http` and `std::uhttp` (`get_bytes`, `post_bytes`),
and `str.encode()`.
