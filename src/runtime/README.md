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
- `handle.rs` — `HandleObj`, an opaque pointer from a native package plus the C type it came from. A value with no operations: Jade holds it, hands it back, and never dereferences it — which is enforced by storing the pointer as a `usize` rather than a `*mut c_void`, and gets `Send`/`Sync` for free at the same time. Its destructor reclaims the type name and the wrapper and deliberately *not* the pointee, which belongs to the library that issued it.
- `dynop.rs` — the single decision core for dynamic (tag-erased) binary operations and negation. This is the divergence-prone center; it returns errors as values.
- `promptf.rs` — `PromptObj`, a prompt value on the AOT heap: a header plus the tagged string it wraps. Unlike `GrammarObj` this is not shared with the VM, which has `VmValue::Prompt`; it exists so the AOT has the same distinction. Before it, `MakePrompt` stored the bare string, so a compiled binary printed a prompt's text where `jade run` printed `<prompt>`, and struct prompt fields could not be lowered at all.
- `ops.rs`, `num.rs`, `float.rs` — arithmetic support: integer pow, boxed floats. `ops::eq` is the strict `==`, which rejects a comparison across kinds; `ops::eq_total` is the one membership uses, where a cross-kind pair answers "not equal" rather than raising. Both engines read the second for `arr.contains(x)` — they disagreed about it until mixed arrays made the case reachable in v1.1.32.
- `render.rs` — the one value-display implementation. `format_float` produces the shortest round-tripping decimal, always positional: never scientific notation, whatever the magnitude. It is now the only one — the AOT runtime used to format floats itself in C with `"%.*g"`, which meant a compiled binary printed `1e+01` for `10.0`.
- `coercef.rs` — coercing an LLM reply into a struct, plus the type-to-fields table the compiled path needs.
- `trust.rs` — the taint model. A string from a shell command, file, network, LLM, or stdin is tainted; anything derived only from source literals is trusted. Tainted values are refused at sinks that would execute or fetch them. `JStr` is the VM-side tagged string type.
- `methods.rs` — the runtime method table for AOT dynamic dispatch, used when two types define a method with the same name and arity.

**Concurrency and I/O**

- `task.rs` — a bounded worker pool and the future object tasks resolve. Replaces the one-detached-pthread-per-spawn model that made large fan-outs a resource failure instead of a queue.
- `provider/` — resolves the *active provider slot* under `$HOME/.jade/provider/active/`. It only resolves the slot; loading and driving the provider package is the engines' job. This replaced an `infer/` module holding a Unix-socket client for the inference daemon — inference is an in-process package call now, so there is no transport left to share.
- `uhttpf/`, `httpf.rs` — HTTP over a Unix socket and over TCP. Each has a text core and a byte core, and the byte one is the real implementation: `request` is `request_bytes` put through `body_text`. That layering is not decoration. A `str` is UTF-8 and NUL-terminated, so reading a body as text substitutes `�` for invalid sequences *and* stops at the first NUL — and until v1.2.5 only the compiled path truncated, so `http.get` on a body holding a NUL reported 8 characters under `jade run` and 4 from the same program built. `body_text` is that rule written once, and `get_bytes`/`post_bytes` are how a program avoids it. `uhttpf` also holds `Stream`, the reader behind `uhttp.stream`: it connects, parses the status and headers, and yields one body line at a time across chunked or raw framing. It is deliberately *pull*-shaped rather than callback-shaped, because the two engines drive it differently — the VM pumps it from a worker thread into a tokio channel, while the compiled path drives it inline from `jrt_uhttp_stream` in `runtime_aot/common.c`, which is what calls the Jade handler. Keeping the handler call on the C side matters: a handler that raises does a `longjmp`, and that must not unwind through a Rust frame.

**Standard-library cores** — `mathf.rs`, `strf.rs`, `fsf.rs`, `pathf.rs`, `envf.rs`, `shf.rs`, `jsonf.rs`, `randomf.rs`, `timef.rs`, `grammarf.rs`. Each holds the shared implementation behind a `std/*` package; the thin `VmValue` marshalling lives in the matching top-level module (`src/math/`, `src/string/`, …).

A core that can *fail* has one more thing to arrange. A Jade raise is a `longjmp`, which must not unwind through a Rust frame, so nothing here throws: the function records the message and returns a neutral value, and a small C forwarder in `runtime_aot/common.c` drains it and raises. `fsf.rs`, `httpf.rs`, `uhttpf/`, `bytesf.rs` and `jsonf.rs` all work this way; `mathf.rs` uses the simpler version, an out-param error code with the message living on the C side, which is enough when the wording is fixed.

Skipping that arrangement is the standing failure mode, and it is invisible: the compiled program answers nil where the VM raises, so it takes the success branch and carries on. `json.parse` did exactly that until v1.3.12 — a comment even said so — and no example parsed invalid JSON, so the parity gate never looked. If a core returns a `Result`, decide where the error surfaces before deciding what it returns.

**FFI surfaces** — `ffi.rs` (scalars and general `jrt_*`), `ffi_coll.rs` (collections), `cstr.rs` (C string helpers). These are `#[no_mangle]` forwarders to the pure Rust implementations in the sibling modules. As symbols moved here from `runtime_aot/common.c`, the C definitions were deleted and their declarations left in `runtime.h`, so the linker resolves them against this crate.

## Who uses it

*Depends on:* nothing in the `jade` crate. The dependency runs one way only.

*Used by:* `src/vm/` and every `std/*` package module call the Rust API directly. `src/aot/` emits calls to the `jrt_*` symbols. `src/runtime_aot/` (C) declares those same symbols in `runtime.h` and calls them. `src/providers/` reads the slot paths from `provider/`.

## Gotchas

**Never make the pool a `#[global_allocator]` here.** Each linked copy of `jade-runtime` — the VM, an AOT binary, a dlopen'd package — has its own pool statics. No pointer ever crosses between them because the FFI deep-copies at the boundary, so no pool ever frees another's memory. A global allocator would break that. The `jade` binary's own global allocator is declared in `src/main.rs`, deliberately.

**Non-raising versus raising.** Functions in `ffi_coll.rs` never raise: a Jade-catchable error cannot be a `longjmp`. Read that file's header before adding an entry point.

**Mutating versus copying.** Some collection functions exist in both forms, and the pair has to keep its meanings straight: `jrt_coll_array_sort` sorts in place for `a.sort()`, while `jrt_coll_array_sorted` returns a new array for `array.sort(a)`. They are not interchangeable, and using one where the other belongs is a silent behaviour change rather than an error — see `codegen`'s `package_fn_is_the_method`.

Anything here that changes user-visible behavior needs checking on *both* engines, because both link it.

## Building and testing

```sh
cargo test -p jade-runtime
cargo test                      # workspace default-members includes it
```
