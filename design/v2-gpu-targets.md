# One folder per GPU

Status: proposed 2026-11-15. Steps 2 and 3 are built: dovata holds a model in VRAM in
jade 2.0.14 and dovata 3.2.3, and the NVIDIA kernels exist in jade 2.0.15. The CPU side,
step 4, is next. Joe asked for Apple, NVIDIA and AMD GPUs to each get their own folder
and their own implementation, and for a design first. The product machine has an RTX 2000 Ada, 16 GB, and the demo and test
machine an RTX 3090, 24 GB, both on Linux; there is no AMD machine yet. The decisions
below are recommendations until Joe agrees to them, and the questions at the end need
Joe's answer before code moves.

## The problem

Inference runs on one GPU today, Apple's, and every part of it assumes that GPU:

- `codegen/infer/metal.rs` is the only code that talks to a GPU, and
  `src/infer/kernels.metal` holds the only kernels.
- `codegen/infer/map.rs` opens each region with macOS's `open` and `shm_open`, and reads
  `struct stat` at macOS's offsets.
- The step in `metal.rs` writes its inputs straight into the model's memory, and
  `codegen/infer/next.rs` reads the logits straight out of it. Both work only because an
  Apple GPU and its CPU share one pool of memory.
- `build::model_plan` refuses any machine that is not an Apple Silicon Mac.

On dovata's side, the planner already plans for one NVIDIA GPU, with every tensor in
VRAM, but the dovata process refuses to hold a CUDA plan, in `host/server.rs`. There is no
AMD target at all.

The three GPUs differ in the ways that matter here:

| | Apple | NVIDIA | AMD |
|---|---|---|---|
| memory | one pool the CPU and GPU share | VRAM on the card, across PCIe | VRAM on the card, across PCIe |
| what the program calls | Metal, an Objective-C framework | the CUDA driver API, `libcuda.so.1` | HIP, `libamdhip64.so.7` or `.6` |
| kernels | Metal source, compiled by the driver when the program starts | PTX from LLVM's NVPTX backend, compiled by the driver for the card when loaded | a code object from LLVM's AMDGPU backend, built for one GPU architecture |
| what `jade build` can show | Metal source | PTX | the GPU's own assembly |
| threads that run in lockstep | a SIMD group of 32 | a warp of 32 | a wavefront of 64 on CDNA, 32 on RDNA |

Sources: NVIDIA says PTX loaded at run time "is compiled to binary code by the device
driver", in the [CUDA programming guide](https://docs.nvidia.com/cuda/cuda-programming-guide/01-introduction/cuda-platform.html).
AMD says a code object holds "fully compiled code for a device-specific instruction set",
in the [HIP porting guide](https://rocm.docs.amd.com/projects/HIP/en/latest/how-to/hip_porting_guide.html).
LLVM 18's [AMDGPU usage page](https://releases.llvm.org/18.1.0/docs/AMDGPUUsage.html)
says GFX9, which includes CDNA, always runs wavefronts of 64, and GFX10 and GFX11, which
are RDNA, run 32 natively. Fedora's [ROCm HIP package](https://packages.fedoraproject.org/pkgs/rocclr/rocm-hip/fedora-43.html)
ships `libamdhip64.so.6` and `.so.7`, so the library's version follows ROCm's.

## The layout

On 2026-11-16 Joe asked for the kernels to sit apart from the CPU code, in a folder of
their own named for what they are, so codegen's two kinds of code are clear: the CPU code
that opens the model's memory and launches each step, and the GPU code each step runs.
The layout below replaces the first draft's, which put each GPU's kernels inside its
folder under `codegen/infer/`. The kernels part was built on branch `2.0.15`.

```
compiler/src/codegen/gpu_kernels/    the GPU code, built in 2.0.15
  mod.rs          the argument record's size; every kernel written in KERNELS order
  builder.rs      K, the builder LLVM kernels are written in; the per-GPU methods:
                  thread and block ids, warp sums, barriers, shared memory, fast math
  values.rs       reading each type; decoding f16, bf16, q8_0, q4_0, q4_K, q5_K, q6_K
  elementwise.rs  get_rows, mul, add, rope, cpy, set_rows, swiglu
  norm.rs         rms_norm
  matmul.rs       mul_mat, one warp per output, and mul_mm, 32 by 32 tiles
  attention.rs    flash_attn
  apple/          kernels.metal, moved from src/infer/
  nvidia/         the kernels built through NVPTX into PTX, and the GPU test
  amd/            later: the same kernels through AMDGPU into a code object

compiler/src/codegen/infer/          the CPU code
  mod.rs          infer_op, the lock, the model's globals; picks the GPU
  tables.rs       the tokenizer's tables, unchanged
  next.rs         the generation loop and argmax, over what the GPU gives it
  step.rs         the step's inputs: ids, positions, cache cells, the mask
  apple/          setup and the step on Metal, from today's metal.rs; objc_msgSend;
                  today's map.rs, with macOS's constants
  nvidia/         setup and the step on CUDA: libcuda.so.1 opened with dlopen, each
                  region opened from the handle dovata's record gives
  amd/            later: the same through HIP
```

`compiler/src/infer/` keeps what runs inside the compiler at build time: `plan.rs`,
`rewrite.rs`, `prelude.jde` and `unicode.rs`. `plan.rs`'s `KERNELS` list stays as the
contract between the plan and the GPUs: an op record names kernel number `k`, and every
GPU has a kernel for every number.

## What every GPU folder answers

`mod.rs`, `next.rs` and `step.rs` call five functions, and nothing else of a GPU folder:

```rust
impl<'ctx> Cx<'ctx> {
    /// Makes the model's memory reachable: maps part files and shared memory on
    /// Apple, opens the VRAM dovata holds on NVIDIA and AMD.
    fn gpu_open_fn(&self) -> FunctionValue<'ctx>;

    /// The device, the kernels, and a buffer or device pointer for each region.
    fn gpu_setup_fn(&self) -> FunctionValue<'ctx>;

    /// Where the step writes input `a` on the host before a step runs.
    fn gpu_input(&self, f: &F<'_, 'ctx>, a: Addr) -> PointerValue<'ctx>;

    /// One step of `n` tokens from position `pos`: send the inputs, launch every
    /// op in the table, wait, and bring the logits back.
    fn gpu_step_fn(&self) -> FunctionValue<'ctx>;

    /// Where the host reads the logits once a step is done.
    fn gpu_logits(&self, f: &F<'_, 'ctx>) -> PointerValue<'ctx>;
}
```

Each one is a match on the plan's GPU, the way the rest of codegen picks per type:

```rust
fn gpu_step_fn(&self) -> FunctionValue<'ctx> {
    match self.plan().gpu {
        Gpu::Apple => self.apple_step_fn(),
        Gpu::Nvidia => self.nvidia_step_fn(),
        Gpu::Amd => self.amd_step_fn(),
    }
}
```

On Apple, `gpu_input` and `gpu_logits` are today's `host_addr`: the model's memory
itself, so nothing is copied. On NVIDIA and AMD they point into a staging buffer in host
memory, and the step copies the inputs up before the first launch and the logits down
after the last. Filling the inputs is the same on every GPU, so it moves out of
`metal.rs` into `step.rs`, written against `gpu_input`.

## How a program picks its GPU

From dovata's record, not from the machine the compiler runs on. The plan already says
what it was planned for, `Target::Apple` or `Target::Cuda`, so `InferPlan` gains a `gpu`
field read from it. `build::model_plan`'s macOS check becomes a check of that field: a
target with no folder yet is refused by name.

A program that runs a model is built on the machine whose dovata holds the model, since
the program opens that dovata's memory. So the GPU in the record is always a GPU on the
build machine, and building a model program for another machine is not a goal.

## Decisions

### 1. NVIDIA and AMD kernels are built through LLVM, by codegen

Three ways to write the kernels were considered:

- *Through LLVM, from Rust builder code*, the way all of codegen is written. LLVM 18,
  which jade already links, has the NVPTX and AMDGPU backends built in: NVPTX gives PTX
  text, and AMDGPU gives an object that LLD, also already inside jade, links into a code
  object. The program needs only the driver, and `jade build` can print the kernels.
- *CUDA C, compiled when the program starts*, through NVRTC, the way Metal source is
  compiled today. It is the shortest port from `kernels.metal`. But NVRTC ships with the
  CUDA toolkit, not the driver, so every machine that runs a program would need the
  toolkit; jade could not show the kernels' assembly; and AMD would need the same again
  through hipRTC.
- *Kernels written in Jade*, lowered through MIR. That is where v2 points in the long
  run, but Jade has no thread ids, shared memory or barriers, and a kernel cannot raise
  an error or count references. It is a language design of its own.

The recommendation is the first.

PTX names the oldest GPU generation it runs on, and each generation's PTX needs a
driver at least as new. LLVM 18 builds for `sm_30` unless told otherwise, and the PTX
version follows the target: `sm_80`, NVIDIA's Ampere generation, gives PTX 7.0, which
needs driver r445 or later. The newest LLVM 18 knows are `sm_90a` and PTX 8.3. Which
generation jade builds for is question 6.

Sources: LLVM 18's [NVPTX usage page](https://releases.llvm.org/18.1.0/docs/NVPTXUsage.html)
and its target list, [`NVPTX.td`](https://github.com/llvm/llvm-project/blob/release/18.x/llvm/lib/Target/NVPTX/NVPTX.td).
LLVM 18's [AMDGPU usage page](https://releases.llvm.org/18.1.0/docs/AMDGPUUsage.html)
says the backend's relocatable ELF is "linked by lld to produce a standard ELF shared
code object". NVIDIA says "NVRTC is part of the CUDA Toolkit release" in the
[NVRTC guide](https://docs.nvidia.com/cuda/nvrtc/index.html), and that the driver API's
library is installed with the device driver, in the
[driver API chapter](https://docs.nvidia.com/cuda/cuda-programming-guide/03-advanced/driver-api.html).
The minimum driver for each PTX version is in the
[PTX ISA's release table](https://docs.nvidia.com/cuda/parallel-thread-execution/index.html).

### 2. The plain kernels are written once for NVIDIA and AMD, and each folder owns what it tunes

Each folder owns its kernels, as asked. But the first version of every kernel is the
same math on both, and the two differ only in which intrinsic answers "which thread am
I", "wait for the block" and "add across the warp". So `gpu/builder.rs` answers those
questions per vendor, and the first `amd/kernels/` calls the same kernel-writing
functions as `nvidia/kernels/`:

```rust
// gpu/builder.rs
pub(super) enum Vendor { Nvidia, Amd }

impl<'ctx> K<'_, 'ctx> {
    /// The thread's index in its block.
    fn thread_x(&self) -> IntValue<'ctx> {
        match self.vendor {
            Vendor::Nvidia => self.intrinsic("llvm.nvvm.read.ptx.sreg.tid.x"),
            Vendor::Amd => self.intrinsic("llvm.amdgcn.workitem.id.x"),
        }
    }

    /// Waits until every thread in the block reaches this point.
    fn barrier(&self) {
        match self.vendor {
            Vendor::Nvidia => self.call_intrinsic("llvm.nvvm.barrier0"),
            Vendor::Amd => self.call_intrinsic("llvm.amdgcn.s.barrier"),
        }
    }
}
```

Tuning replaces a kernel in one folder only: matrix multiplication on NVIDIA's tensor
cores, or on AMD's matrix instructions. The other choice is two full copies from the
start: twice the code, and two sets of bugs, with no AMD machine to find AMD's.

One difference the builder has to carry is the width of a warp. `plan.rs` gives a row
to a group of 32 threads, which is one warp on NVIDIA and one wavefront on RDNA, but
half a wavefront on CDNA, AMD's data center GPUs. Adding across a group then needs a
shuffle of width 32 inside the wavefront, which `gpu/builder.rs` writes per vendor.

Sources: the thread id and barrier intrinsics are on LLVM 18's
[NVPTX usage page](https://releases.llvm.org/18.1.0/docs/NVPTXUsage.html) and in
[`IntrinsicsAMDGPU.td`](https://github.com/llvm/llvm-project/blob/release/18.x/llvm/include/llvm/IR/IntrinsicsAMDGPU.td);
NVIDIA's warp shuffles, `llvm.nvvm.shfl.sync.*`, are in
[`IntrinsicsNVVM.td`](https://github.com/llvm/llvm-project/blob/release/18.x/llvm/include/llvm/IR/IntrinsicsNVVM.td).

### 3. dovata holds the VRAM, and its record carries a handle for each region

On Apple the program maps dovata's memory by name. VRAM cannot be mapped by name: one
process reaches another's device memory through the driver's IPC handles. So dovata,
holding a CUDA plan, allocates each region with `cuMemAlloc`, copies the weights in from
the GGUF file, makes a handle for each region with `cuIpcGetMemHandle`, and writes the
handle into the model's record. The compiler writes the handle's bytes into the program
as a constant, the way it writes a shared memory object's name today, and the program
opens it with `cuIpcOpenMemHandle`. No message goes to dovata, so dovata still has no
protocol.

A handle is 64 bytes, and NVIDIA says it can be passed through shared memory or files,
so a record can carry it. Three rules come with it:

- *A handle shares a whole allocation.* `cuIpcGetMemHandle` takes the start of a
  `cuMemAlloc` allocation, and NVIDIA suggests sizes in multiples of 2 MiB. So each
  region is its own allocation, rounded up to 2 MiB.
- *dovata must not free a region a program still has open.* Freeing it in the exporting
  process before the other process closes its handle is undefined: it may work, fault,
  or let the program read whatever the driver puts in that memory next, such as another
  model's weights, with no error. Decision 4 is how dovata knows when nobody holds a
  region.
- *What happens when dovata itself exits* while a program holds a handle is not in
  NVIDIA's documentation. The first test on Joe's machine should find out, and until then
  `dovata stop` should name the programs that still hold a model and wait for them.

HIP's handles follow the same rules, are also 64 bytes, and AMD notes the opened
address can differ from the exporting process's, so a program never assumes it is the
same.

Sources: the handle's size is in the CUDA driver API's
[types](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__TYPES.html); passing it
through files, the 2 MiB advice and Linux only are in the CUDA programming guide's
[IPC chapter](https://docs.nvidia.com/cuda/cuda-programming-guide/04-special-topics/inter-process-communication.html);
`cuMemAlloc`'s start and the undefined free are in the driver API's
[memory functions](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html).
HIP's rules are in its [device functions](https://rocm.docs.amd.com/projects/HIP/en/latest/doxygen/html/group___device.html)
and the handle's size in its [global definitions](https://rocm.docs.amd.com/projects/HIP/en/latest/doxygen/html/group___global_defs.html).

Two other ways were considered:

- *Handles from CUDA's virtual memory API*, `cuMemExportToShareableHandle`. Its memory is
  counted, so dovata could free a region while a program still uses it, and the last
  user frees it. But its handle is a file descriptor, which reaches another process only
  over a Unix socket, the kind of protocol 2.0.3 removed. Its fabric handle is plain
  bytes, but needs CUDA 12.4 or later and a channel device the machine's administrator
  sets up. NVIDIA's [virtual memory chapter](https://docs.nvidia.com/cuda/cuda-programming-guide/04-special-topics/virtual-memory-management.html)
  sends the descriptor with `SCM_RIGHTS`, and the
  [fabric handle](https://docs.nvidia.com/cuda/cuda-driver-api/structCUmemFabricHandle__v1.html)
  lists its requirements.
- *Each program copies the weights itself*, from the GGUF file, at startup. Every start
  would copy the whole model across PCIe, two programs would hold two copies in VRAM, and
  dovata would no longer be what holds the model.

A change to the record is a change to both crates, so this bumps both versions.

### 4. dovata decides what is in VRAM, and the program follows

Programs and the compiler decide nothing about memory. A program's addresses are
virtual, and dovata manages VRAM as a cache over RAM, as one pool, with the GPU doing all
the computation. [`dovata-vram-cache.md`](dovata-vram-cache.md) is that design: the limit
it aims at, what stays resident, the leases that keep dovata from freeing memory a
program still reads, and who issues the copies. An earlier draft of this decision put
counted holds in the prelude; Joe moved the whole question into dovata.

### 5. The program opens the driver with dlopen

A program links the C library alone today. On Linux, a program that runs a model opens
`libcuda.so.1` with `dlopen` at setup and looks up each driver function once. A machine
without NVIDIA's driver gets an error naming the driver, and a program that never runs a
model does not change at all. Since glibc 2.34, `dlopen` is part of libc itself, so
nothing more is linked. Apple keeps linking Metal as it does now. On AMD, the program
tries `libamdhip64.so.7`, then `.so.6`, since the number follows ROCm's major version.

Sources: the driver installs the driver API's library, per NVIDIA's
[driver API chapter](https://docs.nvidia.com/cuda/cuda-programming-guide/03-advanced/driver-api.html);
NVIDIA's own [cuda-python library catalog](https://github.com/NVIDIA/cuda-python/blob/main/cuda_pathfinder/cuda/pathfinder/_dynamic_libs/descriptor_catalog.py)
loads it as `libcuda.so.1`. The [glibc 2.34 release announcement](https://sourceware.org/pipermail/libc-alpha/2021-August/129718.html)
says libdl's functions moved into libc.

### 6. The inputs and logits cross PCIe once a step

A step of one token sends 4 bytes of token id, 4 of position, 16 of cache cells, a mask
row of `n_ctx` f16s, which is 8 KiB at 4,096 cells, and 4 bytes naming the output row.
It brings back the logits: one f32 per token in the vocabulary, so 593 KiB for Qwen3's
151,936 tokens. At 20 GB/s that is about 30 microseconds, small next to a step. A later
patch can pick the token on the GPU and bring back 8 bytes; `kernels.metal` already has
an `argmax` kernel that nothing launches.

### 7. `--emit kernels` shows each GPU's code

`jade build app.jde --emit kernels` would print the kernels a program that runs a model
carries: PTX on NVIDIA, AMD GPU assembly on AMD, and the Metal source on Apple, which is
the most Apple allows. `--emit asm` stays this machine's CPU assembly. The kernels are
the same for every program built against one plan, and long, so they get their own flag
rather than following the CPU assembly.

### 8. AMD waits for a dovata target and a machine

dovata has no AMD target. Adding one means `Target::Amd { vram_bytes, arch }`, where
`arch` is the GPU's architecture name, such as `gfx1100`, since an AMD code object is
built for one architecture; finding AMD GPUs in `machine/`; and allocating and sharing
VRAM through HIP. The record would carry `arch`, read from HIP's `gcnArchName`, and the
compiler builds the code object for it. LLVM 19 adds targets such as `gfx11-generic`
that run on a whole family, per its [AMDGPU usage page](https://releases.llvm.org/19.1.0/docs/AMDGPUUsage.html),
so moving past LLVM 18 would let one code object serve several cards.

The `amd/` folder exists from the first patch with a README, and is written after
NVIDIA runs, once there is an AMD machine to test on. Until then CI can check that the
AMD kernels build into a code object, and nothing more.

## The order of the work

Each step is its own patch, on a branch named for its version:

1. *The split*, compiler only. `apple/` from `metal.rs`, `map.rs` and `kernels.metal`;
   the five functions; `step.rs`; `InferPlan.gpu` from the record; `nvidia/` and `amd/`
   with a README each; the refusal names the missing GPU. Nothing changes on a Mac:
   `infer::` tests, the golden outputs, and the IR of a program that runs a model are the
   same as before.
2. *dovata holds a CUDA plan*, both crates: VRAM regions, the weights copied in, a handle
   per region in the record, and the collector: leases in the clients directory, and a
   removed model freed when no live lease is left. Tested on Joe's NVIDIA machine with a checker in the spirit
   of `refback`, which reads every tensor back and compares it with the GGUF file.
   Written on branch `2.0.14` with dovata 3.2.3; the checker is `dovata check <name>`.
3. *The kernels*, compiler: `codegen/gpu_kernels/`, with the builder, the value readers
   and every kernel, built into PTX for `sm_86`. Built on branch `2.0.15`. Checked on the
   Mac and in CI two ways: every kernel builds into PTX with a named entry point and no
   outside calls, and every kernel runs on the CPU, one thread per GPU thread, over 47
   cases compared with f64 math. On an NVIDIA machine the same cases run on the GPU, which
   is the first test on Joe's machine. Rope's record gained float parameter 3,
   `freq_base^(-2/n_dims)`, so the kernel need not call a fast pow whose error grows with
   the position. A review found that both GPUs' rope ignored Llama 3.1's frequency
   factors, rope's third input; both read them now, flagged by integer parameter 2. `--emit kernels` waits for step 4, since a program cannot be built
   against a model in VRAM until then.
4. *The NVIDIA host code*, compiler: `driver.rs`, `open.rs`, setup and the step, and
   the lease and the thread that answers dovata's signal, from `dovata-vram-cache.md`,
   with Linux's open flags. Before it opens a handle, a program writes its lease and
   reads dovata's record again, the way dovata 3.2.3's `map` does; a handle compiled into
   the program does not settle by itself whether dovata still holds the memory. Tested on Joe's
   machine with `infer::tests`, which compare a program's tokens and greedy reply with
   llama.cpp's on the same GGUF file, against a llama.cpp built for CUDA. The step skips
   an op with no threads, as Metal's does, since `cuLaunchKernel` refuses an empty grid.
   That test is also the first to run the kernels on a real plan's memory: tensors at
   offsets inside a region, views where dovata placed them, and `set_rows` writing into
   its own `src0`.
5. *Tuning NVIDIA*: tensor cores, the token picked on the GPU, f16 tiles.
6. *AMD*, once there is a machine to test on.

## What stays the same

- The prelude, the tokenizer, the plan's op table and its kernel numbers.
- Everything a program does on a Mac.
- Programs that never run a model.

## Out of scope

- More than one GPU. dovata plans for one.
- A model larger than the GPU's memory. dovata refuses one today.
- Building a model program for another machine.
- Kernels written in Jade.
- A CPU folder. dovata plans for the CPU too, and a `cpu/` folder would answer the same
  five functions, but nobody has asked for one.

## Questions for Joe

1. Kernels built through LLVM from Rust builder code, rather than CUDA C through NVRTC or
   kernels in Jade? See decision 1.
2. One set of plain kernels for NVIDIA and AMD through `gpu/`, each folder owning what it
   tunes, or two full copies from the start? See decision 2.
3. dovata holds the VRAM and gives a handle per region in its record? See decision 3.
4. The questions in [`dovata-vram-cache.md`](dovata-vram-cache.md), which decision 4
   points to.
5. `--emit kernels` as its own flag, or the kernels inside `--emit asm`? See decision 7.
6. Build PTX for `sm_86`? The RTX 3090 is compute capability 8.6 and the RTX 2000 Ada is
   8.9, per [NVIDIA's list](https://developer.nvidia.com/cuda-gpus), so `sm_86` runs on
   both, and the driver compiles it for the newer card when it loads.
