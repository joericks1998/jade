# `src/bytes/`: the `bytes` type, its three methods, and `std::bytes`

## What this subtree is

Two things that belong together. The method surface of the `bytes` type, which is `len`, `decode`, and `slice`. And the `std::bytes` package, which is the three ways to *build* a blob: `zeros`, `from_ints`, and `concat`.

The value itself is not here, and neither is any behavior. A blob is a heap object with the string layout, `{ rc, len, trust, bytes…, 0 }`, and `src/backend/bytes.rs` emits every method and package function into the program that uses it. Since v2 this directory only names them and gives the checker their types. Until v1.4.8 it also held what the VM did when you called one.

## Why `bytes` is not just a string

A Jade `str` is UTF-8 and NUL-terminated. Arbitrary octets are neither. A PNG contains NUL bytes and sequences that are not valid UTF-8, so holding one in a `str` cuts it off at the first NUL and corrupts it on any operation assuming valid text.

`bytes` exists so data can pass through a program unchanged: in from a file or a socket, out to stdout. It is not meant to be a second string type with a parallel set of operations. That is why there are three methods rather than thirty.

Converting between the two is explicit in both directions and never automatic. `s.encode()` gives the octets of a string, and `b.decode()` gives the text of a blob.

## Why construction is a package and not more methods

`str.encode()` was the only way a program could make a blob from values it held, and it cannot make an arbitrary one: a zero byte truncates the string it comes from, and anything above 127 encodes as two octets rather than one. So a program could receive a pixel buffer over the FFI and never build one. That was the gap `std::bytes` closes.

The three functions are a package rather than three more methods for two reasons. A constructor has no receiver, so it was never going to be a method. And the count above is deliberate: growing the method surface is exactly what this type is designed not to do.

## Why writing is `b[i] = v` and not `b.set(i, v)`

Reading one octet was already spelled `b[i]`. An array already writes with `a[i] = v`. Using the same spelling adds no new concept, reuses the index assignment the compiler already lowers, and keeps the method count at three.

It also means the write has *two* targets and not one. A local binding is a `Place::Local` in MIR and a module-level one is a `Place::Global`, and the backend reads each its own way. Until v1.4.8 they were two bytecode instructions, `SetIndex` and `SetIndexGlobal`, with separate arms in each engine. An implementation that handles only the first makes `b[0] = 1` work inside a function and fail at the top level, which is why `examples/bytes/buffer/` writes both ways.

## What each file does

- *`mod.rs`* holds `BYTES_METHODS`, which names the three methods, the `BYTES_PKG` registration, which names the three package functions, and the type registration that lets `b.len()` be checked before it runs.

## The decisions worth knowing

*A blob is reference-semantic.* Since v1.3.27 it is mutable, so two names for one buffer see the same write, exactly as they do for an array. A blob carries no lock. Its layout is a reference count, a length, a trust word and the octets, and the native package boundary it crosses has no lock either. Until v1.4.8 the VM wrapped it in a `Mutex`, but nothing synchronises the payload in a compiled binary. What keeps two tasks off one buffer is `mir::taskcheck`, the same rule that already covered arrays.

*`decode` raises on invalid UTF-8* rather than substituting `�`. Quietly corrupting data is worse than a catchable error. A caller who wants lossy behavior can ask for it, while a caller who assumed the bytes were text needs to hear that they were not.

*Trust travels with the octets.* A blob carries a trust word the way a string does, so `fs.read_bytes(p).decode()` produces a *tainted* string. Without that, decoding would be a way to strip the taint. Data straight off the disk would walk past the check in `sh.exec` that `fs.read(p)` cannot. `concat` takes the more restrictive of its two inputs for the same reason: joining a file's contents onto a buffer the program built itself must not hand back a clean blob.

*An int carries no trust, and `from_ints` therefore launders.* A program that reads a tainted blob, walks it into an int array, and rebuilds it gets a trusted one. That is accepted rather than closed. Trust follows values that can hold it and a number is not one, which is already true of `len()` and of any arithmetic on something read from a file. Closing it would mean a trust word on every array and every arithmetic result.

*`slice` clamps instead of raising.* Reading past the end is how you take the tail of a buffer, and every caller would otherwise write the same `min()` call.

*A failed decode raises a `RuntimeError` struct.* `jade.bytes.decode`, emitted by `src/backend/bytes.rs`, raises through `fail_str` in `src/backend/errors.rs`, like every other failure in support code. Until v1.4.8 the VM's `bytes.decode` raised `JadeError::Exception` instead. That variant means a `raise` the program wrote, and the VM answered one by handing the catch block `state.raised_exception`, which a built-in never filled in. So a caught decode failure bound the bare string `"unknown exception"` under the VM and a `RuntimeError` struct under `jade build`.

## Who uses it

`src/builtins/` lists `BYTES_METHODS` among the primitive-method tables and `BYTES_PKG` among the packages, and the type checker reads both. The compiled backend does all the work. `typechecker/infer/builtins.rs` gives each call its static type, `mir/` lowers the methods and package functions to `Op::Bytes*` operations, and `src/backend/bytes.rs` emits the code for each, including `jade.bytes.set` for writing an octet. So a change to behavior lands there, and a new method or function also needs its name added here.

The message text for every failure lives once, in `src/backend/bytes.rs`, and matches what v1 printed. A program can catch these, which makes the wording part of the language rather than an implementation detail.

The things that produce `bytes` values elsewhere: `read_bytes` and `read_stdin_bytes` in `std::fs`, `get_bytes` and `post_bytes` in `std::http` and `std::uhttp`, `str.encode()`, and a native package handing one back across the FFI.

## Building and testing

```sh
cargo test backend::
./target/debug/jade run examples/bytes/construct/construct.jde
./target/debug/jade run examples/bytes/buffer/buffer.jde
./compiler/src/scripts/golden.sh
```

The golden script is the one that matters. It builds every example, these two included, and compares each binary's output with what the VM and the compiled backend agreed on at v1.4.8.
