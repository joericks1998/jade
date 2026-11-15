# `src/scripts/`: development and CI gates

## What this subtree is

These are the gates CI runs over the built `jade` binary, as opposed to the Rust unit tests `cargo test` runs. They answer one question between them: does a program that worked before still work, all the way to a running binary?

The directory lives under `src/` for tidiness rather than because it is source. Nothing here is compiled into the crate.

## Why they exist

Until v1.4.8, Jade had two engines: a bytecode VM behind `jade run` and the compiled LLVM backend behind `jade build`. The gate here was `backend-parity.sh`, which ran every example on both and diffed the output. The engines drifted apart three times, and parity caught each one.

v2 removed the VM, which leaves nothing to diff against. So before the VM went, each example's output was recorded beside it as `<name>.expected`, from a run where both engines agreed. That recording is the oracle now, and `golden.sh` holds the compiled backend to it.

## What each file does

- *`golden.sh`* builds every example, runs the binary, and compares its output and exit status to the `<name>.expected` and optional `<name>.expected-exit` beside it. An example with no `.expected` file fails rather than passes. It takes an optional path to a `jade` binary, and keeps a skip list: the examples that reach the real network or a Docker socket, `http/basic`, `uhttp/basic` and `uhttp/stream`, the `*_error.jde` fixtures that `jade check` covers, and the examples that dereference a prompt, whose recorded output came from v1's mock provider; a prompt now runs a real model, which `src/infer/tests.rs` checks against llama.cpp. Read the header before assuming an example is covered. Until 2.0.8 it skipped every example under `http/` and `uhttp/`; `http/local` and `uhttp/local` now drive the same request path against a stand-in for curl the fixture writes itself, so they run. The examples under `examples/ffi/rust_*` call Rust packages, so building them runs cargo.

  Until 2.0.5 it also built two C fixtures, a stand-in Jade-ABI package handing out handles and a C library passing structs through a generated shim. Both went with the C path; the Rust packages in `examples/ffi/` cover what crosses now, and they need no step of their own.
- *`ffi-gate.sh`* and *`ffi-gate/`* are the FFI gate, described below. [`ffi-gate/README.md`](ffi-gate/README.md) says what each of its files is.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `golden.sh` and `ffi-gate.sh` as required steps on every pull request. Run them locally before opening one.

*Depends on:* a built `jade` binary, defaulting to `./target/debug/jade`, the fixtures and `.expected` files in `examples/`, and cargo, for the examples and the gate that use Rust packages. A missing cargo makes the FFI gate report a skip rather than a silent pass. Paths are relative to the current directory, so run the scripts from the repository root.

## Running them

```sh
cargo build
./compiler/src/scripts/golden.sh                    # uses ./target/debug/jade
./compiler/src/scripts/golden.sh /path/to/jade      # or a specific binary
./compiler/src/scripts/ffi-gate.sh
```

## `ffi-gate.sh`: a real Rust package, built and run

`golden.sh` covers the language. This script covers the part of the toolchain whose correctness depends on code nobody here wrote: someone else's crate, someone else's C library, and cargo's and rustc's opinion of the bridge `#[jade::export]` generates.

It runs two checks, each catching a different kind of problem.

*A crates.io crate and a system library, built and run.* `ffi-gate/` is a Rust package wrapping `regex`, a large, ordinary crate fetched from crates.io, and the machine's own zlib through `libz-sys`, whose default features link the system library rather than building one in. The gate writes a project around it in a scratch directory, builds `gate.jde`, runs it, and compares what it prints with `gate.expected`. Since 2.0.7 its first line, `find_all`'s array of strings, is recorded quoted, `["1", "22", "333"]`, as a string inside an array prints. That covers cargo fetching and building a dependency graph nobody here wrote, a staticlib that imports a system library, and the link that finds it: Apple's SDK on macOS, a shared object beside the C library on Linux.

*Every run is checked for its exit status.* A process killed by a signal after it has printed everything leaves output that looks perfectly correct. The glib gate this one replaced reported a SIGSEGV as `ok` for five releases that way, visible in every CI log as a line the shell printed, directly above a gate saying it passed.

*Two functions, called until something breaks.* The first check calls each function once, which proves the answer is right and nothing more. What a Rust function returns is Rust's memory: the program copies it and hands it to the function's `__free` companion, so a long run should hold no more memory than a short one. `loop.jde` calls `echo`, which returns a string, and `words`, which returns an array of strings, and compares every answer, since memory released too early gives back garbage of the right length rather than a short answer. The gate runs it at 200,000 and 600,000 iterations and asserts that both survive, answer correctly, and that the larger run does not hold appreciably more memory.

The memory half needs a tool that reports peak resident memory: `/usr/bin/time -v` on GNU coreutils, or `/usr/bin/time -l` on macOS. The script probes for both and skips that half with a reported reason when it finds neither. The 16 MB allowance comes from the size of what leaks: each iteration returns four Rust allocations, about 100 bytes, so 400,000 extra iterations would hold some 40 MB more if they leaked, against a few MB of ordinary allocator churn.

A missing cargo produces a *skip*, reported rather than silent. The first run needs the network, for cargo to fetch `regex` and `libz-sys`.

```sh
./compiler/src/scripts/ffi-gate.sh                    # or pass a path to a jade binary
```

To drive the loop by hand at a different size, set `JADE_FFI_LOOP_ITERS` to the iteration count you want. The fixture defaults to 100,000 without it.

Until 2.0.5 this gate bound glib's whole C header through the generated shim, and called one `alloc_str` binding in a loop; Rust packages replaced that path (`design/v2-rust-packages.md`).
