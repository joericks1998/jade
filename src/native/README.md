# `src/native/` — the VM's C-ABI package loader

## What this subtree is

How the interpreter loads and calls a native shared library. When a `use` resolves to a `.dylib` / `.so` / `.dll`, this module `dlopen`s it, calls its `jade_pkg_init` entry point to collect the name-to-function-pointer bindings, and marshals `VmValue`s across the C boundary in both directions.

## Why it was built this way

A single `.dylib` has to serve both engines. The AOT counterpart is `src/runtime_aot/native.c`, and it mirrors this file deliberately: same registry shape, same `jade_pkg_init` invocation, same marshalling. If the two drift, a package works under `jade run` and misbehaves when compiled, and the symptom is corrupted values rather than a clean error.

The ABI is narrow on purpose. Values cross as a tagged union of `nil`, `int`, `float`, `bool`, `str`, `error`, `array`, `dict`, `struct`, and `bytes` — no callbacks, no functions. Widening it further would mean a `libffi` dependency and two implementations of arbitrary-signature dispatch, one per engine. `pkg/cshim.rs` exists precisely so a plain C library can be wrapped into this ABI instead.

Bytes is the newest tag, and it carries a length rather than relying on a terminator. That is the whole reason it is not a `str`: a blob may contain NUL bytes and need not be valid UTF-8, so a `char*` would truncate one and corrupt the other. Data arriving from a package is marked **tainted**, for the reason a file read is — it came from outside the program.

A struct is the odd one out: it is a dict that also carries its **type name**. That name is the point. A dict with the wrong keys reads as a set of nils and fails silently, so two programs sharing a dict share a convention; two programs sharing a struct share a type the receiver can check. The inference boundary is what drove it — `src/llm/provider_backend.rs` hands a provider package an `InferRequest` rather than an anonymous bag of keys, and reads back frames named `Token` or `Error`.

The name that crosses is the struct's **source** name. `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2` while flattening imports, and that name is baked into the compiled library — so `abi_type_name` strips a trailing `$<digits>` on the way out, and `runtime_aot/native.c`'s `ffi_strdup_abi_type` strips the same thing. The number describes the importing program's module graph, not the type, so it means nothing on the other side of the call. Without stripping it, a provider package built with `use ovata::infer` returns frames named `Token$0` and the caller does not recognise its own protocol.

The one subtle rule is **who owns which buffer**. For input arguments, Jade owns the string buffer. For output values, the native library owns it and must keep it valid through the return of the native function — Jade copies immediately. Array, dict, struct and bytes payloads crossing the boundary are deep-copied into the **libc heap**, so that either `jade-runtime` instance in the process (this VM, and each dlopen'd package, each with its own allocator pool) can free them. That is the reason this file declares `malloc` and `free` directly rather than using Rust's allocator. A top-level string is the one exception, handed over borrowed; a blob is copied at every level, top included, so `ffi_free` has to reclaim it there too.

## What each file does

- **`mod.rs`** — the tag constants (`JADE_TAG_NIL` … `JADE_TAG_BYTES`), the `JadeVal` repr-C union, `load_native_package`, and the `vm_to_ffi` / `ffi_to_vm` marshalling in both directions including the `JadeArr` / `JadeMap` / `JadeStruct` / `JadeBytes` transport trees.
- **`tests.rs`** — loader and marshalling tests.

## Who uses it

*Depends on:* `libloading` for `dlopen`, `vm::VmValue`, `builtins::make_array`, and `jade_runtime::coll::DictObj`.

*Used by:* `vm/chunk.rs` when a `use` resolves to a native library, and `llm/provider_backend.rs` to load a provider package. Its mirror image is `src/runtime_aot/native.c`.

## Gotchas

**A package declares the value ABI it was built against, and an incompatible one is refused at load.** `jade build --lib` emits `jade_pkg_abi_version` into every package; the loader compares it with `jade_runtime::RUNTIME_ABI_VERSION` and falls back to a re-exported `jrt_abi_version` for packages published before that symbol existed. Neither present means the library does not link the Jade runtime at all — a C shim from `jade pkg add --c-abi` — which has no value ABI to disagree about and loads as before.

The check exists because the version was there and nobody read it. `RUNTIME_ABI_VERSION` went 1 → 2 when structs started crossing the boundary in v1.1.31, then 2 → 3 when bytes did in v1.2.2, and every published provider was built against the older number. The result was `native function returned an unknown value tag` raised from inside the call, naming neither the version nor the fix — on both engines, for every fresh install. `src/runtime_aot/native.c` carries the same check, and the two messages must stay in step.

**Any change to marshalling has to land in `runtime_aot/native.c` at the same time.** Bytes is what happens when it does not: v1.2.2 added the tag here, in `runtime.h`, and in `common.c`, but never in `native.c`. Under `jade run` blobs crossed fine; compiled, an argument arrived as `nil` and a return value crashed the process on a null dereference. It went unnoticed until v1.2.5 because nothing tested the tag on either side — so a new tag needs a test in `tests.rs` *and* a fixture the parity gate runs, not just an arm in each marshaller. The tag constants are duplicated in `runtime.h`.

`JADE_TAG_ERROR` is a string tag whose payload is an error message — a package signals failure by returning it, not by any out-of-band mechanism.

## Building and testing

```sh
cargo test native::
```

Build a Jade package to test against with `jade build lib.jde --lib`.
