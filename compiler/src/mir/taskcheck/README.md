# `src/mir/taskcheck/`: the task check's tests

## What this subtree is

This directory holds the tests for `mir/taskcheck.rs`, the pass that refuses tasks that race on shared state. The pass itself is the file beside this directory; Rust finds its `mod tests;` here.

## Why it was built

The task check's rules are about whole programs: a task that writes a global, writes through something it was handed, or calls a function that does, and a spawner that changes what a running task holds. A rule like that is only tested honestly by a program that goes through the real pipeline, so each test here compiles Jade source the way `jade build` does, std library packages included, and reads the check's answer.

## What each file does

- *`tests.rs`* compiles programs through the per-file pass, the import pass, the checker, MIR and the check, and asserts which are refused and how the refusal reads. It covers function values that reach a task, globals read back, methods, the spawner's own writes, and, since 2.0.8, a task that shuffles a global array with `random.shuffle`, which the check refuses now that the function is Jade it can read, naming it as the program calls it.

## Who uses it

`cargo test mir::taskcheck` runs it. Nothing else uses it.
