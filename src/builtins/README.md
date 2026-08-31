# `src/builtins/`: the native built-in registry

## What this subtree is

This is the registry that makes Rust functions callable from Jade. It owns four things: the shared types `BuiltinFn`, `Package`, `PrimType`, and `NativeBoundMethod`; the `PACKAGES` table listing every `std/*` package; the `CORE_BUILTINS` list of always-available globals; and `seed_globals`, which fills in a fresh `VmState`.

The packages themselves are *flat top-level modules*, such as `src/array/`, `src/math/`, and `src/string/`, rather than children of this directory. This file is the table of contents, and each of those modules is one entry in it.

## Every function has two spellings

`string.upper(s)` and `s.upper()` are the same function. A package function whose first argument is the receiver has to work written either way, on both engines.

Until v1.3.21 several did not. `a.map(f)` existed nowhere at all. `string.upper(s)` and `dict.keys(d)` ran under `jade run`, while `jade build` refused them as an *unsupported module call*. Nothing was actually missing from the runtime in either direction, because the symbol the package form needs is the one the method form already called. So the fix routed one spelling to the other rather than writing anything twice.

Two things to keep in mind when adding a function:

- *Register both spellings.* A package entry goes in the package's `fns` table. The method goes in the primitive-method table *and* in `register_*_method_types`, or the type checker will not know about it. `codegen` reads the package table to decide which package calls lower as the method, so a name missing from that table silently keeps the old behavior.
- *Say so if the two differ.* `std/array` is the exception on purpose, and it is why the codegen bridge asks the package table for a name rather than assuming every receiver-first call is the method. Routing `array.sort` to the in-place symbol would have made a compiled program mutate an array the interpreter leaves alone. That is a silent miscompile, not a build error.

## Why it was built this way

The registry is a boundary, and the value of a boundary is that crossing it is cheap and nothing leaks through. Adding a package should touch the package's own files plus one line here. If a new package needs anything *else* changed, the boundary has sprung a leak, and that is worth fixing rather than working around.

A `BuiltinFn` is pure. Its signature is `fn(&[VmValue]) -> Result<VmValue>`, with no access to `VmState`. Some functions genuinely need state, such as the inference backend, the token budget, or the ability to call back into Jade code. Those are declared instead as `natives: &[(&str, NativeFnId)]` on the package, so the VM dispatches them by id.

That list used to be a hand-maintained override inside the VM, and `llm` fared badly under it. All ten of its functions were stateful, so each had to be declared three times. Listing the ids on the package makes them the package's own business.

## What each file does

- *`mod.rs`* holds the whole registry: `BuiltinFn`, `NativeBoundMethod`, `PrimType`, the `Package` struct, `PACKAGES`, `CORE_BUILTINS`, `seed_globals`, `find_primitive_method`, and the type-registration hooks.
- *`tests.rs`* holds the registry tests.

## The packages it registers

Each package is a sibling top-level module with a `mod.rs` and a `tests.rs`. Most are thin `VmValue` marshalling over a shared core in `jade-runtime`, such as `src/math/` over `mathf.rs` and `src/fs/` over `fsf.rs`. That is how the AOT backend gets the same behavior, through the `jrt_*` symbols.

| Module | Import name | Notes |
|---|---|---|
| `src/core/` | *(globals)* | `write`, `len`, and `input`. `print` and `route` are stateful and go through `NativeFnId`. `stream` was a third until v1.2.5 removed it. |
| `src/string/` | `std/string` | Also supplies the `str` primitive methods, such as `upper`, `split`, and `trim`. |
| `src/array/` | `std/array` | `map` and `filter` are stateful, because they call a user function once per element. Its package functions take the *functional* style, so `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place. |
| `src/dict/` | `std/dict` | Also supplies the `dict` primitive methods. |
| `src/math/` | `std/math` | |
| `src/json/` | `std/json` | |
| `src/fs/` | `std/fs` | Output is tainted (see `jade_runtime::trust`). |
| `src/future/` | *(none)* | Supplies the one `future` primitive method, `ready`. No package form: see that module's README for why. |
| `src/path/` | `std/path` | |
| `src/env/` | `std/env` | |
| `src/time/` | `std/time` | |
| `src/random/` | `std/random` | |
| `src/sh/` | `std/sh` | Refuses tainted command strings. |
| `src/http/` | `std/http` | TCP HTTP; returns `{status, body}`. |
| `src/uhttp/` | `std/uhttp` | The same API over a Unix socket, addressed as `unix://<sock>:<path>`. `uhttp.stream` is stateful. Its reader is shared, as `jade_runtime::uhttpf::Stream`, and it compiles on both engines. |
| `src/grammar/` | *(global)* | `Grammar.new(pattern)`, which builds GBNF sampling constraints for a typed prompt dereference. |
| `src/stdio/` | *(internal)* | Not a Jade package. It provides stdout writes that survive a closed pipe, so `jade run app.jde \| head -3` does not panic. |

## Who uses it

*Depends on:* `vm/` for `VmValue` and `NativeFnId`, `compiler/type_infer` for `TypeContext`, and `jade-runtime` for the shared implementations.

*Used by:* `vm/state.rs`, which calls `seed_globals` on every fresh state. `compiler/type_infer.rs` calls the type-registration hooks, so built-ins are visible to inference. `vm/call.rs` dispatches both `BuiltinFn` and `NativeFnId` calls.

## Adding a built-in

1. Write the `BuiltinFn` constant in the package's `mod.rs`.
2. Add it to that package's `fns` slice, or to `CORE_BUILTINS` for a global.
3. Add its type in the package's `register_types`.
4. *Lower it in `src/codegen/`, in the same change.* Steps 1 through 3 only teach the VM. A builtin the interpreter has and the AOT backend does not is not a half-finished feature. It is the two engines disagreeing about what the language is, and the program does not find out until `jade build`, long after it was written and tested under `jade run`.

   A module function goes in `chunk_module_supported` and `emit_module_call`. A bare global goes in `LOWERABLE_BUILTINS` and the `call_builtins` dispatch. If it needs shared logic, put that logic in `jade-runtime`, so both engines call one implementation rather than two that can drift.
5. *Write an `examples/` fixture that exercises it*, so `src/scripts/backend-parity.sh` runs it on both engines. This step is what makes step 4 enforce itself. A builtin no fixture touches looks fine to every test in the repo, which is exactly how `write` and `uhttp.stream` stayed interpreter-only until v1.1.34.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

If the function needs `VmState`, give it a `NativeFnId` variant in `vm/value.rs`, a match arm in `vm/call.rs`, and an entry in the package's `natives` list instead.

Needing `VmState` does *not* excuse step 4. `uhttp.stream` is a `NativeFnId` and it compiles. A runtime helper can call a Jade function value directly, because the value's box holds the raw pointer at offset 0, which is what `jrt_coll_array_map` does. So "it calls back into Jade" is not a reason a builtin cannot be compiled.

## Building and testing

```sh
cargo test builtins:: string:: array:: math::    # etc, per package
```
