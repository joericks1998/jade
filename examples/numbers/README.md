# `examples/numbers/`: numbers and `std::math`

## What this subtree is

The fixtures for Jade's numbers: the int's range and overflow, the sized number types of 2.0.4, hex literals, how a float prints, and `std::math`. Since 2.0.8 `std::math` is written in Jade, `compiler/src/lib/math.jde`, each one-instruction function a call to the LLVM intrinsic behind it; its two fixtures print what the backend version before it printed.

## What each file does

- *`int_bounds/`* and *`overflow/`* pin the ends of the int range and the `integer overflow` that arithmetic past them raises.
- *`sized/`* and *`sized_raises/`* cover `byte`, `short`, `ushort`, `int32`, `uint32`, `ulong` and `float32`, and what raises with them; `sized_literal_error.jde`, `sized_mix_error.jde` and `sized_variable_error.jde` are the refusals. `design/v2-number-types.md` has the rules.
- *`hex/`* covers hex literals, and `hex_digit_error.jde` a bad digit.
- *`float_display/`* pins how a float prints at every magnitude.
- *`math_functions/`* is the v1.3.23 fixture for every `std::math` function and constant on ordinary inputs, ties and reversed ranges included.
- *`math_edges/`* covers `std::math` at its edges: NaN, the infinities, the sign of zero, ints at the ends of their range kept as ints, and the errors `abs` and `pow` raise.

## Who uses it

`compiler/src/scripts/golden.sh` builds each fixture directory and compares its output with its `.expected` file, and the `every_example_matches_its_expected_check_result` test in `compiler/src/cli/check.rs` holds each `*_error.jde` to failing `jade check`.
