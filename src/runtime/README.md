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
- `bytesf.rs` holds `BytesObj`, a binary blob: a header, a trust byte, and an owned `Vec<u8>`. It also holds the shared core of `std::bytes`, meaning `zeros`, `from_ints`, `concat`, and the octet write behind `b[i] = v`, plus the text of every message they can raise. The messages live here rather than in either engine because a program can *catch* one, which makes the wording part of the language: two copies would let the VM and a compiled binary say different things about the same failure. The payload is a plain `Vec<u8>` with no lock, exactly as `coll.rs` keeps its arrays, so the VM wraps the object in a `Mutex` and the compiled path relies on `compiler::taskcheck` instead.
- `handle.rs` holds `HandleObj`, which is an opaque pointer from a native package plus the C type it came from. It is a value with no operations. Jade holds it, hands it back, and never dereferences it. Storing the pointer as a `usize` rather than a `*mut c_void` enforces that, and it makes the type `Send` and `Sync` at the same time. The destructor reclaims the type name and the wrapper, and deliberately *not* the pointee, which belongs to the library that issued it.
- `dynop.rs` is the single decision core for dynamic binary operations and negation, meaning the tag-erased ones. It is the most divergence-prone code here, and it returns errors as values.
- `promptf.rs` holds `PromptObj`, a prompt value on the AOT heap: a header plus the tagged string it wraps. Unlike `GrammarObj`, it is not shared with the VM, which has `VmValue::Prompt` instead. It exists so the AOT path can make the same distinction. Before it, `MakePrompt` stored the bare string, so a compiled binary printed a prompt's text where `jade run` printed `<prompt>`, and struct prompt fields could not be lowered at all.
- `ops.rs`, `num.rs`, and `float.rs` hold arithmetic support, covering integer power and boxed floats. `ops::eq` is the strict `==`, which rejects a comparison across kinds. `ops::eq_total` is what membership uses, where a cross-kind pair answers "not equal" rather than raising. Both engines read the second for `arr.contains(x)`. They disagreed about it until mixed arrays made the case reachable in v1.1.32.
- `render.rs` is the one implementation of value display. `format_float` produces the shortest decimal that round-trips, always in positional form and never in scientific notation, whatever the magnitude. It is now the only such implementation. The AOT runtime used to format floats itself in C with `"%.*g"`, which meant a compiled binary printed `1e+01` for `10.0`.
- `coercef.rs` handles coercing an LLM reply into a struct, plus the type-to-fields table the compiled path needs.
- `trust.rs` holds the taint model. A string from a shell command, a file, the network, an LLM, or stdin is tainted. Anything derived only from source literals is trusted. Tainted values are refused at any sink that would execute or fetch them. `JStr` is the VM-side tagged string type.
- `methods.rs` holds the runtime method table for AOT dynamic dispatch, used when two types define a method with the same name and the same number of arguments.

*Concurrency and I/O*

- `task.rs` holds a bounded worker pool, the future object tasks resolve, and `max_tasks`/`set_max_tasks`, the one number that says how wide a fan-out gets. It replaced a model that detached one pthread per spawn, which turned a large fan-out into a resource failure rather than a queue. A future carries its own body until somebody claims it, which is what lets an awaiting thread run the task itself rather than park — see *An awaiting thread runs the task* below.
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

*A value that owns another value has to retain it, whatever its kind.* `is_collection` decides which `TAG_PTR` words the refcount ops act on, and the list is not "containers" but "things that own a child". A bound method (`let greet = person.greet`) owns its receiver. It was left off the list on the reasoning that it shares a shape with the static fn boxes, which are not refcounted either — but a fn box is an LLVM global constant that owns nothing, so there is nothing there to dangle. A bound method is a real allocation holding a real reference, and leaving it out meant the receiver was freed the moment the frame that built it returned. `fn mk() { let c = C { n: 1 }; return c.get }` then crashed when the binding was called.

The same rule reaches past this crate. A spawned task outlives the expression that started it, so `task::spawn` retains its arguments and the worker releases them once the body has run — its `owns_args` flag says whether the words are tagged Jade values at all, because this crate's own tests hand tasks raw untagged integers.

*A future answers four questions, and only one of them blocks.* `await` blocks, which is the right default and useless to a loop that cannot stop. `ready` asks without blocking; `wait_any` blocks on several at once and answers which, so a program with nothing to draw can idle rather than poll; `cancel` says the caller has stopped waiting. Cancelling does not stop the work — a task is a real thread running straight-line code with no point at which anything outside it could interrupt — so what it changes is the caller's side, and a task that wants to give up early checks `current_is_cancelled`.

`wait_any` needs a signal `await` does not: a waiter interested in N futures cannot park on N condvars, so every completion broadcasts on one shared `Completions` and the waiter rechecks its list. One broadcast per completion, which is cheap next to the work that produced it.

*Timers get a thread, not a task.* `after(secs)` is a future with no body, finished by a single thread parked on the earliest deadline. A task that sleeps holds a pool worker and does not announce itself as blocked, because only `await` does that, so a redraw loop arming a 16ms timer every frame would fill the pool with sleepers and stall real work behind them. A program with no timers never creates the thread.

Starting it is retried per call rather than done once. A `Once` counts the *attempt*, so a machine briefly out of threads at the first `after` left every later `time.after` in that process waiting on a deadline nothing would ever fire, which turns a transient condition into a permanent hang. And if the thread genuinely cannot start, `after` hands back null rather than a future: a timer future has no body, so one nobody will fire is a wait with no end. `jade_time_after` turns the null into an error naming the two things that fix it, which is the honest failure — a hang is the worst way for this to go wrong.

*How wide a fan-out gets is one number, and a program sets it.* `max_tasks` starts at a flat 32 and `set_max_tasks` changes it, both reachable from Jade as bare globals. It used to be the machine's core count, overridable only through a `JADE_MAX_TASKS` environment variable that appeared nowhere in the docs — so the same fan-out ran in a different number of waves on a laptop and a build server, and the only way to learn the knob existed was to read this file. A Jade task is far more often waiting on a socket than saturating a core, which is why sizing it to cores was measuring the wrong resource.

Two things had to change for that number to mean anything. The limit is now read on every scheduling decision rather than captured when the pool is built, because a setter that only took effect on the next *run* is not a setter. And a worker checks the limit before claiming queued work, not only before starting a thread: the pool keeps idle threads for `IDLE_TIMEOUT` after a burst, so bounding growth alone let a wide fan-out's leftover threads ignore a limit the program had since lowered. Peak concurrency, not thread count, is what `max_tasks` promises.

*A parked thread gives its slot back.* `enter_blocking` decrements the running count and `exit_blocking` restores it, tracked per thread because the thread that has to give a slot back is whichever one reaches `await`, several frames inside a body that knows nothing about the pool. Without it, `set_max_tasks(1)` plus a task that awaits another is a deadlock by construction: the parent holds the only slot and the child can never take one. Resuming does not queue for a slot, which would put the same deadlock back; it can leave the count briefly over the limit, the same allowance an awaiter running a body inline already has.

*An awaiting thread runs the task.* `await` blocks a whole OS thread in a compiled binary, so a chain of N nested awaits pins N threads. Growing the pool when a worker blocks is not enough on its own, because every pool has a last thread: at `HARD_MAX_WORKERS` the innermost body had nobody left to run it and the whole chain waited forever, which is a worse failure than the abort it replaced. So the body lives in the future it resolves rather than in the pool's queue, and `await` claims it and calls it inline when nobody else has. Nested `await` then costs stack rather than threads, and it spends the same budget: an inline body keeps counting against the recursion limit, because it really is deeper on a stack already in use. Skip that and nothing bounds an `await` chain at all, which trades a hang for a SIGBUS with no output.

Three things follow, and all three are in the code because the depth test found them one after another. A body may now run on a thread that has work of its own, so the `setjmp` shim in `runtime_aot/posix.c` saves and restores both the handler depth and the recursion depth around it — otherwise a generator that raised inside a task orphaned its buffer onto the *awaiter's* stack, and the awaiter's own `yield`s landed in it. `run_job` brackets the generator stack for the same reason, which also fixes the older version of that bug where a pool worker carried a leftover frame into its next task. And a task is a fresh call chain, matching the interpreter's `new_for_spawn`, so `jrt_recur_enter_task` zeroes the recursion budget for the body rather than letting the awaiter's depth count against it.

The pool's ceiling is now a cap on parallelism alone. Hitting it means the next body waits for a thread instead of getting one of its own; it can no longer stop a program. The same goes for running out of threads entirely — `start_worker` reports a failure back to its caller rather than trying to undo the bookkeeping itself, which re-locked a mutex the calling thread already held and wedged `spawn` against itself. A binary now finishes correctly with every `pthread_create` failing, and says so once on stderr when it starts happening. That is deliberately a warning and not an error: the work is not lost, because whoever awaits the future runs the body inline, so the run is slower and still correct. Raising would turn that into a failure, and it would fail on a loaded machine and pass on an idle one, which is a poor property for anything a `catch` might match. What is worth saying is that the fan-out has gone serial, since a program in that state looks exactly like one that was always slow. Workers get the same 256 MB stack the main body gets, because Rust's 2 MiB default made the same function succeed at top level and overflow inside an `async fn` — and a stack overflow on a worker takes the process down with no Jade error to read.

*The destructor cascade is iterative, not recursive.* `free_obj` walks an explicit worklist: `release_word` drops one reference and hands back a pointer that reached zero rather than reclaiming it on the spot, and the loop reclaims it on a later turn. Depth then costs heap instead of stack. Recursing instead overflowed on a chain of arrays roughly 30,000 deep, which a program builds by wrapping one array in a loop. `Vec::new` does not allocate, so a leaf object still frees without touching the allocator.

Anything here that changes user-visible behavior needs checking on *both* engines, because both link this crate.

## Building and testing

```sh
cargo test -p jade-runtime
cargo test                      # workspace default-members includes it
```
