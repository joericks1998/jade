# `src/runtime_aot/` — the C runtime linked into compiled binaries

## What this subtree is

C, not Rust. This is the runtime that `jade build`-produced binaries link against, compiled into `libJadeRuntime.a` and dropped in `$OUT_DIR` (surfaced to the linker through the `JADE_RT_LIB_DIR` compile-time env).

The Cargo build script that does the compiling lives here too, as `build.rs`, pointed at by `build = "src/runtime_aot/build.rs"` in the root `Cargo.toml`. It sits beside the C it compiles rather than at the repo root, and it cannot go to `src/build.rs` because `src/build/` already claims that module name. Cargo runs a build script with the working directory set to the package root whatever its path, so the `src/runtime_aot/...` paths inside it are unaffected by living here.

Do not confuse it with `src/runtime/` (`jade-runtime`), which is Rust and shared by *both* engines. This directory holds only what is specific to a compiled binary: the exception handler, the platform and concurrency shim, native package loading, and the inference request path.

Because it is C, it has no module declaration in `src/lib.rs`.

## Why it is split the way it is

The runtime is a platform-agnostic core plus a swappable platform backend. `common.c` is shared verbatim; the backend supplies only two things — the concurrency layer (`jade_spawn`/`await`/`join`) and the process-exit primitive `jade_rt_exit`. `posix.c` is the host backend for macOS and Linux; another target could supply its own without touching the core.

The trend over time has been *shrinking*. Symbols keep moving out of `common.c` into `jade-runtime` so both engines share one implementation, with the C declaration left behind in `runtime.h` and the linker resolving it against the Rust staticlib. `ipc/` went further than that and disappeared: it held the socket transport to the inference daemon, which moved to Rust and then, in v1.1.30, was removed outright when inference became an in-process call into a provider package.

## What each file does

- **`runtime.h`** — the ABI. Defines `jade_value_t` and the tagged value layout (bit 0 clear is an int; low 3 bits `001` a heap pointer, `011` a boxed float, `101` a string), plus every `jrt_*` and `jade_*` declaration. This file and `jade-runtime`'s `value.rs` are byte-identical mirrors of each other; changing one means changing both.
- **`common.c`** — the platform-agnostic core: fatal errors, the `setjmp`-based exception machinery, and whatever value operations have not yet migrated to Rust.
- **`posix.c`** — the host backend. pthreads-based concurrency, `dlopen`/`dlsym`, and `exit`. Guarded by `#ifndef __JADE_KERNEL__`.
- **`native.c`** — native (C-ABI) package support: the registry, `jade_pkg_init` invocation, and value marshalling. It mirrors `src/native/mod.rs`'s `load_native_package` / `vm_to_ffi` / `ffi_to_vm` so one `.dylib` serves both `jade run` and `jade build`. The `dlopen` primitives themselves are backend hooks.
- **`infer/infer.c`**, **`infer/infer.h`** — every `jrt_prompt_*` entry point. Each builds an `InferRequest` and drives the installed provider package through `native.c`, so this file has no transport of its own.
- **`build.rs`** — the Cargo build script. Compiles every `.c` here into `libJadeRuntime.a`, surfaces the output directory to the linker, and copies the archive up beside the Rust one so both sit in a single predictable place. It also enforces the Unix-only constraint before `cc` runs.

## Who uses it

*Depends on:* `jade-runtime` (Rust) for everything declared but not defined here.

*Used by:* `build.rs` (in this directory) compiles it; `src/aot/` emits calls into it and links the archive into every binary `jade build` produces. `src/native/mod.rs` is its Rust counterpart and must stay in step with `native.c`.

## Gotchas

Marshalling in `native.c` and `src/native/mod.rs` must agree, or a package that works under `jade run` will misbehave when compiled — and the failure shows up as corrupted values, not a clean error.

The transport tree for arrays and dicts crossing the FFI is **libc-heap**, deliberately, so either `jade-runtime` instance in the process can free it. See the `JadeArr`/`JadeMap` notes in `runtime.h`.

Any change to the tagged value layout has three homes: `runtime.h`, `jade-runtime`'s `value.rs`, and the tag arithmetic in `aot/lower.rs`.

## Building

`build.rs` compiles this automatically as part of `cargo build`. It also enforces the Unix-only constraint first, before `cc` runs, so a Windows target fails with a clear message rather than a missing-POSIX-header error.

Because the build script watches this directory (`cargo:rerun-if-changed=src/runtime_aot`), editing it — including editing `build.rs` itself — reruns the C build.
