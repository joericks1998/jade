# `examples/collections/`: arrays and dicts together

## What this subtree is

Fixtures for what arrays and dicts share: how each behaves when a function writes to it, edge cases printed rather than merely computed, the methods and their package spellings, and a structure that points back at itself. Each runnable program is built and run by `compiler/src/scripts/golden.sh`, which compares what the binary prints with the `.expected` file beside it. Each `*_error.jde` must fail `jade check`.

## Why it was built

Most of these pin a bug the two engines of v1 disagreed on, and each fixture's opening comment names it. They stay because the same code paths still exist in the static backend.

## What each fixture does

- *`container_semantics/`* shows that a dict is a value while an array and a class are references, so a function's write reaches the caller for two of the three.
- *`edge_cases/`* prints arrays and dicts at their edges: empty, nested, and holding strings.
- *`join_and_lines/`* covers `join` and `lines`, the two methods that build a collection from text or text from a collection.
- *`method_receivers/`* checks that a method checks its receiver, and *`method_type_guard/`* that a method on the wrong kind of value is refused, with `method_type_guard_error.jde` beside it.
- *`two_spellings/`* calls every collection function both ways, `a.map(f)` and `array.map(a, f)`.
- *`self_reference/`* builds a cycle through a class field, and prints ordinary nesting, written out with `toString` since 2.0.7 replaced `json.stringify`. `self_reference_error.jde` pins the refusal of an array pushed into itself.
- *`dict_contains_error.jde`* and *`int_len_error.jde`* pin two refusals whose messages name the fix.

Since 2.0.7 a string inside an array or a dict prints quoted, as JSON writes it, so `edge_cases`, `join_and_lines`, `method_type_guard` and `two_spellings` record `["a", "b"]` where they recorded `[a, b]`.

## Who uses it

`compiler/src/scripts/golden.sh` runs every program here, and `compiler/src/cli/check.rs` asserts each one checks and each `*_error.jde` does not. The printing they pin is in `compiler/src/backend/format.rs`, and the methods in `compiler/src/backend/collections.rs`. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
