# host: the dovata process, which holds models in memory

`host` runs dovata as one long-lived process that holds models in memory. The process
starts empty; each model is added to it, planned for this machine, and loaded, and stays
until it is removed or the process stops. On a Mac or the CPU the memory is shared memory
and the model's files; on a machine with an NVIDIA GPU it is VRAM. A compiled Jade
program maps a model's memory into itself and runs its own kernels on it. dovata still never runs a model's math: the
process holds memory, keeps a record of it and reports on it, and the program does the
compute.

```rust
use dovata::host::{self, AddOptions, StartOptions};

// Start the process, with no models, or find the one already running.
let dovata = host::start(&StartOptions::default()).or_else(|_| host::connect())?;

// Add a model: download it if needed, plan it for this machine, load it.
let model = dovata.add("https://huggingface.co/unsloth/Qwen3-0.6B-GGUF", &AddOptions::default())?;

// Read what the process holds: where each region of the model's memory is, and its
// plan. Reading takes no message to the process, and works in every build.
let held = dovata.model(&model.name)?;
let plan = dovata.plan(&model.name)?;

// Map it: every region of its plan, where the program's compiled offsets expect it. A
// program whose offsets came from a different plan is refused.
let mem = dovata.map(&model.name, &plan.table_hash)?;
let region = mem.region(0).unwrap();
let weights = region.as_ptr();       // in this program's memory: bind as a no-copy MTLBuffer, say
let on_gpu = region.device_ptr();    // or in VRAM: a CUDA device address a kernel binds

// Watch, take the model out, stop.
let status = dovata.status()?;                   // every model, every region, how much is resident
dovata.remove(&model.name)?;
dovata.kill()?;
```

The command line does the same: `dovata start`, `add`, `remove`, `status` and `kill`, in
[`src/cli/`](../cli/README.md). Both reach the same process, so a model added from one
is seen, mapped and removed from the other.

## Why it exists

dovata 3.0 began as a build-time crate only: the Jade compiler planned a model with it,
and the compiled program set up the plan's memory itself at startup. That made each
program load the model on its own, and left nothing on the machine that owned the
model's memory or could say where it was. Since 2026-11-03, dovata is also the process
that owns that memory, started and stopped from the command line or from code, with the
monitoring commands watching it live.

There is one dovata process, never one per model. Every model's memory then goes
through one place, which can see all of it and refuse a model that does not fit beside
the others. That is also what lets one process hold several models.

The 2.x daemon was removed because a request crossing a process boundary on every
inference step was pure cost. This design keeps the boundary out of the step, and since
3.2.2 out of everything else a program does too. The process writes down where each
model's memory is, and the memory itself has names any program of the same user can
open: the model's files by their paths, and each allocation as a named shared memory
object. A program, or the Jade compiler building one, reads the record and opens the
memory itself. From then on its kernels read and write the same physical pages the
process holds.

Up to 3.2.1 a program asked the process for the memory over a Unix socket, in JSON, and
got file descriptors back. Every compiled program carried a socket client and a JSON
parser for that one exchange, and the compiler had its own copy of the protocol. Both
are gone: the compiler links this library and calls it, and a program needs nothing but
`open`, `shm_open` and `mmap`.

## How it works

### One process per user, and its run directory

The run directory is `DOVATA_RUN_DIR` when it is set, else `~/.dovata/run`, so a
terminal, a launchd job, the compiler and a Jade program all find the same process. It
is readable by its owner only. It holds:

| Path | What it holds |
|---|---|
| `dovata.lock` | The lock the process holds for as long as it runs, and its process id. |
| `dovata.log` | The process's log. |
| `process` | The process's record: its id, its dovata version, when it started, and the format of these files. |
| `models/<name>/model` | A model's record, a [`HeldModel`]: its state, its plan's table hash, its part files as the process loaded them, and where each region of its memory is. |
| `models/<name>/plan` | The model's whole plan, the one `dovata plan --json` prints. |
| `models/<name>/clients/<pid>.<n>` | A lease for each mapping a program holds: named by the program's process id and a number, and holding when the program started. A compiled Jade program writes `<pid>` alone. |
| `requests/` | Requests programs leave for the process, and its answers. |

Records are JSON, written to a temporary file and renamed into place, so a reader sees a
whole record or the one before it. Each carries `FORMAT`; a library of another format
refuses them and says to use the same dovata version, rather than misreading them.

The process holds a lock on `dovata.lock` for as long as it runs, and the system
releases the lock when it exits, however it exits. That is how a program tells whether
dovata runs: it asks for a shared lock on the file, which is refused only while the
process holds its own. A lock it is given means nothing runs, whatever files a killed
process left. A process writes its record just after taking the lock and removes it just
before giving the lock back, so a program that finds the lock held with no record waits
half a second for one; a lock still held with no record after that is a dovata older
than 3.2.2, and the library says to stop it. A second process is refused once the lock
stays held for a fifth of a second, and is told the first one's process id, which the
lock file holds.

The run directory is per user, so two users on one machine each have their own process.
Neither sees the other's memory.

### Starting

`start` runs `dovata host` in the background, in a process group of its own so a Ctrl-C
at the terminal does not reach it. The process takes the lock, removes what a killed
process left behind, installs its signal handlers, writes its record, and writes one
line to standard output: ready, or the reason it stopped. `start` returns when that line
arrives. When nobody reads the line, because the program that started it has gone, the
process stops itself.

### Requests and signals

The process answers three requests: add a model, remove one, and stop. A program leaves
one as a file in `requests/`, named with an id and the request's kind, such as
`4211-1794616486864936000-1.add`, and sends the process SIGUSR1. The process's handler
writes one byte to a pipe the process waits on, which is all a signal handler can safely
do; the process wakes, renames each waiting request so it is taken once, and answers it
with a file of the same id, `.done`, or `.error` holding the reason. The program waits
for the answer. An add is answered on a thread of its own, since planning a large model
takes seconds, so a model loading never holds up the others.

The process also looks for requests every second, so a request whose signal never came
is still answered. A program waiting on an answer checks every second that the process
still runs, and says so when it stopped.

SIGTERM and SIGINT stop the process, the same as a stop request: every model is taken
out, the names of their shared memory are removed, and the process's records go. Answers
stay, so a program can read the answer to its stop, and the next start removes them.
`dovata kill` and `Process::kill` leave a stop request rather than send SIGTERM, so the
tests can run several processes on threads of one program and stop each alone.

### Adding a model

`add` gets the model the way `dovata plan` does: a local file, or one GGUF from the
Hugging Face Hub, the unquantized one unless `AddOptions::want` names another. It never
quantizes. It finds the model's `tokenizer.json` the way `dovata plan` does, unless
`AddOptions::tokenizer` names one, and leaves the process a `Spec`: the model's files,
its tokenizer, its name, its layout and the step to plan for. The process:

1. checks the tokenizer against the model's vocabulary and plans the model for this
   machine, as `dovata machine target` describes it, with the tokenizer in the plan;
2. sets up the plan's memory: each part file mapped once, read-only, and each allocated
   region as a named shared memory object, readable and writable by this user only, or
   on an NVIDIA machine as VRAM, described below. On Linux every page of shared memory
   is taken now, since the objects live on `/dev/shm`, which holds half the machine's
   memory by default and 64 MiB in a default container; a model that does not fit there
   is refused, where a write past the limit later would end the process with SIGBUS;
3. checks the model fits beside the ones it holds: the files and allocations of every
   model on the same kind of memory together must fit the new model's budget, Metal's
   working set on a Mac and VRAM on an NVIDIA GPU. The planner has already checked the
   model alone;
4. writes the model's plan and record, and answers;
5. reads every page of the model file and touches every page of each allocation on a
   thread of its own, so the whole model ends up in physical memory, rewriting the
   record as it goes so status shows the progress. A program may map the allocations
   while they load, so touching a page is an atomic `or` of zero, which changes no byte
   the program wrote. VRAM is zeroed and the weights copied into it instead.

`add` then waits until the record says the model is ready and returns its status. A
name is taken once: a second model of the same name is refused until the first is
removed, and one being added holds its name from the start, so two adds of one name
never make memory under the same names. Names come from the model, such as
`qwen3-0.6b`, or from `AddOptions::name`.

`remove` takes a model out. The process removes the names of its shared memory and its
record; programs that mapped the model keep their mappings, and the memory goes when the
last one unmaps it. VRAM stays until no lease holds it, below.

### VRAM on an NVIDIA GPU

On a machine with an NVIDIA GPU, `dovata machine target` is a CUDA target and the
planner puts every region in VRAM: the weights in one region, the KV cache and the
step's scratch in others. `--cpu` holds the model in host memory instead.

VRAM cannot be opened by name the way shared memory can. One process reaches another's
device memory through the driver's IPC handles, so the process:

1. opens NVIDIA's driver, `libcuda.so.1`, the first time a model goes in VRAM. It is
   opened at run time rather than linked, so the same `dovata` program runs on a machine
   without NVIDIA's driver and refuses only VRAM, saying to add the model with `--cpu`;
2. checks the model's VRAM fits in what the GPU has free right now, as the driver's
   `cuMemGetInfo` reports it, since other programs use the card too;
3. allocates each region with `cuMemAlloc`, rounded up to 2 MiB, the size NVIDIA
   suggests for memory shared between processes;
4. makes an IPC handle for each with `cuIpcGetMemHandle`, and writes it in the model's
   record as 128 hex digits, with the GPU's PCI bus id: `Open::Vram`. A PCI bus id names
   the same GPU in every process, whatever `CUDA_VISIBLE_DEVICES` each one sets;
5. zeroes each region with `cuMemsetD8`, and copies the weights in from the part files.
   Neighboring weights are read into a 64 MiB page-locked buffer at their planned
   offsets and cross to the GPU in one copy, so a model's thousands of small tensors do
   not each pay for a copy of their own. A memset returns before the GPU has done it, so
   the model is marked ready only after `cuCtxSynchronize`. A model removed while it
   loads stops copying.

A driver that cannot be opened is tried again on the next add, so dovata started before
NVIDIA's driver loaded needs no restart.

A program opens each region with `cuIpcOpenMemHandle` on the same GPU, and gets a device
address that may differ from the one in dovata's process. CUDA does not let the process
that made a handle open it, so a program that runs dovata on one of its own threads, as
`tests.rs` does, cannot map a model in VRAM; `tests.rs` plans for the CPU on an NVIDIA
machine, and `process_test.rs` checks VRAM from a process of its own.

The rules come from NVIDIA's documentation: the handle's 64 bytes in `cuda.h`, the
`cuMemAlloc` start and the 2 MiB advice in the
[IPC chapter](https://docs.nvidia.com/cuda/cuda-programming-guide/04-special-topics/inter-process-communication.html),
and the rest in the driver API's
[memory functions](https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__MEM.html).

### Leases: freeing no VRAM a program still holds

Freeing VRAM before every process that opened it has closed it is undefined in CUDA, and
no driver call lets dovata ask who has it open or take it back. So a program says what
it holds, and dovata waits for it:

- `map` writes a lease before it opens anything: a file in the model's clients
  directory, named by the program's process id and a number of its own, holding when
  the program started, from `/proc/<pid>/stat` on Linux. A process id the system gives
  to a later program does not match the start time, so the lease is not mistaken for
  it. The lease file goes when the `Mapping` is dropped.
- `remove` takes the model's record away first, then reads the leases. `map` writes its
  lease and then reads the record again, refusing if it is gone or changed. Either
  dovata reads the lease, or the program sees the record gone and opens nothing.
- A removed model in VRAM is retired: its clients directory stays, and its VRAM is freed
  once every lease that held it has gone or its program has exited. The process looks
  every second.
- A stop request is refused while a running program holds VRAM, and names the programs.
  It takes the records of models in VRAM away before reading their leases, the same as
  `remove`, and writes them back when it refuses. SIGTERM and SIGINT still stop the
  process, and log the programs they leave holding memory.
- A model of the same name added and removed meanwhile, such as one held with `--cpu`,
  leaves a retired model's clients directory in place, since its leases are there.

Shared memory needs none of it, since the system keeps an object until its last mapping
goes; a lease there only lists the program in `status`.

### Shared memory names

Each allocation's name is `/dovata.` followed by 16 hex digits of a SHA-256 over the run
directory, the model's name and the plan's table hash, then the region's id, such as
`/dovata.3e522bd2e329950d.1`. That fits the 31 bytes macOS allows. The same model added
with the same plan to a process in the same run directory always gets the same names, so
a program compiled against it keeps working after dovata restarts and the model is added
again. A plan with another table hash, such as a different KV cache size, gets other
names, so a program compiled against the first finds no memory to map rather than memory
its offsets do not fit.

A process that is killed cannot remove its names, and shared memory outlives the process
that made it until the machine restarts. The next start reads the records the killed
process left and removes every name in them, reading only the names, so a record of
another version still gives them. A process that stops, on a request, a signal, an error
or a panic, removes them itself, and waits for any add still planning to remove the
names it made.

Names are shared by every user of a machine, so another user could make an object under
a name dovata does not hold at the moment. A program and `map` both refuse an object
that is not this user's, or that anyone else may read or write.

### The tokenizer

`Process::tokenizer(name, table_hash)` gives a program the model's tokenizer, built
from the one its plan carries, to run in the program itself: encoding a prompt and
decoding each generated id need nothing from the process. It is refused for a plan with
another table hash, the same as `map`. Because the process plans with the tokenizer
exactly as `dovata plan` does, the hashes match for a program compiled against `dovata
plan`'s output. How the tokenizer works is in [`src/tokenize/`](../tokenize/README.md).

### Mapping

`map` names a model and the plan's table hash, reads the model's record, takes a lease,
and opens each region where the record says it is: a window of a part file by the file's
path, an allocation by its shared memory name, and VRAM by its IPC handle. Each region is mapped with `MAP_SHARED`: weights
read-only, allocations read-write. A write through one program's mapping is seen by
every other mapping of that model and by the process. Two models never share an
allocation. The mappings stay valid after the model is removed or the process stops.
`map` then makes the program's file in `clients/`.

The table hash check is what keeps the compiled offsets right. A program's offsets are
constants from the plan it was compiled against. If the process holds a plan with
another hash, `map` is refused rather than handing out memory the offsets do not fit.

Two more checks keep a program from crashing on a model file changed underneath it. The
record holds each part file's size and modification time as the process loaded it, and
`map` refuses a file whose size or time differs, since a file rewritten in place changes
the bytes under every mapping. It also checks each region against its file's size before
mapping it, since reading a mapped page past the end of a file crashes with SIGBUS. A
compiled Jade program makes the same checks itself, from the same record, read when it
was built.

A `MappedRegion` gives its base as a raw pointer, which is what a kernel binds, or for
VRAM as a CUDA device address. Its `read` and `write` methods copy bytes one at a time as
relaxed atomics, which is slow but stays defined while another thread or process writes
the same bytes, or for VRAM through the driver; they are for inputs, outputs and checks,
not for weights in bulk. `dovata check` reads every weight back through them.

On a Mac the program wraps each mapped region in a no-copy `MTLBuffer`, the way
[`src/refback/`](../refback/README.md) does. Every base is page-aligned, which Metal
requires; a window's base is moved by the same amount as its file offset is past a page.

### Status

`status` reads the process's record and every model's, and counts each part file's and
each region's bytes in physical memory right now with `mincore`, on a mapping of its own:
pages in memory belong to the file or object, not to one mapping, so a fresh mapping sees
the pages the process and programs read in. Mapped weights can be dropped by the system
under memory pressure and read back later, so a falling resident count shows memory
pressure on the model. It also lists every program that mapped each model, once each by
process id, and whether each is still running. `dovata machine usage` and `watch` show the
process and one line per model beside the machine's own readings. VRAM counts as
resident from the moment it is allocated, and a model in VRAM counts its regions alone:
its files were read once, to copy the weights in.

## Files

| File | What it does |
|---|---|
| `mod.rs` | The pure half, built and tested everywhere: `Process` and the calls that only read files (`models`, `model`, `plan`), `Spec`, `StartOptions`, `AddOptions`, `Status`, `ModelStatus`, `HeldModel` and the types in them, `HostError`, names, the run directory, the budget check, and what each region holds. With the `host` feature off, every call that starts the process, signals it or maps memory returns `HostError::NotBuilt`. |
| `state.rs` | The run directory's files, also built everywhere: their paths, writing them whole, reading them, finding the running process by its lock, the shared memory names, and leaving, taking and answering requests. |
| `server.rs` | `serve`, the process: the lock and the cleanup after a killed process, the table of models, planning, setting up and loading each model's memory, writing each model's record, and taking requests. |
| `signal.rs` | The signal handlers: SIGUSR1 wakes the process to take requests, SIGTERM and SIGINT stop it, each by writing a byte to a pipe it waits on; and `notify`, which sends SIGUSR1. |
| `client.rs` | `start`, and the calls a `Process` makes: add, remove and kill as requests, status read from the records, map opening each region by name or IPC handle under a lease, and the tokenizer. |
| `memory.rs` | Every unsafe memory call: `Map`, a mapping unmapped on drop; reading pages in; `mincore`; named shared memory objects, made, opened and removed; a file's size; whether a process is alive, and when it started. |
| `cuda.rs` | Every unsafe CUDA call: NVIDIA's driver opened with `dlopen`; a GPU and its primary context; VRAM allocated, zeroed, written and shared by IPC handle; page-locked buffers; and VRAM another process shared, opened, read and written. |
| `fixture.rs` | A tiny llama GGUF with f32 weights, for the tests. |
| `tests.rs` | Runs the process on a thread: it starts empty; every weight read through a mapping matches the file; the plan read back is the one the process made, every float exactly; a write shows in a second mapping of the same model and not in another model; several models share the process and each name is taken once; a removed model's mapping stays and its names go; a second process is refused; a killed process's names are removed by the next start; a request dovata cannot read is refused; a stop request stops its own process only, with two in one program; an abandoned start stops itself; a model that cannot be planned leaves the process running; and a model changed on disk is not handed out. |
| `process_test.rs` | An integration test against the real `dovata` program: start from code, add a model, map it, the command line's `start`, `add`, `status`, `check`, `remove` and `machine usage` against the same process, kill from code, start, add and kill from the command line, and SIGTERM stopping it and removing its names. On an NVIDIA machine it also holds the model in VRAM: every region opened by its handle, every weight read back and checked, a stop refused while the test holds VRAM, and a removed model's VRAM kept until the test lets go. |

## What it does not cover yet

- *A model larger than VRAM.* The planner refuses one. `design/dovata-vram-cache.md`
  proposes running VRAM as a cache over RAM.
- *What happens to a program holding VRAM when dovata exits,* by SIGTERM or a crash, is
  not in NVIDIA's documentation. A stop request waits for such programs; a test on an
  NVIDIA machine should find out what the driver does.
- *Leases from compiled Jade programs.* A program the compiler builds writes `<pid>`
  with no start time, and only on a Mac so far; its NVIDIA support code is to write a
  lease and read the record again the way `map` does.
- *The program's own CUDA context.* Each program that opens VRAM makes a context on the
  GPU, which takes VRAM of its own; dovata leaves nothing aside for it yet. `map` makes
  the GPU's primary context current on the calling thread and leaves it so, and a
  region's device address is good in that context.
- *Programs in another pid namespace,* such as a container. A lease is checked by
  process id and start time, which such a program does not share, so dovata would take
  it for exited. A lock each mapping holds on its lease file would cover it.
- *More than one GPU,* as for the planner. dovata uses CUDA's device 0, which on a
  machine with mixed GPUs may not be the GPU `nvidia-smi` lists first and the plan was
  made for.
- *Sharing between models.* Two models added from the same file map it twice; the page
  cache holds the file once, but the budget check counts it twice.
- *Moving memory while a model runs,* such as paging MoE experts. Nothing moves.
- *Keeping weights resident.* They are read in when the model is added, and the system
  may drop them under pressure; status shows it. Locking them with `mlock` is a later
  choice.
- *A model file truncated while mapped.* The checks above catch a change before mapping.
  A file cut short after a program has mapped it still crashes that program with SIGBUS
  on its next read past the new end; no system call on Linux prevents that. Replace a
  model file by writing a new one and renaming it into place, which leaves the old one
  intact for every mapping.
- *A shared memory object resized by another program.* Up to 3.2.1, Linux sealed each
  allocation at its size. A named object cannot be sealed, so a program of the same user
  could shrink one under the others; macOS refuses a second `ftruncate` of a shared
  memory object on its own.
- *Logging out on Linux.* systemd-logind removes a user's `/dev/shm` objects when their
  last session ends, by default. dovata keeps its memory, but programs can no longer open
  it by name; `loginctl enable-linger` keeps it, and the error says so.
- *Two programs running the same model at once.* They share its KV cache, as they did
  before.

## Who uses it

`src/cli/host.rs` builds `dovata start`, `add`, `remove`, `status`, `check`, `kill` and
the hidden `dovata host` on it, and `src/cli/machine.rs` adds the process and its models to `usage`
and `watch`. The Jade compiler takes this crate as a cargo dependency with no
capabilities turned on, and calls `connect`, `models`, `model`, `plan` and `clients_dir`
when it builds a program that dereferences a prompt; the program it builds opens the
memory by the names the record gave. `host` depends on `load`, `layout`, `memplan`,
`target`, `tokenize`, `hf_hub` and `machine`, on libc for the system calls, and at run
time on NVIDIA's driver library when a model goes in VRAM.
