# `src/builtins/`: the built-in registry

## What this subtree is

This is the table of contents for everything a Jade program can call without writing it. It owns three things: the `Package` struct, the `PACKAGES` table listing every `std/*` package, and the lookups the type checkers and MIR lowering use to ask what exists.

The packages themselves are *flat top-level modules*, such as `src/array/`, `src/math/`, and `src/string/`, rather than children of this directory. Each one declares its function names and registers their types with the checker. None of them runs anything. `typechecker/infer/builtins.rs` gives every built-in call its static type, `mir/packages.rs` and `mir/ops.rs` lower it to a MIR operation, and a file in `src/backend/` emits the code, into a program only when that program uses it: `math.rs` for `std::math`, `fs.rs` for `std::fs`, `tasks.rs` for the task controls, and so on.

Until v1.4.8 each package also held a second implementation for the bytecode VM, as `BuiltinFn` values over `VmValue`. v2 removed the VM, and with it those bodies, their tests, and the `seed_globals`, `NativeFnId` and `PrimType` machinery that served it. What is left is names and types.

## Every function has two spellings

`string.upper(s)` and `s.upper()` are the same function. A package function whose first argument is the receiver has to work written either way.

Until v1.3.21 several did not. `a.map(f)` existed nowhere at all, and `string.upper(s)` refused to compile as an *unsupported module call*. Nothing was missing from the runtime, because the symbol the package form needs is the one the method form already called. So the fix routed one spelling to the other rather than writing anything twice.

Two things to keep in mind when adding a function:

- *Register both spellings.* A package entry goes in the package's `fns` list. The method goes in the package's method list, such as `string::STR_METHODS`, *and* in `register_*_method_types`, or the type checker will not know about it. `package_call` in `typechecker/infer/builtins.rs` decides which package calls are the method written receiver-first, and MIR lowers both spellings the same way, so a name missing from that list is treated as a package function of its own.
- *Say so if the two differ.* `std/array` is the exception on purpose, and it is why the checker names the receiver-first functions one by one rather than assuming every receiver-first call is the method. `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place, so routing one to the other would be a silent miscompile, not a build error.

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
| `src/fs/` | `std/fs` | Output is tainted (see `src/backend/fs.rs`). |
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

*Depends on:* `typechecker/type_infer` for `TypeContext`.

*Used by:* `typechecker/type_infer.rs`, which calls the type-registration hooks so built-ins are visible to inference and checks `math.round`-style field access against a package's `fns`. `typechecker/infer/`, the static checker, which reads `all_packages` to know every package name and the method lists, such as `string::STR_METHODS`, to know which methods a primitive has. `mir/lower.rs`, which reads `all_packages` to tell a package name from a value. `project/imports.rs` and `aot/imports.rs`, which tell a `std/*` import from a file import.

## Adding a built-in

1. Add its name to the package's `fns` list, or to `core::GLOBAL_NAMES` for a global.
2. Add its type in the package's `register_types`.
3. *Type it, lower it, and emit it, in the same change.* A package function gets its static type in `package_call` in `typechecker/infer/builtins.rs`, its MIR operation in `mir/packages.rs`, and its code in the matching file in `src/backend/`. A bare global goes in `is_builtin_value` and `builtin_result` in the same checker file, and in `builtin` in `mir/ops.rs`. A name with no checker arm is refused as `std/<pkg> has no function '<name>'`, and one with no MIR arm stops with `jade cannot compile std/<pkg> yet`. `jade check` runs both, so either gap shows up there rather than when a program tries to ship.
4. *Write an `examples/` fixture that exercises it, with its `.expected` output*, so `src/scripts/golden.sh` runs it. A builtin no fixture touches looks fine to every test in the repo, which is how `write` and `uhttp.stream` stayed broken in compiled programs until v1.1.34.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

## Building and testing

```sh
cargo test builtins::
```
