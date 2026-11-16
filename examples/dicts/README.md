# `examples/dicts/`: dicts, and JSON read into them

## What this subtree is

The fixtures for dicts: building and reading one, what a dot means on one, a dict of dicts read repeatedly, and since 2.0.7, JSON text read into a dict with `toDict(text)` and written out with `toString(d)`. Each runnable program is built and run by `compiler/src/scripts/golden.sh`, which compares what the binary prints with the `.expected` file beside it. Each `*_error.jde` must fail `jade check`.

## Why it was built

A dict maps string keys to one kind of value, and v2 settles that kind when the program is checked. Most of these fixtures pin how the rest of the language meets that rule. The JSON fixtures pin part 2 of `design/v2-simplify.md`: JSON is a text format, not a type, so `std::json` and its `Json` value were removed and a dict holds what a JSON object holds. The dict's value type comes from how the program uses it, and it is `str` when nothing says otherwise, so mixed values never fail on their own.

## What each fixture does

- *`basic/basic.jde`* covers building a dict, reading and writing keys, `len`, and a few of its methods.
- *`dot_access/dot_access.jde`* reads a class field with a dot and by name in brackets. *`dot_access_error.jde`* pins the refusal of a dot on a dict, with the message naming brackets.
- *`nested_get/nested_get.jde`* reads a module-level dict of dicts through `.get()` until a missing retain would double-free the inner dict; see `examples/README.md` for the bug it pins.
- *`to_dict/to_dict.jde`* shows `toDict`'s rules: a dict of strings that takes every value as the text the input wrote, `toInt` converting at the use without settling the type, `null` as the text `"null"`, a nested object read as text and passed to `toDict` again, a dict of ints settled by `+ 1`, a value that does not fit and a `null` that does not, nested objects read as dicts by indexing into them, malformed text with its line and column, a key written twice, and `toString` writing a dict as JSON with its strings quoted and escaped.
- *`malformed/malformed.jde`* was `json/parse` until 2.0.7. It reads good text, then bad, then good again, so a failure left behind would show, and checks that an array at the top level is refused.
- *`json_removed_error.jde`* writes `use std::json`, which is refused with a message naming `toDict` and `toString`.
- *`to_dict_type_error.jde`* calls `toDict(5)`, refused because `toDict` reads JSON text, a str.

## Who uses it

`compiler/src/scripts/golden.sh` runs every program here, and `compiler/src/cli/check.rs` asserts each one checks and each `*_error.jde` does not. The code they exercise is `compiler/src/typechecker/infer/builtins.rs`, which decides what a `toDict` dict holds, and `compiler/src/codegen/support/todict.rs` and `format.rs`, which read and write it. See [`examples/README.md`](../README.md) for the rules a fixture has to follow.
