# `examples/control_flow/`: branches and loops

## What this subtree is

These are the fixtures for `if`, `elif`, `else`, `while`, `break` and `continue`. `for` has its own directory, `for_loop/`. Since 2.0.10 the condition of an `if`, an `elif` or a `while` is one parenthesized expression followed by the block's `{`, as in `if (x > 0) {`, and every fixture here is written that way.

## Why it exists

Control flow is in nearly every other fixture too, but these pin each construct on its own, with output recorded beside it, so a change to how one is parsed, checked or compiled shows up here first.

## What each file does

- *`if_else/`*, *`elif/`* and *`nested_if/`* cover branching: one test, an `elif` chain, and an `if` inside an `else`.
- *`while_basic/`*, *`while_accumulate/`*, *`while_in_fn/`* and *`while_nested/`* cover loops: counting, summing, a loop inside a function, and one loop inside another.
- *`break_continue/`* leaves a `for`, leaves a `while (true)`, and leaves only the innermost of two nested loops.
- *`conditions/`* pins the parentheses 2.0.10 requires. `conditions.jde` shows each form, a condition holding parentheses of its own, a loop condition, and a class literal inside the parentheses, which a bare condition could not hold. Four fixtures beside it must fail `jade check`: `bare_if_error.jde`, `bare_elif_error.jde` and `bare_while_error.jde` write a condition with no parentheses, and `partial_parens_error.jde` writes `if (a) and (b) {`, whose parentheses wrap only part of the condition. Each error names the fix and says that `jade fmt` adds the parentheses.

Because `jade fmt` would add those parentheses, CI's `jade fmt --check` skips the four `*_error.jde` files in `conditions/`. `vscode/test/` allows them too, since a file named `*_error.jde` may hold removed syntax.

## Who uses it

`compiler/src/scripts/golden.sh` builds each runnable fixture and compares its output with its `.expected` file, and `cli/check.rs`'s fixture test and CI check that each `*_error.jde` fails. `compiler/src/cli/tests.rs` checks that `jade fmt` leaves every fixture here unchanged, apart from those four.

## Running them

```sh
./target/debug/jade run examples/control_flow/conditions/conditions.jde
./target/debug/jade check examples/control_flow/conditions/bare_if_error.jde   # must fail
./compiler/src/scripts/golden.sh
```
