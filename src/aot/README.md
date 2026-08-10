# `src/aot/` — the LLVM ahead-of-time backend

## What this subtree is

The other execution engine. `jade build` runs the same frontend and compiler as `jade run`, then lowers the resulting bytecode `Chunk` through LLVM 18 into an object file and links it into a native binary or shared library.

```
Chunk → cfg (basic blocks) → lower (LLVM IR) → object → link
```

It is a peer of `src/vm/`, not a phase of `src/compiler/`. Both consume the same `Chunk`, and the language is defined by where they agree.

## Why it was built

Compiling in-process is a recent and deliberate change. Code generation used to live in a separate repository behind a build daemon on `$HOME/.jade/build.sock`, which had a nasty failure mode: a daemon built from an older commit resolved imports differently from the CLI calling it, and stayed silent until the two disagreed. Now there is one resolver, one code path, one version.

The cost is that **building the toolchain needs LLVM 18 present** (point `LLVM_SYS_180_PREFIX` at it). *Running* a released `jade` needs nothing installed — LLVM is linked in, not loaded.

Two design choices are worth knowing before you edit:

*Tagged register slots.* Every bytecode register becomes an `alloca i64` holding a tagged value word — the same ABI the runtime uses. Instructions load a slot, untag to a native value, compute, re-tag, and store. This is simpler than tracking a static type per register (the emitter reuses slots across types), and LLVM's `mem2reg` and `instcombine` promote the allocas to SSA and fold the `untag(tag(x))` round-trips away, so the tag arithmetic is mostly free after optimization.

*Probe before emit.* `compile()` lowers the whole program into a throwaway module first. If lowering fails partway — an unsupported opcode after some functions and globals were already emitted — only the throwaway module is polluted. The real module is touched only once the whole program is known to lower cleanly.

## What each file does

- **`mod.rs`** — the public entry point. Sets up the LLVM context and target machine, runs the probe, emits a thin `main()`, writes the object file, and drives the linker. `CompileMode` selects a binary or a `jade_pkg_init`-exporting shared library.
- **`cfg.rs`** — control-flow-graph reconstruction. `emit.rs` produces a flat `Vec<Instr>` with PC-relative jumps; LLVM needs basic blocks with explicit edges. This file computes block boundaries and edges and holds no LLVM state at all, so it is unit-testable in isolation.
- **[`lower/`](lower/README.md)** — the bulk of the backend: one LLVM IR translation per opcode, plus the calls into `jade-runtime`'s `jrt_*` C-ABI surface for anything that needs the heap, collections, strings, tasks, or inference. Split by concern across eleven files (`abi`, `arith`, `strings`, `rc`, `exc`, `calls`, `builtins`, `llm`, `instr`); read that directory's README before adding a lowering.
- **`imports.rs`** — import resolution and module namespacing. The VM gives every imported file its own namespace; LLVM has no runtime namespaces, so this file mangles imported symbols to keep two modules that both define `greet` distinct. **The VM is the source of truth for what a namespace means** — read this file's header before changing import behavior.
- **`tests.rs`** — backend tests.

## Who uses it

*Depends on:* `compiler/` for the `TProgram` and `emit`, `bytecode/` for the instruction set, `project/` for library resolution, `inkwell` for LLVM, and the two runtimes it links against — `jade-runtime` (Rust, `src/runtime/`) and `libJadeRuntime.a` (C, `src/runtime_aot/`, built by `build.rs`).

*Used by:* `src/build/`, which is the thin layer `cli/build.rs` calls.

## Gotchas

**An artifact must not name a dependency by where it was when it was built.** It used to: `jade_mod_init` embedded an absolute path per native package and `dlopen`'d it, so a binary ran in the directory that produced it and nowhere else, and said so only at run time on someone else's machine. Each is now named twice — by a `libs/`-relative key, which is what a moved artifact resolves against the root its host published, and by the build-time absolute path, which is the answer for a hand-written `[lib]` that is not a dependency and has no relative spelling. A null key means the second is all there is.

**Only `CompileMode::Binary` publishes a libraries root.** A binary owns its process, so it is the thing entitled to decide the one root every image resolves against. A package is not, and `SharedLib` deliberately emits no `jrt_libs_root_publish` call — a second publisher is a second root, and a second root is a second copy of a dependency with its own state. `runtime_aot/README.md` has the full argument; there is a test that a package emits no publish call, because nothing else would catch it.

**An opcode this backend cannot lower is a hard build error.** There is no legacy fallback, so any new instruction added in `compiler/emit.rs` must be lowered here too.

**The same applies to a builtin, and it is easier to miss.** A builtin the VM has and this backend does not is not a missing feature, it is the two engines disagreeing about what the language is — and the program finds out at `jade build`, long after it was written and tested under `jade run`. `write` and `uhttp.stream` sat in that state until v1.1.34. If you add one to `builtins/` or a `std/*` package, add it here in the same change, or the parity gate will not catch it: a builtin nothing in `examples/` exercises looks fine to every test in the repo.

**Calling a Jade function from a runtime helper is already possible.** A function value is a box whose first word is the raw function pointer, so C calls it as `int64_t (*)(int64_t)` — `jrt_coll_array_map` has always done this, and `jrt_uhttp_stream` does it per line. "It calls back into Jade" is not a reason something cannot be compiled; that reasoning is exactly what kept `uhttp.stream` interpreter-only. The real constraint is narrower: the call must be driven from C, not Rust, because a raising handler's `longjmp` must not unwind through a Rust frame.

**A runtime helper that reaches a value's *kind* has to handle every kind the VM does.** `jrt_get_field` handled `JK_STRUCT` and fell through to "value has no fields" for anything else, but the VM reads `d.key` on a dict as `d["key"]`. So dot-access on a dict worked interpreted and raised compiled, and it stayed hidden because method *calls* never reach that path — codegen rewrites `d.keys()` into a direct call, so only data keys showed the gap. When a helper here switches on `jrt_kind_of`, check the VM's corresponding `dispatch.rs` arm for every case it accepts, not just the one you came for.

**A value that only ever seems to flow one way still needs a representation.** `MakePrompt` used to store a prompt as the bare string it wraps, reasoning that a prompt only ever reaches `PromptDeref`. It does not — a prompt can be printed, held in a collection, stored in a struct field, passed, or returned — so a compiled binary showed a prompt's text where the VM showed `<prompt>`, and `MakeStruct` had to refuse prompt fields outright. The fix was to give it a real kind (`jade_runtime::promptf`), which also meant an arm in `gc::is_collection` and one in `gc::free_obj`; without the first it leaked one object per prompt. Anything new that reaches a `TAG_PTR` word needs the same three.

**A method's name does not prove its receiver's kind.** `chunk_val_method_supported` / `chunk_str_method_supported` pick the arm from the name, and the arms used to untag the receiver straight to a pointer of the kind that name implied. With an untyped parameter — `fn f(v) { v.keys() }` — the kind is only known when `f` is called, so `v.keys()` on a string dereferenced a `char*` as a dict, and `v.upper()` on an int dereferenced a small integer. Every arm that untags now calls `jrt_require_kind` (or `jrt_require_str_arg` for a str method's arguments) first. `len` and `contains` are deliberately exempt: they hand the whole tagged word to `jrt_len_chunk` / `jrt_in_any`, which dispatch on the tag themselves. The guard has to *raise* rather than abort — the idiomatic type test is `try { v.keys() } catch e { … }`, and nothing can catch a segfault.

**A borrowed value word must be retained at the point it becomes owned.** The runtime's collection reads hand back the value word without incrementing anything, and the *caller* retains — `GetIndex` does so with a comment saying why. The `dict.get` arm did not, so each call decremented the entry until the collection it named was freed under the table. If you add a lowering that stores a word the runtime handed you, decide explicitly whether it arrived owned (a producer like `jrt_coll_dict_keys`) or borrowed (a lookup), because the two need opposite treatment and both compile.

**A buffer a call site needs is allocated in the entry block, not at the call.** LLVM does not reclaim an `alloca` until the function returns, so a call that marshals its arguments into a fresh one grows the stack every time it runs — and this backend puts calls inside loops. An FFI call in a `while` loop died at a fixed iteration count for that reason, with the count scaling exactly with `ulimit -s`. `Lowerer::entry_buf` is how a lowering asks for such a buffer; [`lower/README.md`](lower/README.md) has the rule and why sharing one is sound.

**The handler stack is thread-wide here, not per frame.** The VM keeps its `handlers` in a local of the dispatch call frame, so a function's handlers die with it. `jade_exc_push_frame` pushes onto one `_Thread_local` stack that only codegen unwinds, and the emitter emits `PopHandler` solely on a try body's normal fall-through — which `try { …; return x } catch e { … }` never reaches. Each function containing a `try` therefore snapshots `jade_exc_depth()` in its prologue and calls `jade_exc_restore` on every return path (explicit `Return`, `Halt`, and the implicit run-off-the-end). Miss one of those paths and a dead `jmp_buf` outlives its stack frame, which shows up as a segfault, an infinite spin, or a raise landing in the wrong handler depending on what overwrote the stack.

The linker line is `-L target/<profile> -ljade_runtime`, which only works because `jade-runtime` is a *workspace member* named in `default-members` — Cargo only uplifts a build artifact to `target/<profile>/` when the crate is a requested top-level target. The root `Cargo.toml` has the full explanation. Do not demote it back to a plain path dependency.

## Building and testing

```sh
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18   # or your install
cargo test aot::
./target/debug/jade build examples/arithmatic/arithmetic/arithmetic.jde -o /tmp/a && /tmp/a
./target/debug/jade build file.jde --emit ir           # inspect the IR
./src/scripts/backend-parity.sh                            # diff against the VM
```
