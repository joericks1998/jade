# `src/bytes/`: primitive methods on a binary blob

## What this subtree is

This is the method surface of the `bytes` type: `len`, `decode`, and `slice`. That is the whole list, and it is short on purpose.

The value itself is not here. A blob lives in `jade_runtime::bytesf::BytesObj`, shared with the AOT heap, so both engines agree on what a `bytes` value *is*. This directory only decides what the VM does when you call a method on one.

## Why `bytes` is not just a string

A Jade `str` is UTF-8 and NUL-terminated. Arbitrary octets are neither. A PNG contains NUL bytes and sequences that are not valid UTF-8, so holding one in a `str` cuts it off at the first NUL and corrupts it on any operation assuming valid text.

`bytes` exists so data can pass through a program unchanged: in from a file or a socket, out to stdout. It is not meant to be a second string type with a parallel set of operations. That is why there are three methods rather than thirty.

Converting between the two is explicit in both directions and never automatic. `s.encode()` gives the octets of a string, and `b.decode()` gives the text of a blob.

## What each file does

- *`mod.rs`* holds the three methods, the `find_bytes_method` lookup, and the type registration that lets `b.len()` be checked before it runs.
- *`tests.rs`* covers the behaviors below, plus the NUL and invalid-UTF-8 cases that are the reason this type exists.

## The three decisions worth knowing

*`decode` raises on invalid UTF-8* rather than substituting `�`. Quietly corrupting data is worse than a catchable error. A caller who wants lossy behavior can ask for it, while a caller who assumed the bytes were text needs to hear that they were not.

*Trust travels with the octets.* A blob carries a trust byte the way a string does, so `fs.read_bytes(p).decode()` produces a *tainted* string. Without that, decoding would be a way to strip the taint. Data straight off the disk would walk past the check in `sh.exec` that `fs.read(p)` cannot.

*`slice` clamps instead of raising.* Reading past the end is how you take the tail of a buffer, and every caller would otherwise write the same `min()` call.

## Who uses it

`src/vm/dispatch.rs` routes a method call on a `VmValue::Bytes` here. The compiled backend does not. It lowers the same three methods to `jrt_bytes_*` symbols in `jade_runtime::bytesf`, so a change to a method's behavior has to land in both places or the engines disagree.

The things that produce `bytes` values live elsewhere: `read_bytes` and `read_stdin_bytes` in `std::fs`, `get_bytes` and `post_bytes` in `std::http` and `std::uhttp`, and `str.encode()`.
