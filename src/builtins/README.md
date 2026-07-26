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
| `src/core/` | *(globals)* | `write`, `len`, `input`. `print`, `stream`, and `route` are stateful and go through `NativeFnId`. |
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
| `src/uhttp/` | `std/uhttp` | Same API over a Unix socket, addressed as `unix://<sock>:<path>`. `uhttp.stream` is stateful. |
| `src/grammar/` | *(global)* | `Grammar.new(pattern)` — GBNF sampling constraints for typed prompt derefs. |
| `src/stdio/` | *(internal)* | Not a Jade package. Stdout writes that survive a closed pipe, so `jade run app.jde \| head -3` does not panic. |

## Who uses it

*Depends on:* `vm/` for `VmValue` and `NativeFnId`, `compiler/type_infer` for `TypeContext`, and `jade-runtime` for the shared implementations.

*Used by:* `vm/state.rs` calls `seed_globals` on every fresh state; `compiler/type_infer.rs` calls the type-registration hooks so built-ins are visible to inference; `vm/call.rs` dispatches both `BuiltinFn` and `NativeFnId` calls.

## Adding a built-in

1. Write the `BuiltinFn` constant in the package's `mod.rs`.
2. Add it to that package's `fns` slice, or to `CORE_BUILTINS` for a global.
3. Add its type in the package's `register_types`.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

If the function needs `VmState`, give it a `NativeFnId` variant in `vm/value.rs`, a match arm in `vm/call.rs`, and an entry in the package's `natives` list instead.

## Building and testing

```sh
cargo test builtins:: string:: array:: math::    # etc, per package
```
