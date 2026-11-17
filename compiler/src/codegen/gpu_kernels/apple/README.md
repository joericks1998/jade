# `src/codegen/gpu_kernels/apple/`: Apple's kernels

## What this subtree is

The kernels a program runs a model with on Apple's GPU, as Metal source. `kernels.metal` has one kernel per name in `crate::infer::plan::KERNELS`, plus `k_argmax`, which nothing launches yet. `mod.rs` holds it as `SOURCE`, the constant the program carries.

## Why it is source

Apple publishes no instruction set for its GPUs, so jade cannot build these through LLVM as it does NVIDIA's. The program carries the source, and Metal compiles it for the GPU the first time the program runs a prompt. It sat in `src/infer/` until 2.0.15, which moved every GPU's kernels into `gpu_kernels/`, beside the CPU code that launches them.

## What each file does

- *`kernels.metal`* is the kernels. Its header says how they share work and how they read their arguments.
- *`mod.rs`* is `SOURCE`.

## Who uses it

*Used by:* `../../infer/metal.rs`, which compiles `SOURCE` with `newLibraryWithSource:` and launches each kernel. The LLVM kernels in `../` compute the same ops, so a change to what an op means goes in both.

## Building and testing

```sh
cargo test gpu_kernels::tests::the_metal_kernels_have_every_name
cargo test infer::        # runs them, with a running dovata and llama.cpp
```
