# `src/native/` — the VM's C-ABI package loader

## What this subtree is

How the interpreter loads and calls a native shared library. When a `use` resolves to a `.dylib` / `.so` / `.dll`, this module `dlopen`s it, calls its `jade_pkg_init` entry point to collect the name-to-function-pointer bindings, and marshals `VmValue`s across the C boundary in both directions.

## Why it was built this way

A single `.dylib` has to serve both engines. The AOT counterpart is `src/runtime_aot/native.c`, and it mirrors this file deliberately: same registry shape, same `jade_pkg_init` invocation, same marshalling. If the two drift, a package works under `jade run` and misbehaves when compiled, and the symptom is corrupted values rather than a clean error.

The ABI is narrow on purpose. Values cross as a tagged union of `nil`, `int`, `float`, `bool`, `str`, `error`, `array`, `dict`, `struct`, `bytes`, `handle`, and `fn`. Widening it further would mean a `libffi` dependency and two implementations of arbitrary-signature dispatch, one per engine. `pkg/cshim.rs` exists precisely so a plain C library can be wrapped into this ABI instead — and it is what makes the `fn` tag possible without libffi, since a shim generated from a declared signature can simply *declare* a C function of that shape.

A `fn` value carries its own `invoke` pointer rather than the package calling some agreed host symbol, and that is the design's load-bearing decision. The two engines re-enter in completely different ways: compiled code calls a lowered function directly, while **the VM cannot be re-entered from a C frame at all** — calling a Jade function needs `VmState` and an async context, and during a native call the C library holds the stack. So the VM runs the call on a worker thread and each callback posts its arguments back to the interpreter and waits. One agreed symbol would have suited neither engine.

That inversion sets the limit worth knowing: callbacks are serviced only while the call that passed them is in flight. A library that stores one and invokes it later finds nobody listening and is told the call failed, rather than reaching an interpreter that has moved on.

Handle is the newest tag and the one with the most reach per line of code. It is an opaque pointer: Jade holds it, hands it back, and never looks inside. That is enough to make an entire class of library bindable — SQLite, libsndfile, PCRE2, FreeType, libcurl, libarchive are all organised around a `T*` the caller keeps between calls, and before this tag existed there was nowhere to put one, so it marshalled to `nil` and none of them could be bound even in principle.

A handle carries its C type name for the same reason a struct does, and the payoff is sharper: `handle<sqlite3>` and `handle<sqlite3_stmt>` are structurally identical, so without a name a binding could not refuse the wrong one, and passing a statement where a connection belongs is a segfault inside SQLite rather than anything Jade could report.

Bytes is the newest tag, and it carries a length rather than relying on a terminator. That is the whole reason it is not a `str`: a blob may contain NUL bytes and need not be valid UTF-8, so a `char*` would truncate one and corrupt the other. Data arriving from a package is marked **tainted**, for the reason a file read is — it came from outside the program.

A struct is the odd one out: it is a dict that also carries its **type name**. That name is the point. A dict with the wrong keys reads as a set of nils and fails silently, so two programs sharing a dict share a convention; two programs sharing a struct share a type the receiver can check. The inference boundary is what drove it — `src/llm/provider_backend.rs` hands a provider package an `InferRequest` rather than an anonymous bag of keys, and reads back frames named `Token` or `Error`.

The name that crosses is the struct's **source** name. `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2` while flattening imports, and that name is baked into the compiled library — so `abi_type_name` strips a trailing `$<digits>` on the way out, and `runtime_aot/native.c`'s `ffi_strdup_abi_type` strips the same thing. The number describes the importing program's module graph, not the type, so it means nothing on the other side of the call. Without stripping it, a provider package built with `use ovata::infer` returns frames named `Token$0` and the caller does not recognise its own protocol.

The one subtle rule is **who owns which buffer**. For input arguments, Jade owns the string buffer. For output values, the native library owns it and must keep it valid through the return of the native function — Jade copies immediately. Array, dict, struct and bytes payloads crossing the boundary are deep-copied into the **libc heap**, so that either `jade-runtime` instance in the process (this VM, and each dlopen'd package, each with its own allocator pool) can free them. That is the reason this file declares `malloc` and `free` directly rather than using Rust's allocator. A top-level string is the one exception, handed over borrowed; a blob is copied at every level, top included, so `ffi_free` has to reclaim it there too.

**A handle splits ownership, and that split is the whole subtlety of the tag.** Its wrapper and type name are libc heap released by `ffi_free`; the pointer inside is not, ever. Jade cannot know what the pointee is or which allocator produced it, and a `sqlite3*` freed by anything but `sqlite3_close` corrupts the library. So closing is an explicit call the binding exposes, and the honest consequence is that a handle dropped without it leaks whatever the C library allocated.

There is a second consequence, in `compiler/type_infer.rs` rather than here: a handle cannot be passed into a spawned function. `taskcheck` watches `SetIndex`/`SetField`/mutating methods and a handle has none — the mutation is entirely inside the library — so two tasks sharing one connection would race with no diagnostic at all. Jade cannot tell a thread-safe library from an unsafe one, so it refuses.

## What each file does

- **`mod.rs`** — the tag constants (`JADE_TAG_NIL` … `JADE_TAG_HANDLE`), the `JadeVal` repr-C union, `load_native_package`, and the `vm_to_ffi` / `ffi_to_vm` marshalling in both directions including the `JadeArr` / `JadeMap` / `JadeStruct` / `JadeBytes` / `JadeHandle` transport trees.
- **`tests.rs`** — loader and marshalling tests.

## Who uses it

*Depends on:* `libloading` for `dlopen`, `vm::VmValue`, `builtins::make_array`, and `jade_runtime::coll::DictObj`.

*Used by:* `vm/chunk.rs` when a `use` resolves to a native library, and `llm/provider_backend.rs` to load a provider package. Its mirror image is `src/runtime_aot/native.c`.

## Gotchas

**A package declares the value ABI it was built against, and an incompatible one is refused at load.** `jade build --lib` emits `jade_pkg_abi_version` into every package; the loader compares it with `jade_runtime::RUNTIME_ABI_VERSION` and falls back to a re-exported `jrt_abi_version` for packages published before that symbol existed. Neither present means the library does not link the Jade runtime at all — a C shim from `jade pkg add --c-abi` — which has no value ABI to disagree about and loads as before.

The check exists because the version was there and nobody read it. `RUNTIME_ABI_VERSION` went 1 → 2 when structs started crossing the boundary in v1.1.31, then 2 → 3 when bytes did in v1.2.2, then 3 → 4 when handles did in v1.3.0, and every published provider was built against the older number. The result was `native function returned an unknown value tag` raised from inside the call, naming neither the version nor the fix — on both engines, for every fresh install. `src/runtime_aot/native.c` carries the same check, and the two messages must stay in step.

**Any change to marshalling has to land in `runtime_aot/native.c` at the same time.** Bytes is what happens when it does not: v1.2.2 added the tag here, in `runtime.h`, and in `common.c`, but never in `native.c`. Under `jade run` blobs crossed fine; compiled, an argument arrived as `nil` and a return value crashed the process on a null dereference. It went unnoticed until v1.2.5 because nothing tested the tag on either side — so a new tag needs a test in `tests.rs` *and* a fixture the parity gate runs, not just an arm in each marshaller. The tag constants are duplicated in `runtime.h`.

The handle tag was added with both, and the fixture earned its keep on the first run. It lives in `src/scripts/handle-fixture.c` rather than `examples/`, because only a native C package can produce a handle and no `.jde` fixture can reach the tag.

`JADE_TAG_ERROR` is a string tag whose payload is an error message — a package signals failure by returning it, not by any out-of-band mechanism.

## Building and testing

```sh
cargo test native::
```

Build a Jade package to test against with `jade build lib.jde --lib`.
