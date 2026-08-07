# `src/builtins/` — the native built-in registry

## What this subtree is

The registry that makes Rust functions callable from Jade. It owns the shared types (`BuiltinFn`, `Package`, `PrimType`, `NativeBoundMethod`), the `PACKAGES` table listing every `std/*` package, the `CORE_BUILTINS` list of always-available globals, and `seed_globals`, which populates a fresh `VmState`.

The packages themselves are *flat top-level modules* — `src/array/`, `src/math/`, `src/string/`, and so on — rather than children of this directory. This file is the table of contents; each of those is one entry.

## Why it was built this way

The registry is a boundary, and the value of a boundary is that crossing it is cheap and nothing leaks through. Adding a package should touch the package's own files plus one line here. If a new package needs anything *else* changed, the boundary has sprung a leak, and that is worth fixing rather than working around.

A `BuiltinFn` is pure: `fn(&[VmValue]) -> Result<VmValue>`, no `VmState` access. Some functions genuinely need state — the inference backend, the token budget, the ability to call back into Jade code — and those are declared as `natives: &[(&str, NativeFnId)]` on the package instead, so the VM dispatches them by id. That list used to be a hand-maintained override inside the VM, and `llm` fared badly under it: every one of its ten functions was stateful, so each had to be declared three times. Listing the ids on the package makes it the package's own business.

## What each file does

- **`mod.rs`** — the whole registry. `BuiltinFn`, `NativeBoundMethod`, `PrimType`, the `Package` struct, `PACKAGES`, `CORE_BUILTINS`, `seed_globals`, `find_primitive_method`, and the type-registration hooks.
- **`tests.rs`** — registry tests.

## The packages it registers

Each is a sibling top-level module with a `mod.rs` and a `tests.rs`. Most are thin `VmValue` marshalling over a shared core in `jade-runtime` — `src/math/` over `mathf.rs`, `src/fs/` over `fsf.rs`, and so on — so the AOT backend gets the same behavior through the `jrt_*` symbols.

| Module | Import name | Notes |
|---|---|---|
| `src/core/` | *(globals)* | `write`, `len`, `input`. `print` and `route` are stateful and go through `NativeFnId`. (`stream` was a third until v1.2.5 removed it.) |
| `src/string/` | `std/string` | Also supplies the `str` primitive methods (`upper`, `split`, `trim`, …). |
| `src/array/` | `std/array` | `map` and `filter` are stateful — they call a user function per element. |
| `src/dict/` | `std/dict` | Also supplies the `dict` primitive methods. |
| `src/math/` | `std/math` | |
| `src/json/` | `std/json` | |
| `src/fs/` | `std/fs` | Output is tainted (see `jade_runtime::trust`). |
| `src/path/` | `std/path` | |
| `src/env/` | `std/env` | |
| `src/time/` | `std/time` | |
| `src/random/` | `std/random` | |
| `src/sh/` | `std/sh` | Refuses tainted command strings. |
| `src/http/` | `std/http` | TCP HTTP; returns `{status, body}`. |
| `src/uhttp/` | `std/uhttp` | Same API over a Unix socket, addressed as `unix://<sock>:<path>`. `uhttp.stream` is stateful; its reader is shared (`jade_runtime::uhttpf::Stream`) and it compiles on both engines. |
| `src/grammar/` | *(global)* | `Grammar.new(pattern)` — GBNF sampling constraints for typed prompt derefs. |
| `src/stdio/` | *(internal)* | Not a Jade package. Stdout writes that survive a closed pipe, so `jade run app.jde \| head -3` does not panic. |

## Who uses it

*Depends on:* `vm/` for `VmValue` and `NativeFnId`, `compiler/type_infer` for `TypeContext`, and `jade-runtime` for the shared implementations.

*Used by:* `vm/state.rs` calls `seed_globals` on every fresh state; `compiler/type_infer.rs` calls the type-registration hooks so built-ins are visible to inference; `vm/call.rs` dispatches both `BuiltinFn` and `NativeFnId` calls.

## Adding a built-in

1. Write the `BuiltinFn` constant in the package's `mod.rs`.
2. Add it to that package's `fns` slice, or to `CORE_BUILTINS` for a global.
3. Add its type in the package's `register_types`.
4. **Lower it in `aot/lower.rs`, in the same change.** Steps 1–3 only teach the VM. A builtin the interpreter has and the AOT backend does not is not a half-finished feature, it is the two engines disagreeing about what the language is — and the program does not find out until `jade build`, after it was written and tested under `jade run`. A module function goes in `chunk_module_supported` + `emit_module_call`; a bare global goes in `LOWERABLE_BUILTINS` and the `call_builtins` dispatch. If it needs shared logic, put that in `jade-runtime` so both engines call one implementation rather than two that can drift.
5. **Write an `examples/` fixture that exercises it**, so `src/scripts/backend-parity.sh` runs it on both engines. This is the step that makes 4 self-enforcing: a builtin no fixture touches looks fine to every test in the repo, which is exactly how `write` and `uhttp.stream` stayed interpreter-only until v1.1.34.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

If the function needs `VmState`, give it a `NativeFnId` variant in `vm/value.rs`, a match arm in `vm/call.rs`, and an entry in the package's `natives` list instead. Needing `VmState` does **not** excuse step 4 — `uhttp.stream` is a `NativeFnId` and compiles. A runtime helper can call a Jade function value directly (its box holds the raw pointer at offset 0, as `jrt_coll_array_map` does), so "it calls back into Jade" is not a reason a builtin cannot be compiled.

## Building and testing

```sh
cargo test builtins:: string:: array:: math::    # etc, per package
```
