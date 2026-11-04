# `src/builtins/`: the built-in registry

## What this subtree is

This is the table of contents for everything a Jade program can call without writing it. It owns three things: the `Package` struct, the `PACKAGES` table listing every `std/*` package, and the lookups the type checker and the compiled backend use to ask what exists.

The packages themselves are *flat top-level modules*, such as `src/array/`, `src/math/`, and `src/string/`, rather than children of this directory. Each one declares its function names and registers their types with the checker. None of them runs anything: `src/codegen/` lowers every built-in call, mostly to a `jrt_*` function in the runtime.

Until v1.4.8 each package also held a second implementation for the bytecode VM, as `BuiltinFn` values over `VmValue`. v2 removed the VM, and with it those bodies, their tests, and the `seed_globals`, `NativeFnId` and `PrimType` machinery that served it. What is left is names and types.

## Every function has two spellings

`string.upper(s)` and `s.upper()` are the same function. A package function whose first argument is the receiver has to work written either way.

Until v1.3.21 several did not. `a.map(f)` existed nowhere at all, and `string.upper(s)` refused to compile as an *unsupported module call*. Nothing was missing from the runtime, because the symbol the package form needs is the one the method form already called. So the fix routed one spelling to the other rather than writing anything twice.

Two things to keep in mind when adding a function:

- *Register both spellings.* A package entry goes in the package's `fns` list. The method goes in the package's method list, such as `string::STR_METHODS`, *and* in `register_*_method_types`, or the type checker will not know about it. `codegen` reads the package list to decide which package calls lower as the method, so a name missing from it silently keeps the old behavior.
- *Say so if the two differ.* `std/array` is the exception on purpose, and it is why the codegen bridge asks the package list for a name rather than assuming every receiver-first call is the method. `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place, so routing one to the other would be a silent miscompile, not a build error.

## What each file does

- *`mod.rs`* holds the `Package` struct, `PACKAGES`, `primitive_method_arity`, `primitive_method_tables`, `global_names`, the package lookups, and the type-registration hooks.
- *`tests.rs`* holds the registry tests, including the tripwire that every primitive method declares an arity.

## The packages it registers

| Module | Import name | Notes |
|---|---|---|
| `src/core/` | *(globals)* | The names every program has without an import, in `GLOBAL_NAMES`: `print`, `len`, `write`, `input`, the task controls, the primitive conversions and `Grammar`. |
| `src/string/` | `std/string` | Also supplies the `str` primitive methods, such as `upper`, `split`, and `trim`. |
| `src/array/` | `std/array` | Its package functions take the *functional* style, so `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place. |
| `src/dict/` | `std/dict` | Also supplies the `dict` primitive methods. |
| `src/math/` | `std/math` | |
| `src/json/` | `std/json` | |
| `src/fs/` | `std/fs` | Output is tainted (see `jade_runtime::trust`). |
| `src/future/` | *(none)* | Supplies the `future` primitive methods, `ready` and `cancel`. No package form: see that module's README for why. |
| `src/path/` | `std/path` | |
| `src/env/` | `std/env` | |
| `src/time/` | `std/time` | |
| `src/random/` | `std/random` | |
| `src/sh/` | `std/sh` | Refuses tainted command strings. |
| `src/http/` | `std/http` | TCP HTTP; returns `{status, body}`. |
| `src/uhttp/` | `std/uhttp` | The same API over a Unix socket, addressed as `unix://<sock>:<path>`, plus `uhttp.stream`. |
| `src/stdio/` | *(internal)* | Not a Jade package. It provides stdout writes that survive a closed pipe, so `jade check app.jde \| head -3` does not panic. |

## Who uses it

*Depends on:* `compiler/type_infer` for `TypeContext`.

*Used by:* `compiler/type_infer.rs`, which calls the type-registration hooks so built-ins are visible to inference and checks `math.round`-style field access against a package's `fns`. `codegen/builtins.rs`, which reads `global_names` to decide which globals a program may read unbound, and the package lists to decide which package calls lower as methods. `project/imports.rs`, which tells a `std/*` import from a file import.

## Adding a built-in

1. Add its name to the package's `fns` list, or to `core::GLOBAL_NAMES` for a global.
2. Add its type in the package's `register_types`.
3. *Lower it in `src/codegen/`, in the same change.* A name the checker accepts and the backend cannot lower is a program that type-checks and then refuses to build. A module function goes in `chunk_module_supported` and `emit_module_call`. A bare global goes in `LOWERABLE_BUILTINS` and the `call_builtins` dispatch. `every_package_fn_lowers_in_the_chunk_backend` in `codegen::tests` fails if a package name has no lowering.
4. *Write an `examples/` fixture that exercises it, with its `.expected` output*, so `src/scripts/golden.sh` runs it. A builtin no fixture touches looks fine to every test in the repo, which is how `write` and `uhttp.stream` stayed broken in compiled programs until v1.1.34.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

## Building and testing

```sh
cargo test builtins::
```
