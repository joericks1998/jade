# `src/native/`: the VM's C-ABI package loader

## What this subtree is

This is how the interpreter loads and calls a native shared library. When a `use` resolves to a `.dylib`, `.so`, or `.dll`, this module `dlopen`s it, calls its `jade_pkg_init` entry point to collect the name-to-function-pointer bindings, and marshals `VmValue` values across the C boundary in both directions.

## Why it was built this way

A single `.dylib` has to serve both engines. The AOT counterpart is `src/runtime_aot/native.c`, and it mirrors this file deliberately: the same registry shape, the same `jade_pkg_init` call, and the same marshalling. If the two drift, a package works under `jade run` and misbehaves when compiled, and the symptom is corrupted values rather than a clean error.

The ABI is narrow on purpose. Values cross as a tagged union of `nil`, `int`, `float`, `bool`, `str`, `error`, `array`, `dict`, `struct`, `bytes`, `handle`, `fn`, and `char`. Widening it further would mean a `libffi` dependency and two implementations of arbitrary-signature dispatch, one per engine.

`pkg/cshim.rs` exists precisely so a plain C library can be wrapped into this ABI instead. It is also what makes the `fn` tag possible without libffi, because a shim generated from a declared signature can simply *declare* a C function of that shape.

A `fn` value carries its own `invoke` pointer, rather than the package calling some agreed host symbol. That is the design's load-bearing decision, because the two engines re-enter Jade in completely different ways.

Compiled code calls a lowered function directly. *The VM cannot be re-entered from a C frame at all*, because calling a Jade function needs `VmState` and an async context, and during a native call the C library holds the stack. So the VM runs the call on a worker thread, and each callback posts its arguments back to the interpreter and waits. One agreed symbol would have suited neither engine.

That inversion sets the limit worth knowing. Callbacks are serviced only while the call that passed them is still in flight. A library that stores one and invokes it later finds nobody listening, and is told the call failed, rather than reaching an interpreter that has moved on.

Handle is the tag with the most reach per line of code. It is an opaque pointer: Jade holds it, hands it back, and never looks inside. That is enough to make an entire class of library bindable. SQLite, libsndfile, PCRE2, FreeType, libcurl, and libarchive are all organised around a `T*` the caller keeps between calls. Before this tag existed there was nowhere to put one, so it marshalled to `nil`, and none of them could be bound even in principle.

A handle carries its C type name for the same reason a struct does, and the payoff is sharper here. `handle<sqlite3>` and `handle<sqlite3_stmt>` look identical in structure, so without a name a binding could not refuse the wrong one. Passing a statement where a connection belongs is then a segfault inside SQLite, rather than anything Jade could report.

Bytes carries a length rather than relying on a terminator. That is the whole reason it is not a `str`. A blob may contain NUL bytes and need not be valid UTF-8, so a `char*` would cut one short and corrupt the other. Data arriving from a package is marked *tainted*, for the same reason a file read is: it came from outside the program.

A struct is the odd one out. It is a dict that also carries its *type name*, and that name is the point. A dict with the wrong keys reads as a set of nils and fails silently, so two programs sharing a dict share a convention. Two programs sharing a struct share a type the receiver can check.

The inference boundary is what drove that. `src/llm/provider_backend.rs` hands a provider package an `InferRequest` rather than an anonymous bag of keys, and reads back frames named `Token` or `Error`.

The name that crosses is the struct's *source* name. `aot/imports.rs` renames an imported module-global `Foo` to `Foo$2` while flattening imports, and that name is baked into the compiled library. So `abi_type_name` strips a trailing `$<digits>` on the way out, and `ffi_strdup_abi_type` in `runtime_aot/native.c` strips the same thing.

The number describes the importing program's module graph rather than the type, so it means nothing on the other side of the call. Without stripping it, a provider package built with `use ovata::infer` returns frames named `Token$0`, and the caller does not recognise its own protocol.

The one subtle rule is *who owns which buffer*. For an input argument, Jade owns the string buffer. For an output value, the native library owns it and must keep it valid until the native function returns, because Jade copies it immediately.

Array, dict, struct, and bytes payloads crossing the boundary are deep-copied into the *libc heap*. That way either `jade-runtime` instance in the process can free them, meaning this VM and each `dlopen`ed package, each of which has its own allocator pool. It is also why this file declares `malloc` and `free` directly rather than using Rust's allocator.

A top-level string is the one exception, handed over borrowed. A blob is copied at every level, including the top, so `ffi_free` has to reclaim it there too.

*Char was the last gap, and it was invisible until a struct field needed it.* `char` is a first-class Jade type, and `for c in "jade"` yields one. There was no tag for it, so it could not cross in any position. Nothing complained, because nothing tried: the FFI's own vocabulary mapped a scalar C `char` to `int`, which is exact.

It surfaced on a C `char[32]` field, where an array of characters wanted characters, and the symptom looked like an encoding problem. It was not one.

Trust rides in `_pad[0]`, because a char has no header of its own the way a string does. A char arriving *from* a package is tainted whatever the package claimed. `TRUSTED` is zero, so honoring the incoming bit would mark a char trusted simply because a package zeroed its struct.

*A Jade function given to a library outlives the call that gave it.* `CallbackBus` holds one channel per VM, every live `CallbackHost`, and a count of how many native calls are currently draining it.

That count replaces something the old design got for free. The channel used to close when the registering call ended, so a callback arriving late failed cleanly. A channel that lives as long as the VM never closes, so the count is what turns "nobody is listening" back into a neutral answer, rather than a worker blocked forever.

The receiver sits behind an async mutex rather than being owned by the in-flight call, because callbacks nest. A Jade callback may itself call a native function, and that call has to serve its own callbacks or it hangs on a receiver the outer loop is holding. A spawned task gets a fresh bus, so a cross-task callback is refused rather than run against another task's globals.

*A handle splits ownership, and that split is the whole subtlety of the tag.* Its wrapper and type name live on the libc heap and are released by `ffi_free`. The pointer inside never is. Jade cannot know what the pointee is or which allocator produced it, and a `sqlite3*` freed by anything other than `sqlite3_close` corrupts the library. So closing is an explicit call the binding exposes, and the honest consequence is that a handle dropped without it leaks whatever the C library allocated.

There is a second consequence, and it lives in `compiler/type_infer.rs` rather than here: a handle cannot be passed into a spawned function. `taskcheck` watches `SetIndex`, `SetField`, and mutating methods, and a handle triggers none of them, because the mutation happens entirely inside the library. So two tasks sharing one connection would race with no diagnostic at all. Jade cannot tell a thread-safe library from an unsafe one, so it refuses.

## What each file does

- *`mod.rs`* holds the tag constants from `JADE_TAG_NIL` through `JADE_TAG_HANDLE`, the `JadeVal` repr-C union, `load_native_package`, and the `vm_to_ffi` and `ffi_to_vm` marshalling in both directions. That includes the `JadeArr`, `JadeMap`, `JadeStruct`, `JadeBytes`, and `JadeHandle` transport trees.
- *`tests.rs`* holds the loader and marshalling tests.

## Who uses it

*Depends on:* `libloading` for `dlopen`, `vm::VmValue`, `builtins::make_array`, and `jade_runtime::coll::DictObj`.

*Used by:* `vm/chunk.rs` when a `use` resolves to a native library, and `llm/provider_backend.rs` to load a provider package. Its mirror image is `src/runtime_aot/native.c`.

## Gotchas

*A package is never unloaded, and removing that rule would look like a safe cleanup.* `_lib` on `NativeLibFn` keeps the image mapped while any of its functions are alive. That is the right rule for a call and the wrong one for a process.

When the last binding dropped at shutdown, `dlclose` unmapped the library. Meanwhile a thread that had not finished exiting still had that library's thread-local destructors queued against it. glibc runs those from `__nptl_deallocate_tsd` as a thread winds down, and by then it was jumping into an address that was no longer mapped. So a program printed every one of its answers and *then* took a SIGSEGV.

glib registers such a destructor, which is how the FFI gate found the bug. It appeared on Linux only, under the VM only, and for four releases the gate reported the crash as a pass, because it never checked an exit status.

`LOADED_LIBS` now holds a clone of every handle until the process exits. `runtime_aot/native.c` never had the bug, because it never unloads and contains no `dlclose` at all. So this is the VM adopting the rule the other engine already followed, rather than a workaround for one library's destructor.

Nothing is lost by it. Jade has no API to unload a package, so an image released here could not be re-loaded by anything, and the process is ending regardless. `RTLD_NODELETE` at open would tell the loader the same thing, but `libloading` does not export it, and its value differs by platform.


*A package declares the value ABI it was built against, and an incompatible one is refused at load.* `jade build --lib` emits `jade_pkg_abi_version` into every package. The loader compares that with `jade_runtime::RUNTIME_ABI_VERSION`, and falls back to a re-exported `jrt_abi_version` for packages published before that symbol existed.

If neither symbol is present, the library does not link the Jade runtime at all. That describes a C shim from `jade pkg add --c-abi`, which has no value ABI to disagree about and loads exactly as before.

The check exists because the version was already there and nobody read it. `RUNTIME_ABI_VERSION` went from 1 to 2 when structs started crossing the boundary in v1.1.31, from 2 to 3 when bytes did in v1.2.2, from 3 to 4 when handles did in v1.3.0, and from 4 to 5 when chars did in v1.3.10. Every published provider was built against an older number.

The result was `native function returned an unknown value tag`, raised from inside the call and naming neither the version nor the fix. It happened on both engines, for every fresh install. `src/runtime_aot/native.c` carries the same check, and the two messages must stay in step.

*A failure message has to land in `runtime_aot/native.c` too, not just the check that reports it.* `load_native_package` here has always included the loader's own reason for a failed `dlopen`. The compiled runtime printed the path and nothing else. So `jade run` said "slice is not valid mach-o file", while the compiled binary said only that it could not load.

The check was not what differed. The explanation was, and a user who happened to build rather than run got neither the cause nor a hint of one.

*Any change to marshalling has to land in `runtime_aot/native.c` at the same time.* Bytes shows what happens when it does not. v1.2.2 added the tag here, in `runtime.h`, and in `common.c`, but never in `native.c`. Under `jade run`, blobs crossed fine. When compiled, an argument arrived as `nil` and a return value crashed the process on a null dereference.

It went unnoticed until v1.2.5, because nothing tested the tag on either side. So a new tag needs a test in `tests.rs` *and* a fixture the parity gate runs, not just an arm in each marshaller. The tag constants are duplicated in `runtime.h`.

The handle tag was added with both, and the fixture earned its keep on the first run. It lives in `src/scripts/handle-fixture.c` rather than in `examples/`, because only a native C package can produce a handle, and no `.jde` fixture can reach the tag.

`JADE_TAG_ERROR` is a string tag whose payload is an error message. A package signals failure by returning one, rather than through any separate channel.

## Building and testing

```sh
cargo test native::
```

Build a Jade package to test against with `jade build lib.jde --lib`.
