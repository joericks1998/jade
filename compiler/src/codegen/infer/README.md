# `src/codegen/infer/`: the code a program runs a model with

## What this subtree is

This directory is the code half of inference: what the prelude, `src/infer/prelude.jde`, asks of `__infer`, written into the program from the model's plan. [`src/infer/`](../../infer/README.md) describes the whole path, from the plan dovata gives at build time to a token.

It has four jobs, one file each:

```
tables.rs  the tokenizer's pieces and the Unicode classes its splitter asks about
map.rs     each region of the model's memory, opened by name and mapped
metal.rs   the GPU: Metal's setup, and one forward step as one command buffer
next.rs    one token per call, picked greedily from the step's logits
```

## Why it is a directory

It was one file, `infer.rs`, until 2.0.13. Splitting it keeps the part that talks to Apple's GPU in `metal.rs` alone. Apple publishes no instruction set for its GPUs, so Metal compiles the kernels, `src/infer/kernels.metal`, inside the program the first time it runs a prompt, and jade cannot show their assembly. NVIDIA and AMD GPUs can be programmed through LLVM instead, so a CUDA path would be a file beside `metal.rs`, and the tables, the mapping and the token choice would serve both.

## What each file does

- *`mod.rs`* answers each `__infer` operation, `infer_op`, and holds what the rest share: the plan, the model's globals, the one lock that covers the model's memory and the GPU, `init`, and the wrapper that takes the lock around a function that may raise. The lock is not held between tokens, since a pipe runs the program's own code there; a conversation whose tokens another prompt displaced reads them back in at its next token.
- *`tables.rs`* writes the tokenizer's pieces, merges and added tokens into the program as constants, with the Unicode classes its splitter asks about, and `taint`, which marks a copy of text as coming from outside the program.
- *`map.rs`* is `map`: it opens each region of the model's memory by the name dovata's record gave at build time, a part file by its path or a shared memory object with `shm_open`, checks who owns it, and maps it. No message goes to dovata; the names are constants of the program.
- *`metal.rs`* is everything that talks to Metal. `setup` builds a device, compiles the kernels, makes a pipeline per kernel and a buffer over each region without copying it. `step` fills the step's inputs, walks the op table and encodes one dispatch per op into one command buffer, then waits for it. Metal is an Objective-C API, so every call is `objc_msgSend` with the selector's own signature; on Apple Silicon a struct of more than 16 bytes, such as `MTLSize`, goes by pointer.
- *`next.rs`* is `next`: it runs the tokens the model has not seen yet through as many steps as they take, then picks the likeliest token with `argmax`, or answers -1 at a stop token or a full context.

## Who uses it

*Used by:* `../func/`, which lowers each `__infer` operation in the prelude to `infer_op`, and `../mod.rs`, whose C `main` calls `init` in a program that runs a model.

*Depends on:* `crate::infer::plan` for the plan, `../libcache/shared.rs` for the model's state, which the program defines once, and `../memory/layout.rs`.

## Gotchas

*macOS on Apple Silicon only, for now.* `build::compile` refuses a program that runs a model anywhere else before this directory is reached.

## Building and testing

```sh
cargo test infer::
```
