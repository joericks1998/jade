# `src/codegen/libcache/`: the std library's object cache

## What this subtree is

This is the cache of compiled std library instances that `jade build` keeps under `~/.jade/lib/<version>/<triple>/`. An instance is one library function at one set of argument types. One whose types are all built in is compiled once per machine and its object kept; a later build of any program that needs the same instance links the same object. [`../README.md`](../README.md) describes the rules in full.

## Why it was built

Part 4 of `design/v2-simplify.md` compiles each library instance once per machine, so a warm build compiles only the program's own code. What can go wrong is shared state: two builds at once, a cache directory nobody can write, an object that is damaged, or a library object whose code depended on one program's classes. The tests here build real programs and run them, so each of those is checked end to end.

## What each file does

- *`mod.rs`* is the cache: it builds each library instance's module, computes its key, reads a hit or writes a miss atomically, builds the program's module with what the library modules declared, and falls back to one module when anything goes wrong before linking. It returns the objects; `build::binary` links them and retries in one module if a cached object does not link. `default_root` is `~/.jade/lib`.
- *`plan.rs`* decides how each function is compiled: with the program, in a cached object, or copied into each caller, and gives each cacheable instance its symbol.
- *`tests.rs`* builds programs with a cache root of their own under the system's temp directory, removed when the test ends, so no test reads or writes `~/.jade`. It checks that a second build writes no object and prints the same; that two type instances of one function get two objects; that a class-typed instance, such as `random.choice` over an array of a program's class, or `time.parts`, is not cached; that `math.sqrt` and `math.clamp` are copied into their callers; that a cache root under a file still builds, through the scratch objects; that two builds at once on threads both link; that an object that is not an object is rebuilt, and one that does not link makes the build link without the cache; that no std instance's module asks for anything only the program knows, and that each defines no mutable state; that a module's IR, and so its key, is the same each time it is built; that demoting an instance demotes its callers; and that a program with no library code builds in one module and makes no directory.

## Who uses it

*Used by:* `../mod.rs`, whose `objects` calls `objects` here when given a cache root, and which builds the program's module from the plan. `build::binary` reads the `Report` to warn about a damaged cache.

*Depends on:* `../shared.rs` for what a library module declares and the program defines, `../func.rs` to lower each instance, and `sha2` for keys and symbols.

## Building and testing

```sh
cargo test codegen::libcache
```
