# `src/backend/`: the code generator

## What this subtree is

This is Jade 2.0's code generator. It turns [MIR](../mir/README.md) into an LLVM module, optimizes it, and links a binary against the C library and nothing else. Every value has a type the compiler knows, so an `int` is an `i64` in a register and a struct is a fixed layout. No compiled program reads a type tag.

```sh
./target/debug/jade build examples/strings/unicode/unicode.jde -o unicode
otool -L unicode        # /usr/lib/libSystem.B.dylib, and nothing else
```

It is the only backend. It sat behind `--backend=static` while it grew, and became the only one in phase 6 of `design/v2-static-values.md`, when v1's backend and both of its runtimes were deleted. `golden.sh` holds every example to the outputs v1.4.8 printed. Prompts do not compile yet; they return with the Metal inference kernel.

## Why it was built

The v1 backend lowered bytecode into calls to two runtime libraries: about 16,600 lines of Rust and C, reached through 237 functions, with a tag check on every operation. GPU inference cannot sit on top of that, and `design/v2-static-values.md` explains the rest of the reasons. This backend replaced both runtimes. What v1 called the runtime is emitted here, into each program, only where the program uses it.

On one benchmark of recursion, string building, dict inserts and a float loop, a static binary ran in 0.01 seconds against 0.31 for v1, and weighed 34 KB against 4.4 MB.

## How it works

*Support code is generated on demand.* Every helper a program needs is an internal LLVM function named `jade.*`, generated the first time something asks for it through `Cx::support`. A program that never splits a string carries no split. Code that depends on a type is generated once per type the program uses, such as releasing an array of strings (`jade.release.Array(Str)`) or printing a dict of arrays. The optimizer inlines what is small and drops what is unused.

*The linker is inside the compiler.* A program is linked in-process by LLD, the linker LLVM ships, built from the same LLVM 18 install. The only thing a machine needs to link a program is the C library itself: `libSystem` on macOS, whose symbols `link.rs` lists in a stub it writes, and on Linux glibc's `libc.so.6`, which every glibc system has, or on a system without glibc, such as Alpine, musl's loader, which is its C library too. So `jade build` works with no `cc`, no Xcode tools and no development packages; a binary is ad hoc signed by LLD, as Apple Silicon requires.

*The C library is the only dependency.* `Cx::libc` declares the functions used, and a test pins the list: memory, `write` and `read`, `snprintf` and `strtod`, the pthread calls behind the 256 MiB main stack and tasks, the clocks, the math library, `fopen` and the directory calls for `std::fs`, and `posix_spawn` with pipes for `std::sh` and `curl`. A few have a different symbol on some platform, such as `errno`'s accessor or Intel macOS's `stat$INODE64`; `libc_symbol` maps them. A `struct stat` or `struct dirent` field is read at an offset chosen per target in `fs.rs`. A program that calls a native package also uses `dlopen` and its siblings.

*Native packages speak v1's `JadeVal` boundary, with static types on the Jade side.* A package is opened the first time the program calls it, from `JADE_LIBS` or the `libs/` beside the program, and its value ABI must match. Each call marshals its arguments by their static types and reads its result back as the type the checker gave it, raising when the package hands back another kind; text from a package is tainted. A handle is `{ rc, ptr, name }`, and releasing one frees the wrapper, never the pointee. A Jade function goes over as a `JadeFn` whose `invoke` is a trampoline generated for its type; a raise inside it waits in the error slot until the library returns, so it never unwinds through the library's frames. `native.rs` has it all.

*Object layouts.* Every heap object starts with an `i64` reference count, and a count below zero marks an immortal object such as a string literal. Strings are `{ rc, len, bytes…, 0 }`. Arrays hold their elements unboxed in a separate buffer, so growing one never moves the object. Dicts keep entries in insertion order, with an FNV-1a index past eight entries. Structs are `{ rc, type_id, fields… }`. An optional of a pointer type is the pointer, null for nil; an optional of a scalar is `{ i1, T }`. `mod.rs` has the full table.

*Strings carry a trust word.* A string is `{ rc, len, trust, bytes…, 0 }`, and trust is 1 for text from outside the program: a file, a command, stdin, the environment, the network. Every function that builds a string from others gives it the most tainted of their trusts, the string builder collects the trust of what is pushed into it, and a char taken from a tainted string keeps the taint in bit 31, so `"" + s[0]` cannot launder it. `sh.exec`, `sh.run`, `sh.output`, `fs.read` and `fs.read_bytes` refuse a tainted argument with v1's message. Bytes share the string layout, so they carry trust the same way.

*Reference counts are atomic*, because tasks share one heap. Releasing an object whose count reaches zero releases what it holds, through a function generated per type. A struct value may hold any descendant of its static type, so struct release switches on the object's type id.

*Text matches v1 exactly.* `print`, `str()` and f-strings all go through `jade.tostr.<type>`. Floats print the shortest digits that read back to the same value, found with `snprintf` and `strtod`, laid out without an exponent. Case mapping uses tables built from Rust's own Unicode data when the program uses `upper`, `lower` or `capitalize`.

*Classes.* A field read through a static class whose descendants keep the field at a different offset, which happens when a class is used as a child's second parent, switches on the object's class id in `func.rs`'s `field_ptr`; otherwise it is one direct load. A class's release runs its destructor chain before freeing its fields: it holds a reference while `drop` runs, gives it back, and frees the object only when that was the last one. A destructor that stored `self` keeps the object alive, and a bit in the type id word (`layout::DROPPED`) stops its destructors from running again; every type id read masks the bit off.

*Errors are values in a slot, not unwinding.* Raising puts a class value in the thread-local `jade.err`. MIR checks the slot after every operation that can raise and branches to the nearest `catch` or to the function's unwind block, which releases its locals and returns. Support code that fails calls `F::fail`, which raises and returns a zero value at once, since its caller checks next; code lowered from MIR calls `F::raise` and carries on to that check. An error that reaches the top prints `jade: <message>`, or `jade: uncaught Name` for a class with no message, and exits with status 1, as v1 did. Calls deeper than 10,000 raise `recursion limit exceeded`.

*Tasks are jobs on one queue.* A call to an `async fn` packs its arguments into a job, which is also its future, and queues it. Worker threads, made as the queue needs them up to `max_tasks()`, run jobs; one mutex and one condition variable guard the queue and every job's state. Awaiting a job that has not started runs it on the awaiting thread, so a chain of tasks each awaiting the next runs on one thread, and `set_max_tasks(1)` cannot deadlock. A job carries a free function generated for its result type, so the queue can release a future without knowing what it holds. `tasks.rs` has the layout.

## What each file does

- *`mod.rs`* holds `Cx`, the module being built, with `support`, `libc` and string literals. It also builds the module from a program, emits the C `main`, and runs optimization and object output.
- *`link.rs`* links the object into a program with LLD, in-process. On macOS it writes a stub the linker reads for each system library the object imports from, `libSystem` always and for a program that runs a model the Objective-C runtime and Metal, listing exactly the symbols it uses, so no Apple SDK is needed. On Linux it links against glibc's runtime `libc.so.6`, or musl's loader where there is no glibc, and the program's `_start` comes from the backend, so no development package is needed either. One `_start` serves both, since musl's `__libc_start_main` reads only the first three of the arguments glibc's takes.
- *`lld.cpp`* is the C entry point into LLD's Mach-O and ELF drivers, and the only C++ in the compiler.
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
- *`tasks.rs`* has tasks: the job layout, the queue and its workers, `await`, `join`, `wait`, `cancel`, the concurrency limit and `time.after`.
- *`time.rs`* has `std::time`: the two clocks, sleeping, UTC calendar conversion and `time.local`.
- *`math.rs`* has `std::math`, as LLVM intrinsics and C math calls, with v1's rounding and saturation.
- *`bytes.rs`* has bytes: indexing, `decode` with strict UTF-8 checking, lossy decoding for command and network output, `slice`, `encode`, `std::bytes`, and the `b"…"` form `print` shows.
- *`fs.rs`* has `std::fs`, the `I/O error: <op> '<path>': <reason>` messages, and `refuse_tainted`, the sink check.
- *`sh.rs`* has `std::sh`, and `spawn`, which starts a program with chosen standard streams; `http.rs` uses it too.
- *`env.rs`* has `std::env` and `std::path`, which follows Rust's `Path` on Unix as v1 did.
- *`random.rs`* has `std::random`: v1's xoshiro256** generator, so a seeded program draws the same numbers.
- *`json.rs`* has the `Json` value, `json.parse` with serde_json's grammar and messages, and `json.stringify`, generated per type.
- *`native.rs`* has native packages: loading, marshalling to and from `JadeVal` by static type, handles, callbacks and their trampolines, and freeing what crossed.
- *`infer.rs`* has inference: the tokenizer's tables and Unicode classes, connecting to dovata and mapping the model's memory with the descriptors it sends, Metal through `objc_msgSend`, one forward step as one command buffer over the op table, and greedy generation. `src/infer/README.md` describes the whole path.
- *`http.rs`* has `std::http` and `std::uhttp`, both through `curl`, and `uhttp.stream`, which calls the handler through a function pointer once a line.
- *`tests.rs`* builds small programs, runs them, and checks what they print. One test checks that a program declares only C library functions.

## Who uses it

*Used by:* `aot::compile`, reached through `jade build`, `jade run` and `jade test`.

*Depends on:* `mir` for its input, `inkwell` for LLVM, and LLD, LLVM's linker, which the root `build.rs` compiles into the compiler from the same LLVM 18 install. No system `cc` or linker is involved.

## Gotchas

*Never emit into a terminated block.* `F`'s branch helpers check, and `func.rs` starts a fresh block after any statement that ends one, such as a failing operation. Raw builder calls placed after a `ret` or `unreachable` produce a module that does not verify.

*Equality is structural for arrays and dicts, and identity for structs.* v1 refused `==` between two arrays at run time; the checker now accepts it, so this backend gives it a meaning. A struct is a reference, so two structs are equal when they are the same object.

*Popping an empty array is an error.* v1 returned `nil`, but an `[int]` cannot hold `nil`, so the static backend raises `pop from an empty array`. `random.choice` on an empty array raises for the same reason.

*A few std answers differ from v1 on purpose.* `math.pow` of two ints with a negative exponent raises, since its result is an int. `http.head` runs `curl --head`, where v1's `-X HEAD` failed against any server that sends a length with no body. `json.stringify` keeps the taint of the strings it writes, where v1's result was trusted, and writes a class value as an object of its fields, where v1 wrote `null`.

*A new C function goes in `Cx::libc`'s table.* It panics on a name it does not know, so a missing signature shows up the first time a test builds the code that uses it. Add the name to the pinned list in `tests.rs` at the same time.

## Building and testing

```sh
cargo test backend::
./src/scripts/golden.sh
./target/debug/jade build --emit ir file.jde    # the IR, before optimization
```
