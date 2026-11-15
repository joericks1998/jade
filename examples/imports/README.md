# `examples/imports/`: `use` and what it can name

## What this subtree is

The fixtures for imports: a sibling file, a class from another file, a directory registered as a library in `jade.toml`, a source package, and the imports `jade check` must refuse.

## What each file does

- *`basic/`* imports a sibling file by its module name, `use math_lib`, and calls its functions through the name.
- *`class_methods/`* uses a class another file defines, with its defaults and methods.
- *`project_lib/`* reaches a directory outside the importing file's through a `[lib]` entry in `jade.toml`.
- *`source_package/`* imports a source package, and `not_exported_error.jde` there calls a function its `[package] exports` leaves out.
- *`missing_error/`* names a module that does not exist, which `jade check` has refused since v1.1.33.
- *`std_hooks_error.jde`* names `std::__sys`, the hook package the std library written in Jade calls into, which a program cannot name; it is told the name is undefined, as for `__infer`.

## Who uses it

`compiler/src/scripts/golden.sh` builds the runnable fixtures and compares their output with each `.expected` file, and the `every_example_matches_its_expected_check_result` test in `compiler/src/cli/check.rs` checks each fixture by its path, so a `use` resolves from the file that wrote it, and holds each `*_error.jde` to failing.
