# `src/aot/`: the `jade build` driver

## What this subtree is

This is the other execution engine, seen from the outside. `jade build` runs the same frontend and compiler as `jade run`, then turns the resulting bytecode `Chunk` into a native binary or a shared library.

```
Chunk → codegen (LLVM IR) → object → link
```

This directory owns the first and last steps, and nothing in between. It resolves imports into one namespaced stream, hands the `Chunk` to [`src/codegen/`](../codegen/README.md), wraps what comes back in a `main()` or a `jade_pkg_init`, writes the object file, and drives `cc`. The translation itself, which is one LLVM IR sequence per opcode, lives in `codegen`. It used to live here as `aot/lower/`.

It is a peer of `src/vm/`, not a phase of `src/compiler/`. Both consume the same `Chunk`, and the language is defined by where the two agree.

## Why it was built

Compiling in-process is a recent and deliberate change. Code generation used to live in a separate repository, behind a build daemon on `$HOME/.jade/build.sock`. That had a nasty failure mode. A daemon built from an older commit resolved imports differently from the CLI calling it, and stayed silent until the two disagreed. Now there is one resolver, one code path, and one version.

The cost is that *building the toolchain needs LLVM 18 installed*, with `LLVM_SYS_180_PREFIX` pointing at it. *Running* a released `jade` needs nothing installed, because LLVM is linked in rather than loaded.

Two design choices are worth knowing before you edit:

*Tagged register slots.* Every bytecode register becomes an `alloca i64` holding a tagged value word, using the same ABI the runtime uses. An instruction loads a slot, untags it to a native value, computes, re-tags, and stores.

That is simpler than tracking a fixed type per register, because the emitter reuses slots across types. LLVM's `mem2reg` and `instcombine` passes then promote the allocas to SSA form and fold the `untag(tag(x))` round trips away, so the tag arithmetic is nearly free after optimization.

*Probe before emit.* `compile()` lowers the whole program into a throwaway module first. If lowering fails partway through, say on an unsupported opcode after some functions and globals were already emitted, only the throwaway module is affected. The real module is touched only once the whole program is known to lower cleanly.

## What each file does

- *`mod.rs`* is the public entry point. It sets up the LLVM context and target machine, runs the probe, emits a thin `main()`, writes the object file, and drives the linker. `CompileMode` selects either a binary or a shared library exporting `jade_pkg_init`.
- *`imports.rs`* handles import resolution and module namespacing. The VM gives every imported file its own namespace, while LLVM has no namespaces at run time. So this file mangles imported symbols to keep two modules that both define `greet` distinct. *The VM is the source of truth for what a namespace means*, so read this file's header before changing import behavior. It is also where a call into a C-ABI dependency is checked against the symbols its manifest declares, which the gotcha below explains.
- *`tests.rs`* holds the driver tests.

The translation this directory drives lives in [`src/codegen/`](../codegen/README.md). It is one LLVM IR sequence per opcode, split by concern across ten files. Read that directory's README before adding a lowering.

## Who uses it

*Depends on:* `codegen/` for the whole translation from `Chunk` to LLVM IR, `compiler/` for the `TProgram` and `emit`, `bytecode/` for the instruction set, `project/` for library resolution, and `inkwell` for LLVM. It also depends on the two runtimes it links against: `jade-runtime`, written in Rust in `src/runtime/`, and `libJadeRuntime.a`, written in C in `src/runtime_aot/` and built by `build.rs`.

*Used by:* `src/build/`, which is the thin layer `cli/build.rs` calls.

## Gotchas

*A mistyped FFI symbol is nobody's link error, so this pass has to be the one that catches it.* `gfx.jade_gfx_key_presed` used to pass `jade check`, then build, link, package, and ship, and fail the first time that line ran, reported as "dict has no key or method". Nothing linked the name, so no linker could object to it, and the runtime only discovers the gap when it looks the symbol up.

The answer was in the project's own `jade.toml` the whole time. An `abi = "c"` dependency must declare a `[symbols]` table, and that table is the complete list of what the generated shim binds. `Renamer::ref_native_qual` is the one place `alias.field` becomes `__native$<pkgid>$<field>`, and it now checks the field against that table. Its `.jde` sibling `ref_value_qual` has always checked whether the module exports the name, in exactly the same way.

Two rules keep the check from rejecting anything real. A package with *no* declared table is not checked at all, because a Jade-ABI package declares its exports in its own project, which this manifest cannot see. An empty set would otherwise reject every call it has ever served. And a `[lib]` sharing a name with a dependency wins the import, so the dependency's table describes a library the build is not using, and the check turns itself off.

*`would_build` has to tell an unresolved import apart from a wrong program.* It probes a build on behalf of `jade check`, and deliberately stays quiet about an import that does not resolve, because that means a dependency is not installed and `check_imports` says so in better words.

That silence used to swallow *every* resolver error, which is exactly why `check` reported `ok` for a program with a bad FFI symbol. `ResolveError` now splits the two. An `Unresolved` error is still dropped, and a `Program` error is reported.

*An artifact must not name a dependency by where that dependency sat at build time.* It used to. `jade_mod_init` embedded an absolute path for each native package and called `dlopen` on it, so a binary ran in the directory that produced it and nowhere else. It said so only at run time, on someone else's machine.

Each dependency is now named twice. A `libs/`-relative key is what a moved artifact resolves against the root its host published. The build-time absolute path is the answer for a hand-written `[lib]` that is not a dependency and has no relative spelling. A null key means the absolute path is all there is.

*Only `CompileMode::Binary` publishes a libraries root.* A binary owns its process, so it is the thing entitled to decide the one root every image resolves against. A package is not entitled to that, so `SharedLib` deliberately emits no `jrt_libs_root_publish` call. A second publisher means a second root, and a second root means a second copy of a dependency with its own state. `runtime_aot/README.md` carries the full argument. There is a test asserting that a package emits no publish call, because nothing else would catch it.

*An opcode this backend cannot lower is a hard build error.* There is no fallback path, so any new instruction added in `compiler/emit.rs` must be lowered here as well.

*The same applies to a builtin, and it is easier to miss.* A builtin the VM has and this backend does not is not a missing feature. It is the two engines disagreeing about what the language is, and the program only finds out at `jade build`, long after it was written and tested under `jade run`. `write` and `uhttp.stream` sat in that state until v1.1.34.

If you add one to `builtins/` or a `std/*` package, add it here in the same change. Otherwise the parity gate will not catch it, because a builtin nothing in `examples/` exercises looks fine to every test in the repo.

*Calling a Jade function from a runtime helper is already possible.* A function value is a box whose first word is the raw function pointer, so C calls it as an `int64_t (*)(int64_t)`. `jrt_coll_array_map` has always done this, and `jrt_uhttp_stream` does it once per line.

"It calls back into Jade" is not a reason something cannot be compiled. That reasoning is exactly what kept `uhttp.stream` interpreter-only. The real constraint is narrower: the call must be driven from C rather than Rust, because a raising handler's `longjmp` must not unwind through a Rust frame.

*A runtime helper that reaches a value's kind has to handle every kind the VM does.* `jrt_get_field` handled `JK_STRUCT` and fell through to "value has no fields" for anything else. But the VM reads `d.key` on a dict as `d["key"]`. So dot access on a dict worked under the interpreter and raised when compiled.

That stayed hidden because method *calls* never reach the same path. Codegen rewrites `d.keys()` into a direct call, so only data keys showed the gap. When a helper here switches on `jrt_kind_of`, check the VM's matching `dispatch.rs` arm for every case it accepts, not only the one you came for.

*A value that only seems to flow one way still needs a representation.* `MakePrompt` used to store a prompt as the bare string it wraps, on the reasoning that a prompt only ever reaches `PromptDeref`. That is not true. A prompt can be printed, held in a collection, stored in a struct field, passed, or returned.

So a compiled binary showed a prompt's text where the VM showed `<prompt>`, and `MakeStruct` had to refuse prompt fields outright. The fix gave a prompt a real kind through `jade_runtime::promptf`, which also meant one arm in `gc::is_collection` and one in `gc::free_obj`. Without the first, it leaked one object per prompt. Anything new that reaches a `TAG_PTR` word needs all three.

*A method's name does not prove its receiver's kind.* `chunk_val_method_supported` and `chunk_str_method_supported` pick the arm from the name, and those arms used to untag the receiver straight to a pointer of the kind the name implied.

With an untyped parameter, as in `fn f(v) { v.keys() }`, the kind is only known when `f` is called. So `v.keys()` on a string dereferenced a `char*` as a dict, and `v.upper()` on an int dereferenced a small integer.

Every arm that untags now calls `jrt_require_kind` first, or `jrt_require_str_arg` for a string method's arguments. `len` and `contains` are deliberately exempt, because they hand the whole tagged word to `jrt_len_chunk` or `jrt_in_any`, which dispatch on the tag themselves. The guard has to *raise* rather than abort, because the idiomatic type test is `try { v.keys() } catch e { … }` and nothing can catch a segfault.

*A borrowed value word must be retained at the point it becomes owned.* The runtime's collection reads hand back the value word without incrementing anything, and the *caller* is responsible for retaining it. `GetIndex` does so, with a comment saying why. The `dict.get` arm did not, so each call decremented the entry until the collection it named was freed out from under the table.

If you add a lowering that stores a word the runtime handed you, decide explicitly whether it arrived owned or borrowed. A producer such as `jrt_coll_dict_keys` hands back an owned word, while a lookup hands back a borrowed one. The two need opposite treatment, and both compile.

*A buffer a call site needs is allocated in the entry block, not at the call.* LLVM does not reclaim an `alloca` until the function returns, so a call that marshals its arguments into a fresh one grows the stack every time it runs. This backend puts calls inside loops. An FFI call in a `while` loop died at a fixed iteration count for that reason, and the count scaled exactly with `ulimit -s`. `Lowerer::entry_buf` is how a lowering asks for such a buffer. [`../codegen/README.md`](../codegen/README.md) states the rule and explains why sharing one is sound.

*The handler stack is thread-wide here, not per frame.* The VM keeps its `handlers` in a local of the dispatch call frame, so a function's handlers die with it. `jade_exc_push_frame` instead pushes onto one `_Thread_local` stack that only codegen unwinds. The emitter emits `PopHandler` solely on a try body's normal fall-through, which `try { …; return x } catch e { … }` never reaches.

So every function containing a `try` snapshots `jade_exc_depth()` in its prologue, and calls `jade_exc_restore` on every return path: an explicit `Return`, a `Halt`, and the implicit run off the end. Miss one of those paths and a dead `jmp_buf` outlives its stack frame. That shows up as a segfault, an infinite spin, or a raise landing in the wrong handler, depending on what overwrote the stack.

The linker line is `-L target/<profile> -ljade_runtime`. That only works because `jade-runtime` is a *workspace member* named in `default-members`. Cargo only copies a build artifact up into `target/<profile>/` when the crate is a requested top-level target. The root `Cargo.toml` carries the full explanation. Do not demote it back to a plain path dependency.

## Building and testing

```sh
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18   # or your install
cargo test aot:: codegen::
./target/debug/jade build examples/arithmatic/arithmetic/arithmetic.jde -o /tmp/a && /tmp/a
./target/debug/jade build file.jde --emit ir           # inspect the IR
./src/scripts/backend-parity.sh                            # diff against the VM
```
