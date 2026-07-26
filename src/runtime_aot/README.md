# `src/runtime_aot/` — the C runtime linked into compiled binaries

## What this subtree is

C, not Rust. This is the runtime that `jade build`-produced binaries link against, compiled by the crate's `build.rs` into `libJadeRuntime.a` and dropped in `$OUT_DIR` (surfaced to the linker through the `JADE_RT_LIB_DIR` compile-time env).

Do not confuse it with `src/runtime/` (`jade-runtime`), which is Rust and shared by *both* engines. This directory holds only what is specific to a compiled binary: the exception handler, the platform and concurrency shim, native package loading, and the inference request path.

Because it is C, it has no module declaration in `src/lib.rs`.

## Why it is split the way it is

The runtime is a platform-agnostic core plus a swappable platform backend. `common.c` is shared verbatim; the backend supplies only two things — the concurrency layer (`jade_spawn`/`await`/`join`) and the process-exit primitive `jade_rt_exit`. `posix.c` is the host backend for macOS and Linux; another target could supply its own without touching the core.

The trend over time has been *shrinking*. Symbols keep moving out of `common.c` into `jade-runtime` so both engines share one implementation, with the C declaration left behind in `runtime.h` and the linker resolving it against the Rust staticlib. `ipc/` is the clearest example: the whole socket transport is now Rust, and only the header declaring the ABI remains.

## What each file does

- **`runtime.h`** — the ABI. Defines `jade_value_t` and the tagged value layout (bit 0 clear is an int; low 3 bits `001` a heap pointer, `011` a boxed float, `101` a string), plus every `jrt_*` and `jade_*` declaration. This file and `jade-runtime`'s `value.rs` are byte-identical mirrors of each other; changing one means changing both.
- **`common.c`** — the platform-agnostic core: fatal errors, the `setjmp`-based exception machinery, and whatever value operations have not yet migrated to Rust.
- **`posix.c`** — the host backend. pthreads-based concurrency, `dlopen`/`dlsym`, and `exit`. Guarded by `#ifndef __JADE_KERNEL__`.
- **`native.c`** — native (C-ABI) package support: the registry, `jade_pkg_init` invocation, and value marshalling. It mirrors `src/native/mod.rs`'s `load_native_package` / `vm_to_ffi` / `ffi_to_vm` so one `.dylib` serves both `jade run` and `jade build`. The `dlopen` primitives themselves are backend hooks.
- **`infer/infer.c`**, **`infer/infer.h`** — the structured JSON inference request builder and every `jrt_prompt_*` entry point. Dispatches through `ipc` for transport and never touches a socket directly.
- **`ipc/ipc.h`** — declares the persistent-connection ABI. **There is no `ipc.c`.** The entry points are implemented in Rust, in `jade-runtime`'s `infer` module.

## Who uses it

*Depends on:* `jade-runtime` (Rust) for everything declared but not defined here.

*Used by:* `build.rs` compiles it; `src/aot/` emits calls into it and links the archive into every binary `jade build` produces. `src/native/mod.rs` is its Rust counterpart and must stay in step with `native.c`.

## Gotchas

Marshalling in `native.c` and `src/native/mod.rs` must agree, or a package that works under `jade run` will misbehave when compiled — and the failure shows up as corrupted values, not a clean error.

The transport tree for arrays and dicts crossing the FFI is **libc-heap**, deliberately, so either `jade-runtime` instance in the process can free it. See the `JadeArr`/`JadeMap` notes in `runtime.h`.

Any change to the tagged value layout has three homes: `runtime.h`, `jade-runtime`'s `value.rs`, and the tag arithmetic in `aot/lower.rs`.

## Building

`build.rs` compiles this automatically as part of `cargo build`. It also enforces the Unix-only constraint first, before `cc` runs, so a Windows target fails with a clear message rather than a missing-POSIX-header error.
