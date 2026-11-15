# `src/backend/libcache/`: the library cache's tests

## What this subtree is

This directory holds `tests.rs`, the tests of `../libcache.rs`, the cache of compiled std library instances that `jade build` keeps under `~/.jade/lib/<version>/<triple>/`. It is a directory only because Rust looks for a module's `tests` submodule beside it; the cache's code is one level up, and [`../README.md`](../README.md) describes it.

## Why it was built

Part 4 of `design/v2-simplify.md` compiles each library instance once per machine. What can go wrong is shared state: two builds at once, a cache directory nobody can write, an object that is damaged, or a library object whose code depended on one program's classes. Each test here builds real programs and runs them, so each of those is checked end to end rather than by reading the code.

## What each file does

- *`tests.rs`* builds programs with `libcache::compile`, each test with a cache root of its own under the system's temp directory, removed when the test ends, so no test reads or writes `~/.jade`. It checks that a second build writes no object and prints the same; that two type instances of one function get two objects; that a class-typed instance, such as `random.choice` over an array of a program's class, or `time.parts`, is not cached; that `math.sqrt` and `math.clamp` are copied into their callers; that a cache root under a file still builds, through the scratch objects; that two builds at once on threads both link; that an object that is not an object is rebuilt, and one that does not link makes the build link without the cache; that no std instance's module asks for anything only the program knows, and that each defines no mutable state, only private constants and declarations of the program's state it recorded; that a module's IR, and so its key, is the same each time it is built; that demoting an instance demotes its callers; and that a program with no library code builds in one module and makes no directory.

## Who uses it

*Used by:* `cargo test`, as `backend::libcache::tests`.

*Depends on:* `../libcache.rs` and `../libplan.rs`, and the front end, checker and MIR to turn source into a program.

## Building and testing

```sh
cargo test backend::libcache
```
