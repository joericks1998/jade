# `examples/path/`: `std::path`

## What this subtree is

The fixtures for `std::path`, which treats a path as text the way Rust's `std::path::Path` does on Unix. Since 2.0.8 the package is written in Jade, `compiler/src/lib/path.jde`, and these fixtures were checked against the backend version it replaced, which printed the same.

## What each file does

- *`components/`* runs every function over the edge cases: an empty path, the root, doubled and trailing slashes, a leading `./`, the `.` and `..` components, a dot that starts a name, and a name that is not ASCII. It joins two paths and more, makes paths absolute without printing anything that depends on the machine, shows that a result keeps the trust of a path from outside the program, and passes a `str?` the way `env.get` returns one.
- *`segment_type_error.jde`* gives `path.join` an int, which the checker refuses at the call, naming the library function.

## Who uses it

`compiler/src/scripts/golden.sh` builds `components/` and compares its output with `components.expected`, and the `every_example_matches_its_expected_check_result` test in `compiler/src/cli/check.rs` holds `segment_type_error.jde` to failing `jade check`.
