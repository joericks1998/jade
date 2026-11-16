# `src/codegen/sys/`: the hooks behind the std library

## What this subtree is

The std library is written in Jade, in [`src/lib/`](../../lib/README.md). What it cannot do in Jade it asks of `__sys`, a hidden package of hooks, and each hook arrives here as `Op::Sys`: one C library call, one LLVM intrinsic, or one value operation Jade has no spelling for. `SysFn` in `mir/mod.rs` is the one place each hook's type is written.

## Why it was built

Before 2.0.8 each std package was written here whole, as Rust that built instructions. Moving the packages into Jade left only what has to touch the C library or hold the program's shared state, and that is what these files are. Every trust check lives in a hook, so library code can neither skip a check nor launder a value.

## What each file does

- *`mod.rs`* has the hooks, `Op::Sys`: what the std library written in Jade, `src/lib/`, asks of the C library and LLVM. `sys_op` emits every hook, calling into the three files below for theirs. The float hooks are one LLVM intrinsic or C math call each, plus `llvm.fptosi.sat` and a bit cast; the int hooks saturate or wrap; the clock hooks read `clock_gettime`; `sleep` is `nanosleep`; the time-zone hooks set `TZ` for a moment under a spin lock and read `localtime_r`'s offset and `strftime`'s `%Z`, tainted. It also has the error slot and its helpers, `strerror`, `lossy`, which exposes `bytes.rs`'s lossy decoder, `keys_in_order`, which reads a dict's keys in the order they were added, the environment hooks, `getenv` tainted and `args` and `cwd` trusted, and the refusal of a tainted string, `refuse_tainted` and `refuse_tainted_in`, whose text is v1's. `random.rs` shares the spin lock and the wall clock, and `../support/tasks.rs` the sleep. Until 2.0.8 `time.rs`, `math.rs`, `fs.rs`, `sh.rs`, `env.rs`, `random.rs` and `http.rs` held whole packages here, written as calls that build instructions; their logic is Jade now, in `src/lib/`.
- *`fs.rs`* has the file-system hooks behind `fs.jde`: `read_text` and `read_file`, which refuse a tainted path unless the call trusts it and taint what they read unless it does, `write_file`, `stat`, `unlink`, `rmdir`, `mkdir`, `rename`, `copy_file`, with v1's errors at the moments v1 read them, and `read_dir`. The `struct stat` and `struct dirent` offsets per target are here.
- *`process.rs`* has the process hooks behind `sh.jde`, `http.jde` and `uhttp.jde`: `shell`, which starts `/bin/sh -c` and refuses a tainted command, `spawn`, which starts curl from `PATH` with its standard error going nowhere, the reads, `read_all`, `read_chunk` and `drain`, which reads two pipes together with `poll`, all tainted, and `write_fd`, `write_child` with `SIGPIPE` ignored, `close`, `wait` and `kill`.
- *`random.rs`* has the generator behind `random.jde`: v1's xoshiro256** seeded through splitmix64, under a spin lock, reached through `rand_below`, which draws below a bound without modulo bias, and `rand_seed`, so a seeded program draws the same numbers.

## Who uses it

*Used by:* `../func.rs`, which calls `sys_op` for every `Op::Sys`, and `../libcache/plan.rs`, which asks `is_one_op` whether a hook is one instruction, so the library function around it is copied into its callers rather than cached.

*Depends on:* `../support/` for strings and arrays, `../shared.rs` for the error-code slot, the spin lock and the random generator's state, and `../layout.rs`.

## Gotchas

*A new hook needs three places.* Its type in `SysFn` in `mir/mod.rs`, its code in `sys_op` here, and its C functions in `Cx::libc`'s table and the pinned list in `../tests.rs`. [`src/lib/README.md`](../../lib/README.md) has the steps.

*A hook that brings outside text in taints it, and a hook that runs a command or reads a path refuses tainted text.* The checks are here, not in the library's Jade code, on purpose.

## Building and testing

```sh
cargo test codegen::
./compiler/src/scripts/golden.sh
```
