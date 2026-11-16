# `src/codegen/support/`: support code for values

## What this subtree is

This is the support code codegen writes into each program: what v1 called the runtime, emitted as internal LLVM functions named `jade.*`. A function is generated the first time something asks for it, through `Cx::support`, so a program that never splits a string carries no split, and code that depends on a type is generated once per type the program uses. Each file adds methods to `Cx`, the module being built, for one kind of value or one service.

```sh
./target/debug/jade build --emit ir examples/strings/unicode/unicode.jde | grep 'define internal' | head
```

## Why it was built

A compiled Jade program links the C library and nothing else, so everything a value does, from joining two strings to growing an array, has to be written into the program. Until 2.0.11 these files sat beside the core of codegen in one directory of thirty files; they moved here so the top of `src/codegen/` is how MIR becomes LLVM, and this directory is what values need.

## What each file does

- *`text.rs`* has strings: allocation, UTF-8, comparison, every string method, and the string builder.
- *`unicode.rs`* builds the case tables.
- *`bytes.rs`* has bytes: indexing, `decode` with strict UTF-8 checking, lossy decoding for command and network output, `slice`, `encode`, `std::bytes`, and the `b"…"` form `print` shows.
- *`collections.rs`* has arrays and dicts.
- *`compare.rs`* has equality for every type and ordering for numbers, strings, chars and bools.
- *`format.rs`* has printing, `input`, the text of every type, float formatting, and the conversions `int()`, `float()`, `bool()` and `char()`. Since 2.0.7 a string inside an array or a dict is written quoted and escaped as JSON writes it, by `jade.str.quote`, as dict keys are, so `print(d)` and `toString(d)` give JSON; a string on its own is its text.
- *`sized.rs`* has the sized number types of `design/v2-number-types.md`: `+ - * / %` at each width with the signed or unsigned overflow intrinsic, unsigned division, remainder and right shift, shifts checked against the width, negation, and `convert`, which every converter where a sized type is on either side uses and which raises `300 does not fit a byte, which holds 0 to 255`. A sized value is held at its own width, `i8` to `i64` or `f32`, by `layout::llty`; `format.rs` prints a `ulong` past the int's range and a `float32` as the fewest digits that read back as the same float32.
- *`todict.rs`* has `toDict(text)`: one reader per dict type a program reads, `jade.todict.<type>`, with a function below it for a value of each type the dict holds, nested to any depth, and one for an object or an array of each. A dict of strings takes every value, a JSON string unescaped and anything else as the exact text the input wrote, using `jade.todict.skip`, which reads past a value checking its grammar and building nothing. Any other value type raises on a value that does not fit, `null` included. The scanner is the one the JSON package had until 2.0.7, which followed serde_json: its grammar, numbers, escapes, depth limit of 128 and line and column positions. Keys and values take the text's trust. The checker picks the value type and MIR's `Op::ToDict` carries it here; part 2 of `design/v2-simplify.md` has the rules.
- *`mod.rs`* declares the files. Everything here is a method on `Cx`, so nothing is reached by path. Tasks were `tasks.rs` here until 2.0.13 and are [`../tasks/`](../tasks/README.md) now.

## Who uses it

*Used by:* `../func/mod.rs`, which calls these methods for every MIR operation on a value, and `../sys/`, whose hooks build strings and arrays with them.

*Depends on:* `../func/builder.rs` for `F`, `../memory/layout.rs` for the object offsets, `../memory/rc.rs` for retain and release, `../func/errors.rs` for raising, and `../libcache/shared.rs` for any mutable state, which must go through `Cx::state`.

## Gotchas

*Make mutable state with `Cx::state`.* The task queue and its lock are made there, so a cached library object declares them and the program defines them once. A global made with `add_global` would be defined in every object that uses it.

## Building and testing

```sh
cargo test codegen::
./compiler/src/scripts/golden.sh
```
