# `src/runtime/` — `jade-runtime`, the shared runtime crate

## What this subtree is

A separate Rust crate holding Jade's *value semantics* in one place, so the two execution engines cannot drift.

- The bytecode **VM** (`jade run`) links it as an `rlib` and calls it natively.
- **AOT-compiled binaries** (`jade build`) link it as a C-ABI `staticlib`, resolving the `#[no_mangle] extern "C"` `jrt_*` symbols the codegen emits.

It is a workspace member, not merely a path dependency, and the root `Cargo.toml` explains why in detail: Cargo only uplifts `libjade_runtime.a` to `target/<profile>/` when the crate is a requested top-level target, and the linker line `jade build` emits looks for it exactly there. Being a member also means `cargo test` runs its ~70 tests.

## Why it was built

Historically the VM (Rust, `VmValue` + `Arc`) and the AOT backend (C, `jrt_*_any`) were two independent implementations of the same language, and every divergence between them was a bug reconciled after the fact. A float that printed `3` in one and `3.0` in the other; a dict whose keys sorted differently; integer overflow that errored in the VM and wrapped in compiled code. This crate is the structural fix: shared code, one behavior.

It is intentionally dependency-free and LLVM-free, so it builds everywhere `jade run` runs.

## What each file does

**Value representation and memory**

- `value.rs` — the tagged 64-bit value ABI (`JadeValue`), byte-identical to `runtime_aot/runtime.h`. Pure bit-twiddling, no allocation.
- `heap.rs` — `ObjHeader`, the unified header on every reference type: refcount plus cycle-collector color and flags.
- `gc.rs` — heap accounting. `leak_obj`/`free_obj` keep a live-object counter, because no golden-output difftest can observe a leak *or* a premature free — this makes the heap population measurable so the collector can be verified.
- `pool.rs` — the segregated free-list allocator both engines use. These are ordinary functions, **never a `#[global_allocator]`**; that invariant is what keeps them safe in a process that dlopen's a native package.
- `arena.rs` — the per-frame bump arena for collections the compiler proved do not escape. Paired with `compiler/escape.rs`.
- `sys.rs` — raw `malloc`/`free` bindings, so heap objects are interchangeable with the C runtime.

**Values and operations**

- `coll.rs` — the shared array, dict, and struct payloads, generic over the element word type so the VM (`VmValue`) and AOT (`i64`) share one implementation. Value versus reference semantics fall out of `T: Clone`.
- `string.rs` — the tagged-string allocator. Every Jade string carries a trust byte at offset `-1`; strings use an 8-byte header so data pointers stay 8-aligned.
- `strval.rs` — bounded string compare and truthiness for the dynamic ops.
- `dynop.rs` — the single decision core for dynamic (tag-erased) binary operations and negation. This is the divergence-prone center; it returns errors as values.
- `ops.rs`, `num.rs`, `float.rs` — arithmetic support: integer pow, boxed floats.
- `render.rs` — the one value-display implementation. `format_float` produces the shortest round-tripping decimal.
- `coercef.rs` — coercing an LLM reply into a struct, plus the type-to-fields table the compiled path needs.
- `trust.rs` — the taint model. A string from a shell command, file, network, LLM, or stdin is tainted; anything derived only from source literals is trusted. Tainted values are refused at sinks that would execute or fetch them. `JStr` is the VM-side tagged string type.
- `methods.rs` — the runtime method table for AOT dynamic dispatch, used when two types define a method with the same name and arity.

**Concurrency and I/O**

- `task.rs` — a bounded worker pool and the future object tasks resolve. Replaces the one-detached-pthread-per-spawn model that made large fan-outs a resource failure instead of a queue.
- `provider/` — resolves the *active provider slot* under `$HOME/.jade/provider/active/`. It only resolves the slot; loading and driving the provider package is the engines' job. This replaced an `infer/` module holding a Unix-socket client for the inference daemon — inference is an in-process package call now, so there is no transport left to share.
- `uhttpf/`, `httpf.rs` — HTTP over a Unix socket and over TCP.

**Standard-library cores** — `mathf.rs`, `strf.rs`, `fsf.rs`, `pathf.rs`, `envf.rs`, `shf.rs`, `jsonf.rs`, `randomf.rs`, `timef.rs`, `grammarf.rs`. Each holds the shared implementation behind a `std/*` package; the thin `VmValue` marshalling lives in the matching top-level module (`src/math/`, `src/string/`, …).

**FFI surfaces** — `ffi.rs` (scalars and general `jrt_*`), `ffi_coll.rs` (collections), `cstr.rs` (C string helpers). These are `#[no_mangle]` forwarders to the pure Rust implementations in the sibling modules. As symbols moved here from `runtime_aot/common.c`, the C definitions were deleted and their declarations left in `runtime.h`, so the linker resolves them against this crate.

## Who uses it

*Depends on:* nothing in the `jade` crate. The dependency runs one way only.

*Used by:* `src/vm/` and every `std/*` package module call the Rust API directly. `src/aot/` emits calls to the `jrt_*` symbols. `src/runtime_aot/` (C) declares those same symbols in `runtime.h` and calls them. `src/providers/` reads the slot paths from `provider/`.

## Gotchas

**Never make the pool a `#[global_allocator]` here.** Each linked copy of `jade-runtime` — the VM, an AOT binary, a dlopen'd package — has its own pool statics. No pointer ever crosses between them because the FFI deep-copies at the boundary, so no pool ever frees another's memory. A global allocator would break that. The `jade` binary's own global allocator is declared in `src/main.rs`, deliberately.

**Non-raising versus raising.** Functions in `ffi_coll.rs` never raise: a Jade-catchable error cannot be a `longjmp`. Read that file's header before adding an entry point.

Anything here that changes user-visible behavior needs checking on *both* engines, because both link it.

## Building and testing

```sh
cargo test -p jade-runtime
cargo test                      # workspace default-members includes it
```
