# `src/codegen/gpu_kernels/`: the code that runs on the GPU

## What this subtree is

Codegen writes two kinds of code. Everything else in `src/codegen/` writes CPU code: the program itself, and, in [`../infer/`](../infer/README.md), the part of it that opens the model's memory and starts each step. This directory writes the GPU code: one kernel per op of a model's forward step, such as matrix multiplication or attention.

Inference has three parts, in three places:

```
src/infer/                   build time, inside the compiler: asks dovata for the plan,
                             turns ?p into calls to the Jade tokenizer
src/codegen/infer/           run time, on the CPU: opens the model's memory, launches
                             each step, picks the next token
src/codegen/gpu_kernels/     run time, on the GPU: the kernels each step launches
```

The kernels are the same for every program built against a plan. Each takes up to four source tensors, a destination, and a 576-byte argument record giving every tensor's shape, strides and ggml type and the op's parameters, so one kernel serves every shape a plan gives it. `crate::infer::plan::KERNELS` is the contract between a plan and every GPU here: an op record names a kernel by its place in that list, and each GPU has a kernel of each name.

| GPU | where | what the program carries | who compiles it for the card |
|---|---|---|---|
| Apple | `apple/kernels.metal` | Metal source | Metal, when the program first runs a prompt |
| NVIDIA | `nvidia/` and the files here | PTX, built through LLVM | NVIDIA's driver, when the program loads it |

## Why it was built this way

*Apple's kernels are source text; NVIDIA's go through LLVM.* Apple publishes no instruction set for its GPUs, so the most a program can carry is Metal source. NVIDIA's GPUs can be programmed through LLVM's NVPTX backend, which LLVM 18, already inside jade, has built in. Building them through LLVM means a program needs only NVIDIA's driver, not the CUDA toolkit, and the kernels are written with the same tools as the rest of codegen. `design/v2-gpu-targets.md` has the decision and the two other ways that were weighed.

*The LLVM kernels are written once, against a builder.* `builder.rs`'s `K` is a function builder in the style of `codegen::F`, for a module of its own built for the GPU. The few things that differ between GPUs are methods of `K`, each a match on `Vendor`: which thread this is, adding a value across a warp, waiting for the block, memory the block shares, and the fast math functions. An AMD GPU, through LLVM's AMDGPU backend, would be one more `Vendor`, answering those few methods.

*The kernels are plain on purpose.* They are `kernels.metal`'s kernels, written through the builder: the same work per thread, the same order of sums. The ops that read a whole row for each output, `mul_mat`, `rms_norm` and `flash_attn_ext`, give each output one warp of 32 threads; every other op runs one thread per value; a `mul_mat` over many tokens, as when a prompt is read, multiplies 32 by 32 tiles through shared memory. Tensor cores, the token picked on the GPU, and f16 tiles are the tuning that comes once these run.

*The weight type is checked once per output, not once per value.* A matrix multiplication's loop is written out once for each weight type, and decodes 32 weights at a time, reading their scale once. That makes `k_mul_mat` the longest kernel, about 7,900 lines of PTX.

*Rope's angles stay accurate at long positions.* NVIDIA's fast `sin`, `cos` and `pow` are accurate near zero only, and rope's angles reach thousands of radians. So the plan works out `freq_base^(-2/n_dims)` once on the CPU, as float parameter 3 of a rope record, the kernel raises it to each pair's power by squaring, and `sin_cos` brings the angle into -pi to pi in f64 before the fast sine and cosine. A model with frequency factors, as Llama 3.1 and 3.2 have in `rope_freqs.weight`, passes them as rope's third tensor, and integer parameter 2 says so; each pair's angle is divided by its factor, on both GPUs.

## What each file does

- *`mod.rs`* lists the directory's parts, `ARGS_BYTES`, the argument record's size, and `write_all`, which writes every kernel into a module in `KERNELS` order.
- *`builder.rs`* is `Gpu`, the module the kernels are built in, and `K`, the builder each kernel and helper is written in: integers, floats, memory, branches and loops, and the per-GPU methods.
- *`values.rs`* reads and writes the values of a tensor of every type the kernels take: the argument record, one value of f32, f16, bf16, i32 or a block type, an index, and 32 values at a time, decoded from q8_0, q4_0, q4_K, q5_K and q6_K blocks with their scale read once.
- *`elementwise.rs`* has the kernels that compute each value on its own: `get_rows`, `mul`, `add`, `rope`, `cpy`, `set_rows` and `swiglu`.
- *`norm.rs`* has `rms_norm`, one warp per row.
- *`matmul.rs`* has `k_mul_mat`, one warp per output for a step of one token, and `k_mul_mm`, a block of 64 threads per 32 by 32 tile for a step of many.
- *`attention.rs`* has `k_flash_attn`, one warp per query head and token, the softmax in one pass over the cache.
- *[`apple/`](apple/README.md)* holds `kernels.metal`, Apple's kernels, and the constant the program carries them in.
- *[`nvidia/`](nvidia/README.md)* builds the kernels into PTX, and tests them on an NVIDIA GPU.
- *`emulate.rs`*, in tests only, runs the kernels on the CPU: built for this machine, compiled with LLVM's JIT, one CPU thread per GPU thread.
- *`tests.rs`* has the cases every runner runs: each kernel over small tensors of every type it reads, compared with the same op worked out in f64 from ggml's definitions.

## How the kernels are tested

Every case is an op record built the way `src/infer/plan.rs` builds one, turned into an argument record and a grid the way the step in `../infer/metal.rs` does. Its output is compared with plain f64 math, with the block types decoded from ggml's reference code, written out again in `tests.rs` rather than shared with the kernels. There are 49 cases: every kernel, every weight type, both matrix kernels with tiles cut short at the edges, rows whose length is not a multiple of 32, f16 and bf16 on the token side, a broadcast batch, both rope modes at position 3001, with and without Llama 3.1's frequency factors, a transposed copy, and attention over heads of 64, 80 and 128 values with grouped heads and a causal mask.

The cases run twice:

- *On the CPU, everywhere the tests run.* `emulate.rs` builds the kernels for this machine and runs each block as 64 CPU threads, so a warp sum waits for its 32 threads and adds in the same order NVIDIA's shuffles do, and a barrier waits for the block. This checks the kernels' math on a machine with no GPU, such as CI.
- *On an NVIDIA GPU, when the machine has one.* `nvidia/tests.rs` opens the driver, loads the PTX, and runs the same cases. Loading runs NVIDIA's own assembler on the PTX, so this also checks what the CPU cannot: the PTX itself and the per-GPU methods of `K`. A kernel that faults, such as on an illegal address, leaves the driver failing every later call, so the cases after it are reported as not run.

Neither run sees what only a real plan has: tensors at offsets inside a shared region, views placed at any offset dovata gives them, and `set_rows` writing into the same buffer it was given as `src0`. Those come with the CPU side, step 4 of `design/v2-gpu-targets.md`, tested against llama.cpp's replies.

## Who uses it

*Used by:* `../infer/metal.rs`, which carries `apple::SOURCE` into a program and reads `ARGS_BYTES`. Nothing launches the NVIDIA kernels yet; the CPU side, which opens dovata's VRAM and launches each step on CUDA, is step 4 of `design/v2-gpu-targets.md`.

*Depends on:* `crate::infer::plan` for `KERNELS`, the op record's layout and the thread rules, and `inkwell` for LLVM.

## Gotchas

*A kernel must not call a function the GPU cannot find.* LLVM turns a math intrinsic with no NVPTX instruction, such as `llvm.sin.f32`, into a call to `sinf`, which no PTX defines, and the driver refuses the module. `K`'s math methods use NVIDIA's own instructions, and a test checks the PTX declares nothing `.extern`.

*Every thread of a warp must reach a warp sum, and every thread of a block a barrier.* A kernel ends a thread early only for a whole warp at once, or for `k_mul_mm`, a whole block, and loops around a sum or a barrier run the same number of times in every thread. On the CPU, a kernel that broke this would hang its test rather than fail it.

*Change a kernel in both places.* `apple/kernels.metal` and the LLVM kernels compute the same ops; a change to what an op means, or to the argument record, goes in both.

## Building and testing

```sh
cargo test gpu_kernels::                     # the PTX, and every case on the CPU
cargo test gpu_kernels:: -- --nocapture      # each case's worst error, and the GPU test's skip reason
```

On a Linux machine with an NVIDIA GPU, the same command runs every case on the GPU as well.
