# `src/runtime_aot/`: the C runtime linked into compiled binaries

## What this subtree is

This directory is C, not Rust. It is the runtime that binaries from `jade build` link against. It compiles into `libJadeRuntime.a`, which lands in `$OUT_DIR` and reaches the linker through the compile-time `JADE_RT_LIB_DIR` variable.

The Cargo build script that does the compiling lives here too, as `build.rs`. The root `Cargo.toml` points at it with `build = "src/runtime_aot/build.rs"`. It sits beside the C it compiles rather than at the repo root, and it cannot live at `src/build.rs`, because `src/build/` already claims that module name. Cargo runs a build script with its working directory set to the package root whatever the script's own path, so the `src/runtime_aot/...` paths inside it are unaffected by living here.

Do not confuse this with `src/runtime/`, the `jade-runtime` crate, which is Rust and shared by *both* engines. This directory holds only what is specific to a compiled binary: the exception handler, the platform and concurrency shim, native package loading, and the inference request path.

Because it is C, it has no module declaration in `src/lib.rs`.

## Why it is split the way it is

The runtime is a platform-independent core plus a swappable platform backend. `common.c` is shared unchanged. The backend supplies only two things: the concurrency layer, meaning `jade_spawn`, `jade_await`, and `jade_join`, and the process-exit primitive `jade_rt_exit`. `posix.c` is the host backend for macOS and Linux. Another target could supply its own without touching the core.

The trend over time has been *shrinking*. Symbols keep moving out of `common.c` into `jade-runtime`, so both engines share one implementation. The C declaration stays behind in `runtime.h`, and the linker resolves it against the Rust static library.

`ipc/` went further and disappeared entirely. It held the socket transport to the inference daemon, which moved to Rust and then, in v1.1.30, was removed outright once inference became an in-process call into a provider package.

## What each file does

- *`runtime.h`* defines the ABI. It holds `jade_value_t` and the tagged value layout, plus every `jrt_*` and `jade_*` declaration. In that layout, bit 0 clear means an int, and the low three bits `001` mean a heap pointer, `011` a boxed float, and `101` a string. This file and `value.rs` in `jade-runtime` are byte-identical mirrors of each other, so changing one means changing both.
- *`common.c`* holds the platform-independent core: fatal errors, the `setjmp`-based exception machinery, and whatever value operations have not yet moved to Rust.
- *`posix.c`* is the host backend. It holds pthreads-based concurrency, `dlopen` and `dlsym`, and `exit`. A `#ifndef __JADE_KERNEL__` guards it.

  It defines `_GNU_SOURCE` before any header, because `jade_image_dir` uses `dladdr`. macOS declares `dladdr` unconditionally, while glibc hides it. Without the define, the file built on every developer's Mac and failed only in CI, so three releases were merged and never shipped. Anything else GNU-only that lands here is covered by the same define, but the ordering is fragile: it has to come before `runtime.h`.

*Path buffers are `PATH_MAX`, and that is not a style preference.* On glibc, `realpath` writes up to `PATH_MAX` bytes into whatever buffer it is given, and a fortified build aborts the process when the buffer is smaller. That happens no matter how long the path actually is.

macOS sets `PATH_MAX` to 1024 and Linux sets it to 4096. So a hard-coded 1024 was exactly right on one platform and an instant abort on the other. Every FFI package in a compiled binary died at startup on Linux until v1.3.13. `runtime.h` now defines the constant once, with a 4096 fallback.

Two properties of this tree let that bug sit unnoticed, and both still hold. The fortify checks only exist in optimised builds, so a debug toolchain never runs them, which means `cargo test` and the parity gate will not catch the next one. And `build.rs` compiles with `.warnings(false)`, which suppressed the compile-time warning glibc emits saying exactly what was wrong.

So sweeping the tree by hand is worth doing whenever anything here touches a fixed buffer:

```sh
docker run --rm -v "$PWD:/w" -w /w gcc:13 sh -c \
  'for f in src/runtime_aot/*.c src/runtime_aot/infer/*.c; do
     gcc -O2 -D_FORTIFY_SOURCE=3 -Wall -c -I src/runtime_aot -I src/runtime_aot/infer \
       -o /tmp/o.o "$f"; done'
```

- *`native.c`* supports native C-ABI packages: the registry, calling `jade_pkg_init`, and value marshalling. It mirrors `load_native_package`, `vm_to_ffi`, and `ffi_to_vm` in `src/native/mod.rs`, so one `.dylib` serves both `jade run` and `jade build`. The `dlopen` primitives themselves are backend hooks.
- *`infer/infer.c`* and *`infer/infer.h`* hold every `jrt_prompt_*` entry point. Each one builds an `InferRequest` and drives the installed provider package through `native.c`, so this file carries no transport of its own.
- *`build.rs`* is the Cargo build script. It compiles every `.c` file here into `libJadeRuntime.a`, exposes the output directory to the linker, and copies the archive up beside the Rust one so both sit in one predictable place. It also enforces the Unix-only rule before `cc` runs.

## Who uses it

*Depends on:* `jade-runtime` (Rust) for everything declared but not defined here.

*Used by:* the `build.rs` in this directory, which compiles it, and `src/aot/`, which emits calls into it and links the archive into every binary `jade build` produces. `src/native/mod.rs` is its Rust counterpart and has to stay in step with `native.c`.

## One libraries directory per process

A dependency has to be loaded once, not once for each package that uses it. That is why `jrt_native_load_rel` exists alongside the older `jrt_native_load`.

`dlopen` keys a loaded image by the path it was asked for. So two images that resolve the same dependency to two different paths get two independent instances, each with its own globals and its own initializer. For an ordinary library that is wasted memory. For one that owns a device or a graphics context, it is two devices, and the failure lands in the operating system rather than in Jade.

So the root is chosen *once*, by whoever hosts the process, which is either a compiled binary's `main` or the CLI, and then published. Every load resolves `<root>/<key>` and nothing else. It is deliberately *not* a search chain. With a chain, two images can land on different steps and therefore on different copies, and nothing can observe that it happened.

*The channel is the environment, and that is forced rather than chosen.* Every image carries its own statically linked copy of this runtime, so no C or Rust global crosses a `dlopen` boundary. The environment is held by libc, which is shared, and it is the only channel available.

`jrt_libs_root_publish` writes `JADE_LIBS` with `overwrite = 0`, so a root the user set is never replaced. That is what lets a process with no Jade host in it, such as a C program embedding a package, still have one agreed root.

`jrt_native_check_one` records each dependency's resolved path under `JADE_PKG_<name>`, and raises if a second, different path appears for the same name. Keying on the resolved path rather than on the version is stronger than a version check, because it also catches two roots, a stray copy, and a symlink that escaped canonicalization.

`jade_realpath` matters for the same reason, and is not cosmetic. Two spellings of one file mean two instances.

## Gotchas

Marshalling in `native.c` and in `src/native/mod.rs` must agree. Otherwise a package that works under `jade run` misbehaves when compiled, and the failure shows up as corrupted values rather than a clean error.

That is not hypothetical. v1.2.2 added the `bytes` tag to `runtime.h`, `common.c`, and the VM's marshaller, but not to `native.c`. For three releases, a blob argument silently became `nil` when compiled, and a blob return value crashed the process.

Adding a tag means four arms here: outbound, inbound, `ffi_free_node`, and the `jade_ffi_free` gate. The gate is the easy one to miss, because forgetting it leaks rather than fails.

*Read the result before releasing the argument trees.* `jrt_native_call` used to free the arguments as soon as the call returned, and only then convert `out`. A native function is allowed to return a pointer *into* one of its arguments, such as a `tag_of(h)` handing back the handle's type name, and anything of that shape. So the conversion was reading freed memory, and the compiled binary printed an empty string where the interpreter printed the right one. The VM had always had the order right.

The error path needs the same care, plus one thing more. `native_raise` does a `longjmp`, so the message is copied into a local buffer, the trees are released, and only then does it raise. Raising first would leak every argument tree.

The transport tree for arrays, dicts, structs, bytes, and handles crossing the FFI lives on the *libc heap*, deliberately, so either `jade-runtime` instance in the process can free it. See the `JadeArr`, `JadeMap`, `JadeBytes`, and `JadeHandle` notes in `runtime.h`.

*One place knows how to call a value.* `jrt_call_value` is it. A plain function is entered directly, a bound method has its receiver put in front first, and a native binding goes through `jrt_native_call` — and every caller, from an ordinary indirect call to `array.map` to a callback a C library invokes, goes through it rather than reading a code pointer out of the box itself.

That was three separate copies of the dispatch before, and one of them, `aot_invoke_callback`, had a six-argument ladder and no bound-method case at all. It is also what lets the callee's own entry own arity: `jrt_call_value` passes `(argc, argv)` and the entry checks the count and fills the defaults, because the call site cannot.

*A guard belongs here, not at the call site, when it has to raise.* A raise is a `longjmp` and must not unwind through a Rust frame, so the checks codegen emits before it dereferences a word all live on this side: `jrt_require_kind` for a primitive method's receiver, `jrt_require_str_arg` for a string method's argument, and `jrt_require_str_val`, `jrt_require_float_val`, `jrt_require_dict_key`, `jrt_require_callable` and `jrt_require_struct` for the places a static type is about to be trusted with a pointer.

Each one exists because its absence was a crash. `{"a": 1}.slice(0, 1)` read a `DictObj` as a `BytesObj`; `{true: "y"}` used a boolean as a `char*`; `fn go(f) { f(1) }; go(5)` loaded a function pointer out of the integer 5. The interpreter checks the value it has and raises, so these have to as well, and with the same wording — `jrt_require_kind` names both a key and a method for a dict, because a dict reads `d.name` as a lookup before it looks for a method and the VM says so.

*A method call resolves against the receiver it has.* `jrt_struct_is_type` answers whether the receiver is the type a devirtualized call site assumed, and `jrt_method_fallback` handles it when it is not. The fallback goes through `jrt_get_field` rather than straight to the method registry, because that function already implements the interpreter's order — a data field holding a function beats a method of the same name — and then hands the result to `jrt_call_value`. `jrt_method_resolve_or_raise` is the registry lookup with the interpreter's wording on a miss; the Rust half reports failure through a status out-param, since it cannot raise.

*A binary runs its body on a thread of its own.* `jrt_run_main` in `posix.c` gives it 256 MB of stack, matching what the CLI gives the interpreter. On the process default a program that printed fine under `jade run` segfaulted compiled — 2,000 levels of nested array was enough, because rendering one walks it. Every piece of per-execution state the runtime keeps is already thread-local, since async tasks have always run elsewhere, so nothing had to move. State that is a fact about the *program* rather than the execution must therefore not be thread-local: `jrt_libs_root_publish`'s bookkeeping was, and the answer to "where did this root come from" vanished the moment the body ran on another thread.

*The core builtins have values here.* `jrt_builtin_value` hands back a static `{entry, kind, name}` box for `len`, `str`, `print` and the rest, so `let f = len` and `xs.map(str)` work; the name used to read as an empty global. Each entry is where that builtin's own argument check lives — `print` takes one argument or two, `len` refuses a value with no length through `jade_len`. The byte above the ObjKind in the box says which sort of callable it is, which is how the renderer prints `<builtin len>` and `<type str>` rather than `<object>`.

*A callback must not let a raise escape.* A Jade `raise` is a `longjmp`. One leaving `aot_invoke_callback` would unwind through the C library's own frames, past whatever it was in the middle of, and leave its state in whatever condition it happened to be.

So the callback runs inside a `setjmp` frame and reports failure through its return value. `jrt_uhttp_stream` follows the same rule, and it is why the shim delays a callback's error until the library has returned normally.

A handle is the one payload whose ownership splits. `jade_ffi_free` releases its wrapper and its type name, and never its `ptr`, which belongs to the library that issued it. Freeing the pointer here would hand the C library's memory back through the wrong allocator.

*Nothing in this directory formats a value for display any more.* `jrt_snprintf_float` was the last holdout, and it is gone. It used `"%.*g"`, which switches to exponent form exactly when a float needs trailing zeros before the decimal point. So a compiled binary printed `1e+01` for `10.0` while the VM printed `10.0`.

Value text comes from `jrt_render_any` in `jade-runtime`. A second implementation here would drift the same way. Note also that float and string text have no upper bound in length, since `1e300` is 301 digits, so neither may be formatted into a fixed scratch buffer.

Any change to the tagged value layout has three homes: `runtime.h`, `jade-runtime`'s `value.rs`, and the tag arithmetic in `src/codegen/`.

The exception stack, meaning `exc_stack` and `exc_depth` in `common.c`, is `_Thread_local` and *grows*, and *nothing unwinds it automatically*. A `longjmp` needs its `jmp_buf` to live in a stack frame that has not returned, so codegen scopes the depth: `jade_exc_depth` in a function's prologue, and `jade_exc_restore` on each of its return paths. `jade_exc_restore` only ever lowers the depth, because raising it would resurrect a buffer whose frame is gone. If you add a path out of a lowered function, it needs the restore too.

It was a fixed 64-slot array until v1.4.2, which is not many: a recursive function with a `try` in it ran out at depth 64 and reported "exception stack overflow" for a program the interpreter ran fine, since the interpreter's handlers are a vec owned by the dispatch call frame and have no ceiling. The first 64 frames still live in a thread-local array so the common case allocates nothing; past that it doubles on the heap, up to a backstop far above any real program.

*A task body must not inherit the thread it happens to run on.* `jade_task_invoke` in `posix.c` saves the handler depth and swaps in a fresh recursion budget around the body, and hands both back on the way out. That used to be free: each task got a thread of its own, so there was nothing to inherit. It stopped being free when a bounded pool started reusing workers, and it stopped being optional when an awaiting thread began running tasks inline rather than parking on them — the thread underneath a task body can now be one in the middle of its own generator, its own `try`, and its own deep call chain. See *An awaiting thread runs the task* in `src/runtime/README.md`.

*A raise has to produce the value the VM would produce, not just the right text.* Every non-user error in the interpreter is a `RuntimeError` struct with a `message` field, built in `vm/exceptions.rs`. So `catch e` binds a struct, and `catch RuntimeError e` matches.

Raising the bare message string here meant the same `try` saw a str when compiled and a struct when interpreted. `e.message` raised, and a typed catch quietly never fired.

`throw_msg` builds the struct now, and everything that raises goes through it. `jrt_throw_io` adds the interpreter's `I/O error: ` prefix for the fs, http, uhttp, and sh forwarders. `jrt_throw_runtime` is the entry point for codegen's own failures, such as a zero divisor or an overflow. A user's `raise x` deliberately does *not* pass through either, and throws the value written, exactly as the VM does. The `[line:col]` prefix is the one part that stays absent, because compiled code has no source position at run time.

`jrt_require_kind` and `jrt_require_str_arg` exist because a primitive method's *name* does not establish its receiver's kind. The compiled path calls them before untagging a receiver whose type it could not determine ahead of time. They raise through `throw_msg`, deliberately, so a Jade `catch` can see the failure. The common way to test a type is a method call wrapped in `try`.

## Building

`build.rs` compiles this directory as part of `cargo build`. It enforces the Unix-only rule first, before `cc` runs, so a Windows target fails with a clear message rather than a missing-POSIX-header error.

The build script watches this directory, through `cargo:rerun-if-changed=src/runtime_aot`. So editing anything here reruns the C build, including editing `build.rs` itself.
