# `src/builtins/`: the built-in registry

## What this subtree is

This is the table of contents for everything a Jade program can call without writing it. It owns three things: the `Package` struct, the `PACKAGES` table listing every `std/*` package, and the lookups the type checkers and MIR lowering use to ask what exists.

The packages themselves are *flat top-level modules*, such as `src/array/`, `src/bytes/`, and `src/string/`, rather than children of this directory. Each one declares its function names, and for a primitive's methods their names. None of them runs anything. `typechecker/infer/builtins.rs` gives every built-in call its static type, `mir/lower/packages.rs` and `mir/lower/ops.rs` lower it to a MIR operation, and a file in `src/codegen/support/` emits the code, into a program only when that program uses it: `bytes.rs` for `std::bytes`, `tasks.rs` for the task controls, and so on.

Since 2.0.8 every other std package is written in Jade: `std::path`, `std::time`, `std::math`, `std::env`, `std::fs`, `std::sh`, `std::random`, `std::http` and `std::uhttp` are each a `.jde` module in [`src/lib/`](../lib/README.md), whose registry is `crate::stdlib`. Such a package is not in `PACKAGES`, and nothing about it is written here: its functions are typed from their bodies and compile like a program's own. What is left in `PACKAGES` is the value types' own packages, `string`, `array`, `dict` and `bytes`, whose functions are the values' methods, and `__infer`, the inference prelude's. `is_std_package`, `std_global_name` and `std_functions` answer for both kinds, for the per-file pass, which binds a package's name and refuses a function it does not have, and the import walk, which treats both as built in.

Until v1.4.8 each package also held a second implementation for the bytecode VM, as `BuiltinFn` values over `VmValue`. v2 removed the VM, and with it those bodies, their tests, and the `seed_globals`, `NativeFnId` and `PrimType` machinery that served it. What is left is names. Until 2.0.6 each package also registered its name, and each primitive its method types, with the per-file pass; that pass no longer types anything, and binds a package's `global_name` itself when a `use` names it.

## Every function has two spellings

`string.upper(s)` and `s.upper()` are the same function. A package function whose first argument is the receiver has to work written either way.

Until v1.3.21 several did not. `a.map(f)` existed nowhere at all, and `string.upper(s)` refused to compile as an *unsupported module call*. Nothing was missing from the runtime, because the symbol the package form needs is the one the method form already called. So the fix routed one spelling to the other rather than writing anything twice.

Two things to keep in mind when adding a function:

- *Register both spellings.* A package entry goes in the package's `fns` list. The method goes in the package's method list, such as `string::STR_METHODS`, and needs a declared arity in `primitive_method_arity`. `package_call` in `typechecker/infer/builtins.rs` decides which package calls are the method written receiver-first, and MIR lowers both spellings the same way, so a name missing from that list is treated as a package function of its own.
- *Say so if the two differ.* `std/array` is the exception on purpose, and it is why the checker names the receiver-first functions one by one rather than assuming every receiver-first call is the method. `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place, so routing one to the other would be a silent miscompile, not a build error.

## What each file does

- *`mod.rs`* holds the `Package` struct, `PACKAGES`, `primitive_method_arity`, `primitive_method_tables`, `global_names`, the package lookups, the lookups that answer for both kinds of std package, and `register_core_names`, which binds the globals in the per-file pass's name table.
- *`tests.rs`* holds the registry tests, including the tripwire that every primitive method declares an arity.

## The packages it registers

| Module | Import name | Notes |
|---|---|---|
| `src/core/` | *(globals)* | The names every program has without an import, in `GLOBAL_NAMES`: `print`, `len`, `write`, `input`, the task controls, the type functions and their converters, `toDict` among them, and `Grammar`. |
| `src/string/` | `std/string` | Also supplies the `str` primitive methods, such as `upper`, `split`, and `trim`. |
| `src/array/` | `std/array` | Its package functions take the *functional* style, so `array.sort(a)` returns a sorted copy while `a.sort()` sorts in place. |
| `src/dict/` | `std/dict` | Also supplies the `dict` primitive methods. |
| `src/future/` | *(none)* | Supplies the `future` primitive methods, `ready` and `cancel`. No package form: see that module's README for why. |
| `src/bytes/` | `std/bytes` | Also supplies the `bytes` primitive methods. |
| `src/infer/` | `std/__infer` | The inference prelude's own package, which only the compiler's Jade can name. |
| `src/stdio/` | *(internal)* | Not a Jade package. It provides stdout writes that survive a closed pipe, so `jade check app.jde \| head -3` does not panic. |

`std/math`, `std/path` and `std/time` left this table in 2.0.8, when each became a `.jde` module in `src/lib/`, and their directories went with them. `std/env`, `std/fs`, `std/sh`, `std/random`, `std/http` and `std/uhttp` followed in the same release, with `src/env/`, `src/fs/`, `src/sh/`, `src/random/`, `src/http/` and `src/uhttp/`. Their records, `HttpResponse`, `HttpBytesResponse` and `ShOutput`, are classes their `.jde` declares, and no longer built into every program.

`src/json/` and `std/json` were removed in 2.0.7: JSON is read with `toDict(text)` and written with `toString(d)`, both globals. A `use std::json` is refused by the per-file pass in `typechecker/resolve.rs`, which names the two.

## Who uses it

*Depends on:* `typechecker/resolve` for `Names`, the table `register_core_names` fills.

*Used by:* `typechecker/resolve.rs`, which binds the global names and a package's name, and checks `fs.read`-style field access against a package's functions, of either kind. `typechecker/infer/`, the static checker, which reads `all_packages` to know every package name and the method lists, such as `string::STR_METHODS`, to know which methods a primitive has. `mir/lower/mod.rs`, which reads `all_packages` to tell a package name from a value. `project/imports.rs` and `mir/imports.rs`, which tell a `std/*` import from a file import.

## Adding a built-in

Write it in Jade if it can be: a function in a package's `.jde` in `src/lib/`, with a hook for what Jade cannot do. `src/lib/README.md` says how. What follows is for a function the backend writes.

1. Add its name to the package's `fns` list, or to `core::GLOBAL_NAMES` for a global.
2. *Type it, lower it, and emit it, in the same change.* A package function gets its static type in `package_call` in `typechecker/infer/builtins.rs`, its MIR operation in `mir/lower/packages.rs`, and its code in the matching file in `src/codegen/support/`. A bare global goes in `is_builtin_value` and `builtin_result` in the same checker file, and in `builtin` in `mir/lower/ops.rs`. A name with no checker arm is refused as `std/<pkg> has no function '<name>'`, and one with no MIR arm stops with `jade cannot compile std/<pkg> yet`. `jade check` runs both, so either gap shows up there rather than when a program tries to ship.
3. *Write an `examples/` fixture that exercises it, with its `.expected` output*, so `src/scripts/golden.sh` runs it. A builtin no fixture touches looks fine to every test in the repo, which is how `write` and `uhttp.stream` stayed broken in compiled programs until v1.1.34.

Adding a whole package also needs `pub mod <name>;` in `src/lib.rs`, a `use crate::<name>;` here, and an entry in `PACKAGES`.

## Building and testing

```sh
cargo test builtins::
```
