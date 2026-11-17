# dovata: VRAM as a cache over RAM

Status: proposed 2026-11-15, for review. Nothing here is built. It covers discrete GPUs,
NVIDIA first; on Apple, RAM and the GPU's memory are already one pool.

Joe set the direction:

- *dovata treats RAM and VRAM as one pool*, and manages it aggressively enough to come as
  close as the hardware allows to running from VRAM alone.
- *The GPU does all the computation.* Joe's experiments found that the CPU's slowness
  outweighs what running part of a model on it saves, so there are no CPU kernels.
- *Programs and the compiler decide nothing about memory.* A program's addresses are
  virtual; dovata decides what is in VRAM.

So the job is a cache: VRAM holds the bytes the GPU reads most, RAM holds everything, and
dovata decides what moves when. This doc replaces decision 4 of
[`v2-gpu-targets.md`](v2-gpu-targets.md), which put counted holds in the prelude.

## The limit

Each token, the GPU reads every weight it uses. A byte already in VRAM comes at VRAM
speed; a byte that is not must cross PCIe first. With copies running while the GPU
computes, a token takes the longer of the two:

```
time per token ≥ max( resident bytes / VRAM speed,  missing bytes / PCIe speed )
```

| | RTX 2000 Ada, product | RTX 3090, demo and test |
|---|---|---|
| VRAM | 16 GB | 24 GB |
| VRAM speed | 224 GB/s | 936 GB/s |
| PCIe | 4.0 x8, about 16 GB/s | 4.0 x16, about 32 GB/s |
| one GB missing from VRAM costs, per token | about 62 ms | about 31 ms |

Qwen3 32B is 19.8 GB at Q4_K_M. On the product card about 14 GB of it fits beside the
cache, so if every token pulls the other 5.8 GB across again:

```
max( 14 GB / 224 GB/s,  5.8 GB / 16 GB/s ) = max( 62 ms, 362 ms ) = 362 ms a token
```

That is the number to beat. Every lever below does one of two things: makes fewer bytes
cross per token, or makes each byte that crosses serve more than one token.

`dovata plan` should print this bound for each model on each machine, so every change is
measured against the hardware rather than against the last version.

## The levers

### 1. Each byte that crosses serves several tokens

A layer copied into VRAM serves every token in the step that reads it. Reading a prompt
already runs hundreds of tokens per step, so it pays the copy once for all of them.
Generating a reply runs one token per step, so it pays the copy every token. Two ways to
put more tokens in a generating step:

- *Speculative decoding.* A small draft model, kept wholly in VRAM, guesses the next few
  tokens, and the large model checks all of them in one step, keeping the ones it agrees
  with. Each streamed byte then serves as many tokens as the draft guessed right. It
  gives the same reply the large model alone would; see
  [Leviathan and others, 2023](https://arxiv.org/abs/2211.17192).
- *Batching conversations.* Several conversations on one model step together, so one copy
  serves each of them. Today a model holds one conversation at a time, so this needs a
  cache per conversation first.

### 2. The right things stay resident

The cache's policy, by kind of memory:

- *A dense model's layers are read in the same order every token,* so least recently used
  is the wrong rule. With room for 3 of 4 layers, LRU always evicts the layer needed next:

  ```
  read 1 2 3 → full; read 4, evict 1; read 1, evict 2; read 2, evict 3 ...
  every read misses
  ```

  Instead dovata pins as many whole layers as fit and streams the same remaining layers
  through a few staging slots every token. Three of four layers then hit every time.
- *Experts* in a mixture-of-experts model are kept by how often they are picked. The
  program's kernels count picks in a table in shared memory; that is a signal, and dovata
  makes the decision.
- *The KV cache* moves in blocks of tokens. A live conversation's blocks stay; an idle
  one's go to RAM and come back when it speaks.
- *Whole models* leave VRAM when nothing has used them for a while and another needs the
  room. Weights never change and are already in RAM, so leaving copies nothing back.

### 3. PCIe never idles and runs at full speed

- The layer order is known, so the next missing layer starts copying while the current
  one computes, into a second staging slot.
- Copies run at full speed only from page-locked host memory, so dovata locks its RAM
  copies, with `mlock` in its own process; a program that copies from them registers its
  mapping with `cuMemHostRegister`, which "page-locks the memory range" per the
  [driver API](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html).
- Copies go in large pieces, on a stream of their own, so the GPU's copy engine works
  while its compute units do.

### 4. Fewer bytes per weight

The RAM copy stays quantized, so a Q4_K_M weight crosses PCIe as about 5 bits, not 16:
Qwen3 8B's Q4_K_M file is 5.03 GB for 8.2 billion weights.
A colder layer could cross in a smaller quantization still, which trades answer quality
for speed. That is Joe's call; nothing here does it unless he says so.

## Who does what

- *dovata* owns all of it: one VRAM pool from the driver, the RAM copies in locked shared
  memory, the placement table, and every decision about what is resident. It publishes
  the table with a generation number in a small shared page that programs map by name,
  and it frees nothing a program might still read, by the leases below.
- *The program* runs every op on the GPU and follows the table. Before a layer's kernels
  it makes sure the layer is where the table says.
- *The compiler* writes the step and the kernels. It decides nothing about placement.

One question is who issues the copies:

- *The program issues them,* following the table: the copy for layer i + 1 goes on its
  copy stream while layer i computes. Fast and simple, since the program already runs
  everything else on its GPU stream. The program does the copy, but dovata chose it.
- *dovata issues them* in its own process, and the program's GPU waits on dovata's
  events before each layer. CUDA shares events between processes with
  `cuIpcGetEventHandle`, in the
  [driver API's memory functions](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html).
  The program then does no copying at all, but each layer needs a handshake in both
  directions: dovata's "layer ready" and the program's "slot free".

The recommendation is the first, measured against the second on the 3090 if the
handshake turns out cheap.

## Leases: freeing nothing a program might read

No CUDA call lets dovata unmap another process's memory or ask who still maps it, and
freeing memory another process has open is undefined, per the
[driver API](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html). So the
program says what it holds, and dovata waits for it:

1. Before it opens dovata's memory, a program writes a lease in the model's clients
   directory: a file named by its pid, holding the process's start time, so a pid the
   system reuses is not mistaken for it.
2. When dovata moves something, it bumps the table's generation and signals each program
   with a lease. A thread in the program waits for that signal, takes the model's lock,
   which is free between tokens, closes what the new table no longer uses, and writes the
   generation it has caught up to in its lease. It answers within one token, even while
   the program's own code waits forever on input.
3. dovata frees old memory once every live lease has caught up, and drops the lease of a
   process that has exited.

None of it is the compiler's: the thread and the lease are part of the NVIDIA support code,
as mapping the model is part of Apple's today.

## What changes

- *dovata* plans VRAM as resident layers plus staging slots, not one fixed placement, and
  stops refusing models larger than VRAM. Its CUDA plan's note that partial placement is
  deferred, in `dovata/src/memplan/README.md`, is what this settles.
- *The plan's addresses.* Today a tensor's region and offset are constants of the
  program. With a cache, a streamed layer's address is a staging slot that changes from
  token to token. The op table keeps naming tensors, and the program reads each one's
  current address from dovata's table when the step starts.
- *The compiler* writes no CPU kernels, and nothing about memory.

## Questions for Joe

1. Who issues the copies: the program, following dovata's table, or dovata, with the
   program's GPU waiting on its events?
2. Speculative decoding with a small resident draft model as the main way to make each
   streamed byte serve several tokens?
3. Colder layers in a smaller quantization, or never?
4. Keep one conversation per model for now, or move to a cache per conversation so
   conversations can share each streamed layer?
