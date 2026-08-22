# `src/runtime/`: `jade-runtime`, the shared runtime crate

## What this subtree is

This is a separate Rust crate holding Jade's *value semantics* in one place, so the two execution engines cannot drift apart.

- The bytecode *VM*, which is `jade run`, links it as an `rlib` and calls it natively.
- A *compiled binary*, produced by `jade build`, links it as a C-ABI `staticlib`. That resolves the `#[no_mangle] extern "C"` `jrt_*` symbols codegen emits.

It is a workspace member rather than merely a path dependency, and the root `Cargo.toml` explains why in detail. Cargo only copies `libjade_runtime.a` up into `target/<profile>/` when the crate is a requested top-level target, and the linker line `jade build` emits looks for it exactly there. Being a member also means `cargo test` runs its roughly 70 tests.

## Why it was built

The VM, written in Rust around `VmValue` and `Arc`, and the AOT backend, written in C around `jrt_*_any`, used to be two independent implementations of the same language. Every place they diverged became a bug reconciled after the fact. A float printed `3` in one and `3.0` in the other. A dict sorted its keys differently. Integer overflow raised an error in the VM and wrapped in compiled code. This crate is the structural fix: shared code, and one behavior.

It deliberately has no dependencies and no LLVM, so it builds everywhere `jade run` runs.

## What each file does

*Value representation and memory*

- `value.rs` holds the tagged 64-bit value ABI, `JadeValue`, byte-identical to `runtime_aot/runtime.h`. It is pure bit manipulation, with no allocation.
- `heap.rs` holds `ObjHeader`, the shared header on every reference type. It carries the reference count plus the cycle collector's color and flags.
- `gc.rs` handles heap accounting. `leak_obj` and `free_obj` keep a live-object counter, because no golden-output test can observe a leak *or* a premature free. Counting makes the heap population measurable, which is what lets the collector be verified.
- `pool.rs` holds the segregated free-list allocator both engines use. These are ordinary functions and *never a `#[global_allocator]`*. That rule is what keeps them safe in a process that has `dlopen`ed a native package.
- `arena.rs` holds the per-frame bump arena, used for collections the compiler proved do not escape. It pairs with `compiler/escape.rs`.
- `sys.rs` holds raw `malloc` and `free` bindings, so heap objects are interchangeable with the C runtime.

*Values and operations*

- `coll.rs` holds the shared array, dict, and struct payloads. It is generic over the element word type, so the VM with its `VmValue` and the AOT path with its `i64` share one implementation. Value semantics and reference semantics both fall out of `T: Clone`.
- `string.rs` holds the tagged-string allocator. Every Jade string carries a trust byte at offset `-1`, and strings use an 8-byte header so data pointers stay aligned to 8 bytes.
- `strval.rs` holds bounded string comparison and truthiness, for the dynamic operators.
- `handle.rs` holds `HandleObj`, which is an opaque pointer from a native package plus the C type it came from. It is a value with no operations. Jade holds it, hands it back, and never dereferences it. Storing the pointer as a `usize` rather than a `*mut c_void` enforces that, and it makes the type `Send` and `Sync` at the same time. The destructor reclaims the type name and the wrapper, and deliberately *not* the pointee, which belongs to the library that issued it.
- `dynop.rs` is the single decision core for dynamic binary operations and negation, meaning the tag-erased ones. It is the most divergence-prone code here, and it returns errors as values.
- `promptf.rs` holds `PromptObj`, a prompt value on the AOT heap: a header plus the tagged string it wraps. Unlike `GrammarObj`, it is not shared with the VM, which has `VmValue::Prompt` instead. It exists so the AOT path can make the same distinction. Before it, `MakePrompt` stored the bare string, so a compiled binary printed a prompt's text where `jade run` printed `<prompt>`, and struct prompt fields could not be lowered at all.
- `ops.rs`, `num.rs`, and `float.rs` hold arithmetic support, covering integer power and boxed floats. `ops::eq` is the strict `==`, which rejects a comparison across kinds. `ops::eq_total` is what membership uses, where a cross-kind pair answers "not equal" rather than raising. Both engines read the second for `arr.contains(x)`. They disagreed about it until mixed arrays made the case reachable in v1.1.32.
- `render.rs` is the one implementation of value display. `format_float` produces the shortest decimal that round-trips, always in positional form and never in scientific notation, whatever the magnitude. It is now the only such implementation. The AOT runtime used to format floats itself in C with `"%.*g"`, which meant a compiled binary printed `1e+01` for `10.0`.
- `coercef.rs` handles coercing an LLM reply into a struct, plus the type-to-fields table the compiled path needs.
- `trust.rs` holds the taint model. A string from a shell command, a file, the network, an LLM, or stdin is tainted. Anything derived only from source literals is trusted. Tainted values are refused at any sink that would execute or fetch them. `JStr` is the VM-side tagged string type.
- `methods.rs` holds the runtime method table for AOT dynamic dispatch, used when two types define a method with the same name and the same number of arguments.

*Concurrency and I/O*

- `task.rs` holds a bounded worker pool and the future object tasks resolve. It replaced a model that detached one pthread per spawn, which turned a large fan-out into a resource failure rather than a queue.
- `provider/` resolves the *active provider slot* under `$HOME/.jade/provider/active/`. It only resolves the slot. Loading and driving the provider package is the engines' job. It replaced an `infer/` module holding a Unix-socket client for the inference daemon. Inference is an in-process package call now, so there is no transport left to share.
- `uhttpf/` and `httpf.rs` handle HTTP over a Unix socket and over TCP. Each has a text core and a byte core, and the byte one is the real implementation: `request` is `request_bytes` passed through `body_text`.

  That layering is not decoration. A `str` is UTF-8 and NUL-terminated, so reading a body as text substitutes `�` for invalid sequences *and* stops at the first NUL byte. Until v1.2.5, only the compiled path truncated, so `http.get` on a body holding a NUL reported 8 characters under `jade run` and 4 from the same program built. `body_text` is that rule written once, and `get_bytes` and `post_bytes` are how a program avoids it.

  `uhttpf` also holds `Stream`, the reader behind `uhttp.stream`. It connects, parses the status and headers, and yields one body line at a time, across chunked or raw framing. It is deliberately *pull*-shaped rather than callback-shaped, because the two engines drive it differently. The VM pumps it from a worker thread into a tokio channel, while the compiled path drives it inline from `jrt_uhttp_stream` in `runtime_aot/common.c`, which is what calls the Jade handler. Keeping the handler call on the C side matters: a handler that raises does a `longjmp`, and that must not unwind through a Rust frame.

*Standard-library cores* are `mathf.rs`, `strf.rs`, `fsf.rs`, `pathf.rs`, `envf.rs`, `shf.rs`, `jsonf.rs`, `randomf.rs`, `timef.rs`, and `grammarf.rs`. Each holds the shared implementation behind one `std/*` package. The thin `VmValue` marshalling lives in the matching top-level module, such as `src/math/` or `src/string/`.

A core that can *fail* has one more thing to arrange. A Jade raise is a `longjmp`, and that must not unwind through a Rust frame, so nothing here throws. Instead the function records the message and returns a neutral value, and a small C forwarder in `runtime_aot/common.c` drains the message and raises.

`fsf.rs`, `httpf.rs`, `uhttpf/`, `bytesf.rs`, and `jsonf.rs` all work that way. `mathf.rs` uses the simpler version: an out-parameter error code, with the message living on the C side. That is enough when the wording is fixed.

Skipping that arrangement is the standing failure mode here, and it is invisible. The compiled program answers nil where the VM raises, so it takes the success branch and carries on. `json.parse` did exactly that until v1.3.12, and a comment even said so. No example parsed invalid JSON, so the parity gate never looked. If a core returns a `Result`, decide where the error surfaces before you decide what it returns.

*FFI surfaces* are `ffi.rs` for scalars and the general `jrt_*` set, `ffi_coll.rs` for collections, and `cstr.rs` for C string helpers. All are `#[no_mangle]` forwarders to the pure Rust implementations in the sibling modules. As symbols moved here from `runtime_aot/common.c`, the C definitions were deleted and their declarations left in `runtime.h`, so the linker resolves them against this crate.

## Who uses it

*Depends on:* nothing in the `jade` crate. The dependency runs one way only.

*Used by:* `src/vm/` and every `std/*` package module, which call the Rust API directly. `src/aot/` emits calls to the `jrt_*` symbols. `src/runtime_aot/`, written in C, declares those same symbols in `runtime.h` and calls them. `src/providers/` reads the slot paths from `provider/`.

## Gotchas

*Never make the pool a `#[global_allocator]` here.* Each linked copy of `jade-runtime` has its own pool statics, and there are several: the VM, a compiled binary, and every `dlopen`ed package. No pointer ever crosses between them, because the FFI deep-copies at the boundary, so no pool ever frees another pool's memory. A global allocator would break that. The `jade` binary's own global allocator is declared in `src/main.rs`, deliberately.

*Non-raising compared to raising.* Functions in `ffi_coll.rs` never raise, because a Jade-catchable error cannot be a `longjmp`. Read that file's header before adding an entry point.

*A dict is a compact hash map.* `DictObj` keeps its entries in one vector, in insertion order. Once the dict grows past `DICT_SCAN_MAX`, it also keeps an open-addressed table mapping a key's hash to that key's position in the vector.

`entries()` hands back insertion order, so rendering and `value_copy` are unaffected. The index only answers "where is this key". Until v1.3.22 there was no index at all, just a vector searched by scanning, which made every lookup cost time proportional to the size and building a dict cost time proportional to the square of it. Small dicts still skip the table, because scanning a contiguous vector wins at that size, and most dicts are that size.

*Mutating compared to copying.* Some collection functions exist in both forms, and the pair has to keep its meanings straight. `jrt_coll_array_sort` sorts in place, for `a.sort()`. `jrt_coll_array_sorted` returns a new array, for `array.sort(a)`. They are not interchangeable, and using one where the other belongs is a silent change in behavior rather than an error. See `package_fn_is_the_method` in `codegen`.

`jrt_obj_unique` is a third case. A dict has value semantics, so a write has to leave any other name for the same dict alone. But a copy is only observable when somebody else is actually holding the dict, and the reference count answers exactly that question. Checking it is what lets the compiled `d[k] = v` path write in place rather than copy on every write.

Anything here that changes user-visible behavior needs checking on *both* engines, because both link this crate.

## Building and testing

```sh
cargo test -p jade-runtime
cargo test                      # workspace default-members includes it
```
