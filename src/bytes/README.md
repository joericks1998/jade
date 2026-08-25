# `src/bytes/`: the `bytes` type, its three methods, and `std::bytes`

## What this subtree is

Two things that belong together. The method surface of the `bytes` type, which is `len`, `decode`, and `slice`. And the `std::bytes` package, which is the three ways to *build* a blob: `zeros`, `from_ints`, and `concat`.

The value itself is not here. A blob lives in `jade_runtime::bytesf::BytesObj`, shared with the AOT heap, so both engines agree on what a `bytes` value *is*. This directory decides what the VM does when you call a method on one, or call one of the package functions.

## Why `bytes` is not just a string

A Jade `str` is UTF-8 and NUL-terminated. Arbitrary octets are neither. A PNG contains NUL bytes and sequences that are not valid UTF-8, so holding one in a `str` cuts it off at the first NUL and corrupts it on any operation assuming valid text.

`bytes` exists so data can pass through a program unchanged: in from a file or a socket, out to stdout. It is not meant to be a second string type with a parallel set of operations. That is why there are three methods rather than thirty.

Converting between the two is explicit in both directions and never automatic. `s.encode()` gives the octets of a string, and `b.decode()` gives the text of a blob.

## Why construction is a package and not more methods

`str.encode()` was the only way a program could make a blob from values it held, and it cannot make an arbitrary one: a zero byte truncates the string it comes from, and anything above 127 encodes as two octets rather than one. So a program could receive a pixel buffer over the FFI and never build one. That was the gap `std::bytes` closes.

The three functions are a package rather than three more methods for two reasons. A constructor has no receiver, so it was never going to be a method. And the count above is deliberate: growing the method surface is exactly what this type is designed not to do.

## Why writing is `b[i] = v` and not `b.set(i, v)`

Reading one octet was already spelled `b[i]`. An array already writes with `a[i] = v`. Using the same spelling adds no new concept, reuses the `SetIndex` opcode both engines already lower, and keeps the method count at three.

It also means the write path is *two* instructions and not one. The emitter picks `SetIndex` for a local binding and `SetIndexGlobal` for a module-level one, and they are separate arms in both engines. An implementation that handles only the first makes `b[0] = 1` work inside a function and fail at the top level.

## What each file does

- *`mod.rs`* holds the three methods, the three package functions, `find_bytes_method`, the `BYTES_PKG` registration, and the type registration that lets `b.len()` be checked before it runs.
- *`tests.rs`* covers the behaviors below, plus the NUL and invalid-UTF-8 cases that are the reason this type exists.

## The decisions worth knowing

*A blob is reference-semantic.* Since v1.3.27 it is mutable, so two names for one buffer see the same write, exactly as they do for an array. `VmValue::Bytes` holds `Arc<Mutex<BytesObj>>`; the lock lives in the VM's wrapper rather than inside `BytesObj`, because that type is `repr(C)` and shares its layout with the compiled heap and the native package ABI, neither of which has one. Nothing synchronises the payload on the AOT path either. What keeps two tasks off one buffer is `compiler::taskcheck`, the same rule that already covered arrays.

*`decode` raises on invalid UTF-8* rather than substituting `�`. Quietly corrupting data is worse than a catchable error. A caller who wants lossy behavior can ask for it, while a caller who assumed the bytes were text needs to hear that they were not.

*Trust travels with the octets.* A blob carries a trust byte the way a string does, so `fs.read_bytes(p).decode()` produces a *tainted* string. Without that, decoding would be a way to strip the taint. Data straight off the disk would walk past the check in `sh.exec` that `fs.read(p)` cannot. `concat` takes the more restrictive of its two inputs for the same reason: joining a file's contents onto a buffer the program built itself must not hand back a clean blob.

*An int carries no trust, and `from_ints` therefore launders.* A program that reads a tainted blob, walks it into an int array, and rebuilds it gets a trusted one. That is accepted rather than closed. Trust follows values that can hold it and a number is not one, which is already true of `len()` and of any arithmetic on something read from a file. Closing it would mean a trust byte on every array and every arithmetic result.

*`slice` clamps instead of raising.* Reading past the end is how you take the tail of a buffer, and every caller would otherwise write the same `min()` call.

*Nothing here raises `JadeError::Exception`.* That variant means a `raise` the program wrote, and the VM answers one by handing the catch block `state.raised_exception`, which a built-in never fills in. `bytes.decode` used to raise it, so a caught decode failure bound the bare string `"unknown exception"` under `jade run` and a `RuntimeError` struct under `jade build`. A test in `tests.rs` pins the rule.

## Who uses it

`src/vm/dispatch.rs` routes a method call on a `VmValue::Bytes` here, and its two index-assignment arms call `write_octet` for `b[i] = v`. The compiled backend does not use this directory at all. It lowers the same three methods to `jrt_bytes_*` symbols in `jade_runtime::bytesf`, lowers the package functions to the `jk_bytes_*` forwarders in `src/runtime_aot/common.c`, and writes an octet through the `JK_BYTES` arm of `jrt_val_set_index`. So a change to any behavior has to land in both places, or the engines disagree.

The message text for every failure lives once, in `jade_runtime::bytesf`, and both engines format from there. A program can catch these, which makes the wording part of the language rather than an implementation detail.

The things that produce `bytes` values elsewhere: `read_bytes` and `read_stdin_bytes` in `std::fs`, `get_bytes` and `post_bytes` in `std::http` and `std::uhttp`, `str.encode()`, and a native package handing one back across the FFI.

## Building and testing

```sh
cargo test bytes::
cargo test -p jade-runtime bytesf
./target/debug/jade run examples/bytes/construct/construct.jde
./target/debug/jade run examples/bytes/buffer/buffer.jde
./src/scripts/backend-parity.sh
```

The parity script is the one that matters. Everything here has a second implementation in the compiled backend, and only running the same program both ways proves the two agree.
