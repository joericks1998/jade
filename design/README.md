# `design/`: design documents for work in progress

## What this subtree is

Each file here is the design for one large change, written before the code so the decisions in it can be reviewed first. A design states the problem, what it recommends, what a program looks like under it, and what stops working. Once the work lands, the directory READMEs it touched become the reference, and the design stays as the record of why.

## Why it exists

Jade 2.0 changes the compiler's architecture end to end, and several of its choices change what programs the language accepts. Those choices belong to the person who owns the language, and a design document is where they can be made before anything is built on them. The directory READMEs describe code that exists; nothing in them could hold a decision about code that does not exist yet.

## What each file does

- *`v2-static-values.md`* is the design for compiling every value to a plain machine value with a type known at compile time, with no runtime library: how types are inferred per call, how `nil`, dicts, mixed values, JSON and exceptions are typed, how the compiler's pipeline changes, which examples change, and in what order the work lands.
- *`v2-classes.md`* is the design for 2.0.1's simpler language: classes in place of structs and `extend`, with methods attached by `fn A::m()`, private members, constructors, types as decorators, and no closures.
- *`v2-number-types.md`* is the design for 2.0.4's sized number types, `byte` to `ulong` and `float32`: their names and widths, a type function and a converter for each, and arithmetic rules that follow assembly, with no implicit widening.
- *`v2-pipes.md`* is the design for 2.0.4's pipes: `x |> f` calls `f` once per piece of `x` as it arrives, while `f(x)` waits for all of it; what a piece is for a generator, a held stream, a prompt and any other value; and how a typed dereference moves to `toInt(?p)`.
- *`v2-rust-packages.md`* is the design for 2.0.5's native packages: Rust crates built by cargo replace the C foreign function interface, with `#[jade::export]` writing the `extern "C"` layer and a description of each signature embedded in the built library for the checker to read.
- *`v2-simplify.md`* is the plan for 2.0.6 to 2.0.9: TIR removed so the later stages read the parser's tree, `toDict` and `toString` in place of `std::json` and the `Json` type, the standard library written in Jade, and library code that does not depend on a program's types compiled once into a cached `.a` and linked.
- *`v2-gpu-targets.md`* is the proposal, not yet agreed, for giving Apple, NVIDIA and AMD GPUs one folder each under `codegen/infer/`: the five functions every GPU folder answers, NVIDIA and AMD kernels built through LLVM, dovata holding VRAM and handing each region to a program by an IPC handle in its record, dovata deciding what is in VRAM, and the order the work lands in.
- *`dovata-vram-cache.md`* is the proposal, not yet agreed, for dovata managing VRAM as a cache over RAM on discrete GPUs, with the GPU doing all the computation: the per-token limit it aims at, why a dense model's layers are pinned and streamed rather than evicted by recent use, the leases that keep dovata from freeing memory a program still reads, and who issues the copies.

*Paths in the older designs.* A design keeps the paths it was written with. Since 2.0.11, `src/backend/` is `src/codegen/`, with its support code in `codegen/support/`, its hooks in `codegen/sys/`, `libplan.rs` and `libcache.rs` in `codegen/libcache/`, and `link.rs` and `lld.cpp` in `src/link/`. `src/aot/imports.rs` is `src/mir/imports.rs`, the driver in `src/aot/mod.rs` is `src/build/mod.rs`, and `mir/lower.rs`, `ops.rs`, `packages.rs` and `pipe.rs` are in `mir/lower/`.

## Who uses it

People and sessions working on the `2.0.0` branch read it before working on the type checker, the code generator or the runtime. Nothing in the build reads it.
