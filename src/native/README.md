# `src/native/` — the VM's C-ABI package loader

## What this subtree is

How the interpreter loads and calls a native shared library. When a `use` resolves to a `.dylib` / `.so` / `.dll`, this module `dlopen`s it, calls its `jade_pkg_init` entry point to collect the name-to-function-pointer bindings, and marshals `VmValue`s across the C boundary in both directions.

## Why it was built this way

A single `.dylib` has to serve both engines. The AOT counterpart is `src/runtime_aot/native.c`, and it mirrors this file deliberately: same registry shape, same `jade_pkg_init` invocation, same marshalling. If the two drift, a package works under `jade run` and misbehaves when compiled, and the symptom is corrupted values rather than a clean error.

The ABI is narrow on purpose. Values cross as a tagged union of `nil`, `int`, `float`, `bool`, `str`, `error`, `array`, and `dict` — no structs, no callbacks. Widening it would mean a `libffi` dependency and two implementations of arbitrary-signature dispatch, one per engine. `pkg/cshim.rs` exists precisely so a plain C library can be wrapped into this ABI instead.

The one subtle rule is **who owns which buffer**. For input arguments, Jade owns the string buffer. For output values, the native library owns it and must keep it valid through the return of the native function — Jade copies immediately. Array and dict trees crossing the boundary are deep-copied into the **libc heap**, so that either `jade-runtime` instance in the process (this VM, and each dlopen'd package, each with its own allocator pool) can free them. That is the reason this file declares `malloc` and `free` directly rather than using Rust's allocator.

## What each file does

- **`mod.rs`** — the tag constants (`JADE_TAG_NIL` … `JADE_TAG_DICT`), the `JadeVal` repr-C union, `load_native_package`, and the `vm_to_ffi` / `ffi_to_vm` marshalling in both directions including the `JadeArr` / `JadeMap` transport trees.
- **`tests.rs`** — loader and marshalling tests.

## Who uses it

*Depends on:* `libloading` for `dlopen`, `vm::VmValue`, `builtins::make_array`, and `jade_runtime::coll::DictObj`.

*Used by:* `vm/chunk.rs` when a `use` resolves to a native library, and `llm/provider_backend.rs` to load a provider package. Its mirror image is `src/runtime_aot/native.c`.

## Gotchas

Any change to marshalling has to land in `runtime_aot/native.c` at the same time. The tag constants are duplicated in `runtime.h`.

`JADE_TAG_ERROR` is a string tag whose payload is an error message — a package signals failure by returning it, not by any out-of-band mechanism.

## Building and testing

```sh
cargo test native::
```

Build a Jade package to test against with `jade build lib.jde --lib`.
