# `src/codegen/gpu_kernels/nvidia/`: NVIDIA's kernels

## What this subtree is

`mod.rs`'s `ptx` builds every kernel in `../` for NVIDIA GPUs, through LLVM's NVPTX backend, and gives the PTX as text. PTX is NVIDIA's portable GPU code: the program will carry it, and NVIDIA's driver compiles it for the card when the program loads it, so the program needs the driver alone and no CUDA toolkit.

## The target

The PTX is built for `sm_86`, the RTX 3090's generation, and runs on newer cards too, such as the RTX 2000 Ada, `sm_89`: the driver compiles it for the card it finds. LLVM 18 writes PTX version 7.1 for `sm_86`, which drivers from CUDA 11.1 on read. The sources are LLVM 18's [NVPTX usage page](https://releases.llvm.org/18.1.0/docs/NVPTXUsage.html) and NVIDIA's [list of compute capabilities](https://developer.nvidia.com/cuda-gpus).

## How a kernel is launched

Each kernel takes six parameters: the four source tensors and the destination, each a 64-bit device pointer to the tensor's first byte or 0 for none, and the argument record, 576 bytes passed by value as a kernel parameter. It runs in blocks of 64 threads, as on Metal, and the grid is the op's threads divided by 64, rounded up. The record and the grid are worked out from the op record as `../../infer/metal.rs` does for Metal; `launch` in `../tests.rs` is the same arithmetic in Rust.

## What each file does

- *`mod.rs`* is `ptx`: LLVM's NVPTX machine, the kernels written with `Vendor::Nvidia`, the module checked and optimized at `O3`, and PTX out.
- *`tests.rs`* checks the PTX on any machine, and on a machine with an NVIDIA GPU runs every case of `../tests.rs` on it. It opens `libcuda.so.1` itself, loads the PTX with `cuModuleLoadDataEx`, which runs NVIDIA's assembler and returns its log when it refuses the PTX, and launches each case with `cuLaunchKernel`. Without the driver or a GPU it prints why and passes. A GPU older than `sm_86` is skipped the same way.

## Who uses it

*Used by:* the tests, for now. The CPU side that launches these kernels in a program, which opens dovata's VRAM and runs each step on CUDA, comes next, as step 4 of `design/v2-gpu-targets.md`.

## Building and testing

```sh
cargo test gpu_kernels::nvidia -- --nocapture
```

On the NVIDIA machine, the output names the GPU and gives each case's worst error; a failure names the case and the first wrong value.
