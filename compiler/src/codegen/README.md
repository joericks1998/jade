# `src/codegen/`: MIR to object files

## What this subtree is

This is Jade 2.0's code generator. It turns [MIR](../mir/README.md) into LLVM IR, and LLVM optimizes the IR and turns it into machine code, written as object files: the program's own, and one per std library instance, kept in a cache under `~/.jade/lib`. [`src/link/`](../link/README.md) then joins the objects and the C library into a binary. This directory writes the IR; LLVM does the assembling. Every value has a type the compiler knows, so an `int` is an `i64` in a register and a struct is a fixed layout. No compiled program reads a type tag.

```
MIR → codegen writes → LLVM IR → LLVM compiles → .o files → src/link/ → binary
```

```sh
./target/debug/jade build examples/strings/unicode/unicode.jde -o unicode
otool -L unicode        # /usr/lib/libSystem.B.dylib, and nothing else
```

It is the only code generator. It sat behind `--backend=static` while it grew, and became the only one in phase 6 of `design/v2-static-values.md`, when v1's backend and both of its runtimes were deleted. It was `src/backend/` until 2.0.11, which moved linking into `src/link/` and gave this directory the name of the one job it does. `golden.sh` holds every example to the outputs v1.4.8 printed.

## Why it was built

The v1 backend lowered bytecode into calls to two runtime libraries: about 16,600 lines of Rust and C, reached through 237 functions, with a tag check on every operation. GPU inference cannot sit on top of that, and `design/v2-static-values.md` explains the rest of the reasons. Codegen replaced both runtimes. What v1 called the runtime is emitted here, into each program, only where the program uses it.

On one benchmark of recursion, string building, dict inserts and a float loop, a static binary ran in 0.01 seconds against 0.31 for v1, and weighed 34 KB against 4.4 MB.

## How it works

*Support code is generated on demand.* Every helper a program needs is an internal LLVM function named `jade.*`, generated the first time something asks for it through `Cx::support`. A program that never splits a string carries no split. Code that depends on a type is generated once per type the program uses, such as releasing an array of strings (`jade.release.Array(Str)`) or printing a dict of arrays. The optimizer inlines what is small and drops what is unused.

*The binary links the C library alone.* [`src/link/`](../link/README.md) links the objects with LLD, inside the compiler, so `jade build` needs no `cc`. On Linux the program's `_start`, which a development package would normally supply, is written into its module by `start.rs`.

*The C library is the only dependency.* `Cx::libc` declares the functions used, and a test pins the list: memory, `write` and `read`, `snprintf` and `strtod`, the pthread calls behind the 256 MiB main stack and tasks, the clocks, the math library, `fopen` and the directory calls for `std::fs`, and `posix_spawn` with pipes for `std::sh` and `curl`. A few have a different symbol on some platform, such as `errno`'s accessor or Intel macOS's `stat$INODE64`; `libc_symbol` maps them. A `struct stat` or `struct dirent` field is read at an offset chosen per target in `sys/fs.rs`.

*A Rust package is called through its `extern "C"` wrapper, with C forms built from static types.* `#[jade::export]` gives each exported function a wrapper, `__jade_<crate>_<fn>(a0, …, out, err) -> i32`, and a `__free` companion (`jade-rs/`). Each argument is passed as a pointer to its C form, built from the Jade value by `c_write_fn`: a number at its width, `bool` as a byte, `char` without its taint bit, a string or bytes as `{ ptr, len, cap }` lending the Jade string's own bytes, an array as a buffer of its elements' C forms, an optional as `{ some, value }`, and a Rust struct's class as its fields' C forms in order, laid out by LLVM as C lays them out. What `c_write_fn` allocated is freed after the call. The result comes back through `out`, is copied into Jade values by `c_read_fn`, text tainted, and handed to the companion, which drops it with Rust's allocator. Status 1 raises the message in `err`. Nothing is loaded when the program runs: the staticlib is linked in. `native.rs` has it all.

*Object layouts.* Every heap object starts with an `i64` reference count, and a count below zero marks an immortal object such as a string literal. Strings are `{ rc, len, bytes…, 0 }`. Arrays hold their elements unboxed in a separate buffer, so growing one never moves the object. Dicts keep entries in insertion order, with an FNV-1a index past eight entries. Structs are `{ rc, type_id, fields… }`. An optional of a pointer type is the pointer, null for nil; an optional of a scalar is `{ i1, T }`. `mod.rs` has the full table.

*Strings carry a trust word.* A string is `{ rc, len, trust, bytes…, 0 }`, and trust is 1 for text from outside the program: a file, a command, stdin, the environment, the network. Every function that builds a string from others gives it the most tainted of their trusts, the string builder collects the trust of what is pushed into it, and a char taken from a tainted string keeps the taint in bit 31, so `"" + s[0]` cannot launder it. The hooks behind `sh.exec`, `sh.run`, `sh.output`, `fs.read` and `fs.read_bytes` refuse a tainted argument with v1's message, and the hooks that bring outside text in mark it, so the std library's Jade code can neither skip a check nor launder a value. Bytes share the string layout, so they carry trust the same way.

*Reference counts are atomic*, because tasks share one heap. Releasing an object whose count reaches zero releases what it holds, through a function generated per type. A struct value may hold any descendant of its static type, so struct release switches on the object's type id.

*Text matches v1 exactly.* `print`, `str()` and f-strings all go through `jade.tostr.<type>`. Floats print the shortest digits that read back to the same value, found with `snprintf` and `strtod`, laid out without an exponent. Case mapping uses tables built from Rust's own Unicode data when the program uses `upper`, `lower` or `capitalize`.

*Classes.* A field read through a static class whose descendants keep the field at a different offset, which happens when a class is used as a child's second parent, switches on the object's class id in `func.rs`'s `field_ptr`; otherwise it is one direct load. A class's release runs its destructor chain before freeing its fields: it holds a reference while `drop` runs, gives it back, and frees the object only when that was the last one. A destructor that stored `self` keeps the object alive, and a bit in the type id word (`layout::DROPPED`) stops its destructors from running again; every type id read masks the bit off.

*Errors are values in a slot, not unwinding.* Raising puts a class value in the thread-local `jade.err`. MIR checks the slot after every operation that can raise and branches to the nearest `catch` or to the function's unwind block, which releases its locals and returns. Support code that fails calls `F::fail`, which raises and returns a zero value at once, since its caller checks next; code lowered from MIR calls `F::raise` and carries on to that check. An error that reaches the top prints `jade: <message>`, or `jade: uncaught Name` for a class with no message, and exits with status 1, as v1 did. Calls deeper than 10,000 raise `recursion limit exceeded`; a call to a std library function does not count. MIR skips the check after a division by a constant that is not 0 or -1, which cannot raise, and `raise_if` emits nothing for a condition the builder already folded to false.

*The std library is Jade code, plus hooks.* Since 2.0.8 every std package but the value types' own is written in Jade in `src/lib/`, and reaches codegen as ordinary MIR functions. What it cannot do in Jade arrives as `Op::Sys`, one hook per operation: a C call, an LLVM intrinsic, or a value operation Jade has no spelling for. A hook whose C call fails returns an empty value and leaves the C library's error code in the thread-local slot `jade.oserr`, which the `os_error` hook reads, so library code builds v1's message from it and nothing between can change it, as an allocation can change `errno`. Shared mutable state, such as the lock around `TZ` and the random generator, stays in a hook. What stays generated here is the value types, `toDict`, printing, errors, tasks, and inference.

*The std library is compiled once per machine and cached.* Since 2.0.9 a build compiles each std library instance, one library function at one set of argument types, in an LLVM module of its own, keeps its object under `~/.jade/lib/<version>/<triple>/`, and links it with LLD; a later build of any program that needs the same instance links the same object. `libcache/plan.rs` decides what is cached, `libcache/mod.rs` keeps the cache, and `shared.rs` keeps the program and the objects agreeing on their state. Part 4 of `design/v2-simplify.md` has the plan.

- *What is cached.* An instance whose types are all built in, which reads no global or field and does nothing only the program can do, and which calls only instances like it. One that touches a class, such as `random.choice` over an array of `Card` or `time.parts`, whose `TimeParts` type id is its place among the program's classes, compiles with the program as before. `raise "text"` builds a `RuntimeError`, the one class library code may hold: a library object asks the program to build it, through `jade.new_error`.
- *What is copied.* An instance small enough that inlining it matters is not given an object: each module that calls it, the program's or another library object's, compiles a private copy, as support code is, so `math.sqrt(x)` is still one `fsqrt` in the program's code and `math.clamp` is `fmaxnm` and `fminnm`. The rule is in `libcache/plan.rs`: numbers, bools and chars only, arithmetic, comparisons, one-instruction hooks and calls to other copied instances, and at most 20 MIR statements and terminators with each callee counted once. *The measurement:* in 2.0.8, which compiled every library instance with the program, `opt -passes='default<O2>'` over each std example's IR inlined 45 of the 46 instances this rule copies in every example that called them; `abs(int)` was kept out of line in its one example. A loop of 20 million iterations calling `math.sqrt`, `abs`, `max`, `clamp`, `min` and `floor` ran in 36 ms with 2.0.8 and with this rule, in 54 ms when only instances that call nothing were copied, since `clamp` then became a call, and in 229 ms with every instance in an object of its own.
- *The key.* An object is stored as `<key>.o`, the key being the SHA-256 of the compiler's version, the target triple, CPU, features and code-generation options, the pipeline, `default<O2>`, and the instance's module as IR text before optimization. The key covers the IR, so a development build whose library or codegen changed never reads an object built from older code, even at the same version. A cached instance's code has the symbol `jade.lib.` and 16 hex digits of the SHA-256 of its instance label, external and hidden.
- *Every build* makes every cached instance's module, which costs 2 to 4 ms for a program's 8 to 16 instances on a warm cache, hashes it, and links the object when it is there. A miss optimizes the module and writes the object to a name of its own in the same directory, then renames it into place, so two builds at once never read part of one. The modules are independent, so they are built on up to 8 threads, each with its own LLVM context and target machine.
- *State has one definition.* The thread's error slot, the hooks' error-code slot, the task queue, the random generator, the time-zone lock and `argc` and `argv` are each made by `Cx::state`, which defines the global in the program's module and only declares it in a library module. So are the functions whose code reads the program's classes, `jade.set_err`, `jade.raise_msg`, `jade.release.struct` and `jade.new_error`, through `Cx::shared_fn`. A library module records what it declared, and the program's module defines all of it with external linkage and hidden visibility, even what its own code never uses: `env.args()` in a cached object still needs the C `main` to store the arguments. A thread-local that crosses modules uses the initial-exec model, which LLD turns into local-exec in the executable. A name no library object uses stays internal, so a program built in one module, which is what `--emit ir` shows, compiles to the IR 2.0.8 made.
- *The cache never fails a build.* A cache directory that cannot be made or written sends the objects to the output's directory, deleted after linking. A failure before linking builds the program in one module. A link failure with an object from the cache makes the driver, `build::binary`, build once more in one module, and when that links, a warning names the directory to remove. A library module that asks for something only the program knows, such as a class's layout, marks itself through `Cx::mark_program_dependent`, and its instance and every cached instance calling it compile with the program; nothing in the std library does, and a test holds it to that.
- *Nothing prunes the cache.* Objects of older versions stay in their own directories until removed; `rm -rf ~/.jade/lib` clears it. `--emit ir`, `--emit mir` and `--emit asm` never use it.

*Tasks are jobs on one queue.* A call to an `async fn` packs its arguments into a job, which is also its future, and queues it. Worker threads, made as the queue needs them up to `max_tasks()`, run jobs; one mutex and one condition variable guard the queue and every job's state. Awaiting a job that has not started runs it on the awaiting thread, so a chain of tasks each awaiting the next runs on one thread, and `set_max_tasks(1)` cannot deadlock. A job carries a free function generated for its result type, so the queue can release a future without knowing what it holds. `support/tasks.rs` has the layout.

## What each file does

The top level is the core: the module being built and how MIR becomes LLVM. Three subdirectories hold the rest, each with its own README.

- *`mod.rs`* holds `Cx`, the module being built, with `support`, `libc` and string literals. It also builds the program's module, whole or beside library objects, emits the C `main`, makes the target machine, and runs optimization and object output. `ir` returns a program's IR in one module for `--emit ir`; `asm` builds the same module, optimizes it with the target machine an object gets, and asks LLVM for assembly instead of an object, for `--emit asm`; and `objects` writes a program's object files for the linker, its `lib_cache` argument naming the cache's root, `None` building in one module. `Objects::remove` deletes what was written beside the output once it is linked.
- *`fb.rs`* is `F`, a function-building layer over inkwell. Its methods return values directly, and `if_`, `if_else`, `while_` and `for_range` take closures. All the support code is written in it.
- *`layout.rs`* maps types to LLVM types, holds the object offsets, and reads and makes optionals.
- *`func.rs`* lowers one MIR function: locals as zeroed stack slots, statements with the ownership rules, terminators, every binary operator with its overflow and zero checks, and every support operation. Its `Env` holds each function's LLVM function by id; a library module declares only the ones it calls. A library function, `Func::library`, skips the recursion count, as a built-in always did, so the optimizer inlines a one-instruction one such as `math.sqrt` down to that instruction.
- *`rc.rs`* emits retain and the per-type release and destroy functions.
- *`errors.rs`* has the error slot, raising, `jade.new_error`, an error's message, the report for an uncaught error, the `F::fail` and `F::raise` helpers, `IO_ERROR`, the prefix of every failure of the outside world, and `thread_global`, which makes the thread-local slots through `Cx::state`.
- *`shared.rs`* has `Cx::state` and `Cx::shared_fn`, through which every piece of mutable state and every class-dependent support function is made, so the program defines each once and library objects declare it, and `Cx::define_exports`, which defines in the program what the library objects declared.
- *`start.rs`* has `_start`, the start-up code a Linux program begins at, as glibc's `start.S` writes it for x86-64 and AArch64. Writing it into the program means linking needs no development package. musl's `__libc_start_main` reads only the first three of the arguments glibc's takes, so one `_start` serves both. A Mac program has none, since its loader calls `main`.
- *`native.rs`* has calls into Rust packages: the C form of each type, building one from a Jade value and reading one back, the call through the wrapper, the companion that frees what Rust returned, and raising its error.
- *`infer.rs`* has inference: the tokenizer's tables and Unicode classes, connecting to dovata and mapping the model's memory with the descriptors it sends, Metal through `objc_msgSend`, one forward step as one command buffer over the op table, and greedy generation one token per call, so a pipe can hand on each piece of a reply as it comes. `src/infer/README.md` describes the whole path.
- *`tests.rs`* builds small programs, runs them, and checks what they print. One test checks that a program declares only C library functions.
- *[`support/`](support/README.md)* has the support code for values: strings, case tables, bytes, arrays and dicts, comparison, printing and conversions, sized numbers, `toDict`, and tasks.
- *[`sys/`](sys/README.md)* has the `__sys` hooks the std library written in Jade calls: the C library and LLVM operations behind `std::fs`, `std::sh`, `std::http`, `std::random`, `std::time`, `std::math` and `std::env`.
- *[`libcache/`](libcache/README.md)* has the std library's object cache: which instances get an object, which are copied into their callers, and the cache under `~/.jade/lib`.

## Who uses it

*Used by:* `build::binary`, which hands the objects to `src/link/`, and `build::compile` for `--emit ir` and `--emit asm`, reached through `jade build`, `jade run` and `jade test`.

*Depends on:* `mir` for its input, `inkwell` for LLVM, `sha2` for the cache's keys and symbols, and `rustpkg::jade_home` for the cache's root. It does not call the linker; the driver in `src/build/` does, after this directory is done.

## Gotchas

*Never emit into a terminated block.* `F`'s branch helpers check, and `func.rs` starts a fresh block after any statement that ends one, such as a failing operation. Raw builder calls placed after a `ret` or `unreachable` produce a module that does not verify.

*Equality is structural for arrays and dicts, and identity for structs.* v1 refused `==` between two arrays at run time; the checker now accepts it, so codegen gives it a meaning. A struct is a reference, so two structs are equal when they are the same object.

*Popping an empty array is an error.* v1 returned `nil`, but an `[int]` cannot hold `nil`, so codegen raises `pop from an empty array`. `random.choice` on an empty array raises for the same reason.

*A few std answers differ from v1 on purpose.* `math.pow` of two ints with a negative exponent raises, since its result is an int; that is in `src/lib/math.jde` now. `http.head` runs `curl --head`, where v1's `-X HEAD` failed against any server that sends a length with no body; that is in `src/lib/http.jde`.

*Make mutable state with `Cx::state`, never `add_global`.* A global made any other way is defined in every library object that uses it, so each object would have its own. A read-only constant, such as a string literal or a Unicode table, may be private and duplicated. A support function whose code reads `Cx::structs`, `Cx::drops` or the model's plan goes through `Cx::shared_fn` or calls `Cx::mark_program_dependent`.

*A cached instance's IR must be the same in every program.* It is built from the instance's MIR alone, in a context of its own, with no classes; nothing in it may come from the program, or the key changes with every program and the cache never hits. `a_library_modules_key_is_stable_and_covers_its_inputs` builds one twice.

*A new C function goes in `Cx::libc`'s table.* It panics on a name it does not know, so a missing signature shows up the first time a test builds the code that uses it. Add the name to the pinned list in `tests.rs` at the same time.

## Building and testing

```sh
cargo test codegen::
cargo test codegen::libcache    # the library cache, each test with a cache root of its own
./compiler/src/scripts/golden.sh
HOME=$(mktemp -d) ./compiler/src/scripts/golden.sh   # the same, from a cold cache
./target/debug/jade build --emit ir file.jde    # the IR, before optimization, in one module
./target/debug/jade build --emit asm file.jde   # this machine's assembly, optimized, in one module
```
