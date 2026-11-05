# `src/backend/`: the static backend

## What this subtree is

This is Jade 2.0's code generator. It turns [MIR](../mir/README.md) into an LLVM module, optimizes it, and links a binary against the C library and nothing else. Every value has a type the compiler knows, so an `int` is an `i64` in a register and a struct is a fixed layout. No compiled program reads a type tag.

```sh
./target/debug/jade build --backend=static examples/strings/unicode/unicode.jde -o unicode
otool -L unicode        # /usr/lib/libSystem.B.dylib, and nothing else
```

It sits behind `--backend=static` until it covers the whole language. The examples it covers so far are listed in `src/scripts/static-examples.list`, and `golden.sh --backend=static` holds them to the same recorded outputs as the default backend.

## Why it was built

The v1 backend lowered bytecode into calls to two runtime libraries: about 16,600 lines of Rust and C, reached through 237 functions, with a tag check on every operation. GPU inference cannot sit on top of that, and `design/v2-static-values.md` explains the rest of the reasons. This backend replaces both runtimes. What v1 called the runtime is emitted here, into each program, only where the program uses it.

On one benchmark of recursion, string building, dict inserts and a float loop, a static binary ran in 0.01 seconds against 0.31 for v1, and weighed 34 KB against 4.4 MB.

## How it works

*Support code is generated on demand.* Every helper a program needs is an internal LLVM function named `jade.*`, generated the first time something asks for it through `Cx::support`. A program that never splits a string carries no split. Code that depends on a type is generated once per type the program uses, such as releasing an array of strings (`jade.release.Array(Str)`) or printing a dict of arrays. The optimizer inlines what is small and drops what is unused.

*The C library is the only dependency.* `Cx::libc` declares the few functions used: `malloc`, `realloc`, `free`, `memcmp`, `write`, `read`, `exit`, `snprintf`, `strtod`, `qsort`, `nanosleep`, `clock_gettime`, the pthread calls that give the program a 256 MiB stack, as v1 did, and the pthread lock, condition variable and thread calls tasks use. A test pins that list.

*Object layouts.* Every heap object starts with an `i64` reference count, and a count below zero marks an immortal object such as a string literal. Strings are `{ rc, len, bytes…, 0 }`. Arrays hold their elements unboxed in a separate buffer, so growing one never moves the object. Dicts keep entries in insertion order, with an FNV-1a index past eight entries. Structs are `{ rc, type_id, fields… }`. An optional of a pointer type is the pointer, null for nil; an optional of a scalar is `{ i1, T }`. `mod.rs` has the full table.

*Reference counts are atomic*, because tasks share one heap. Releasing an object whose count reaches zero releases what it holds, through a function generated per type. A struct value may hold any descendant of its static type, so struct release switches on the object's type id.

*Text matches v1 exactly.* `print`, `str()` and f-strings all go through `jade.tostr.<type>`. Floats print the shortest digits that read back to the same value, found with `snprintf` and `strtod`, laid out without an exponent. Case mapping uses tables built from Rust's own Unicode data when the program uses `upper`, `lower` or `capitalize`.

*Classes.* A field read through a static class whose descendants keep the field at a different offset, which happens when a class is used as a child's second parent, switches on the object's class id in `func.rs`'s `field_ptr`; otherwise it is one direct load. A class's release runs its destructor chain before freeing its fields: it holds a reference while `drop` runs, gives it back, and frees the object only when that was the last one. A destructor that stored `self` keeps the object alive, and a bit in the type id word (`layout::DROPPED`) stops its destructors from running again; every type id read masks the bit off.

*Errors are values in a slot, not unwinding.* Raising puts a class value in the thread-local `jade.err`. MIR checks the slot after every operation that can raise and branches to the nearest `catch` or to the function's unwind block, which releases its locals and returns. Support code that fails calls `F::fail`, which raises and returns a zero value at once, since its caller checks next; code lowered from MIR calls `F::raise` and carries on to that check. An error that reaches the top prints `jade: <message>`, or `jade: uncaught Name` for a class with no message, and exits with status 1, as v1 did. Calls deeper than 10,000 raise `recursion limit exceeded`.

*Tasks are jobs on one queue.* A call to an `async fn` packs its arguments into a job, which is also its future, and queues it. Worker threads, made as the queue needs them up to `max_tasks()`, run jobs; one mutex and one condition variable guard the queue and every job's state. Awaiting a job that has not started runs it on the awaiting thread, so a chain of tasks each awaiting the next runs on one thread, and `set_max_tasks(1)` cannot deadlock. A job carries a free function generated for its result type, so the queue can release a future without knowing what it holds. `tasks.rs` has the layout.

## What each file does

- *`mod.rs`* holds `Cx`, the module being built, with `support`, `libc` and string literals. It also builds the module from a program, emits the C `main`, and runs optimization, object output and the link.
- *`fb.rs`* is `F`, a function-building layer over inkwell. Its methods return values directly, and `if_`, `if_else`, `while_` and `for_range` take closures. All the support code is written in it.
- *`layout.rs`* maps types to LLVM types, holds the object offsets, and reads and makes optionals.
- *`func.rs`* lowers one MIR function: locals as zeroed stack slots, statements with the ownership rules, terminators, every binary operator with its overflow and zero checks, and every support operation.
- *`rc.rs`* emits retain and the per-type release and destroy functions.
- *`text.rs`* has strings: allocation, UTF-8, comparison, every string method, and the string builder.
- *`format.rs`* has printing, `input`, the text of every type, float formatting, and the conversions `int()`, `float()`, `bool()` and `char()`.
- *`collections.rs`* has arrays and dicts.
- *`compare.rs`* has equality for every type and ordering for numbers, strings, chars and bools.
- *`unicode.rs`* builds the case tables.
- *`errors.rs`* has the error slot, raising, an error's message, the report for an uncaught error, and the `F::fail` and `F::raise` helpers.
- *`tasks.rs`* has tasks: the job layout, the queue and its workers, `await`, `join`, `wait`, `cancel`, the concurrency limit, `time.sleep`, `time.after` and the two clocks.
- *`tests.rs`* builds small programs, runs them, and checks what they print. One test checks that a program declares only C library functions.

## Who uses it

*Used by:* `aot::compile_static`, reached through `jade build --backend=static`.

*Depends on:* `mir` for its input, `inkwell` for LLVM, and the system `cc` for the link.

## Gotchas

*Never emit into a terminated block.* `F`'s branch helpers check, and `func.rs` starts a fresh block after any statement that ends one, such as a failing operation. Raw builder calls placed after a `ret` or `unreachable` produce a module that does not verify.

*Equality is structural for arrays and dicts, and identity for structs.* v1 refused `==` between two arrays at run time; the checker now accepts it, so this backend gives it a meaning. A struct is a reference, so two structs are equal when they are the same object.

*Popping an empty array is an error.* v1 returned `nil`, but an `[int]` cannot hold `nil`, so the static backend raises `pop from an empty array`.

*A new C function goes in `Cx::libc`'s table.* It panics on a name it does not know, so a missing signature shows up the first time a test builds the code that uses it. Add the name to the pinned list in `tests.rs` at the same time.

## Building and testing

```sh
cargo test backend::
./src/scripts/golden.sh --backend=static
./target/debug/jade build --backend=static --emit ir file.jde    # the IR, before optimization
```
