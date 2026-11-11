# `src/scripts/`: development and CI gates

## What this subtree is

These are the gates CI runs over the built `jade` binary, as opposed to the Rust unit tests `cargo test` runs. They answer one question between them: does a program that worked before still work, all the way to a running binary?

The directory lives under `src/` for tidiness rather than because it is source. Nothing here is compiled into the crate.

## Why they exist

Until v1.4.8, Jade had two engines: a bytecode VM behind `jade run` and the compiled LLVM backend behind `jade build`. The gate here was `backend-parity.sh`, which ran every example on both and diffed the output. The engines drifted apart three times, and parity caught each one. It also found the bugs that sat in its own blind spots once it was pointed at them, such as the LLM examples it originally skipped.

v2 removed the VM, which leaves nothing to diff against. So before the VM went, each example's output was recorded beside it as `<name>.expected`, from a run where both engines agreed on all 120. That recording is the oracle now, and `golden.sh` holds the compiled backend to it.

## What each file does

- *`golden.sh`* builds every example, runs the binary, and compares its output and exit status to the `<name>.expected` and optional `<name>.expected-exit` beside it. An example with no `.expected` file fails rather than passes. It takes an optional path to a `jade` binary, and keeps a skip list: the network examples, the `*_error.jde` fixtures that `jade check` covers, and the examples that dereference a prompt, whose recorded output came from v1's mock provider; a prompt now runs a real model, which `src/infer/tests.rs` checks against llama.cpp. Read the header before assuming an example is covered.

  It also runs the native handle round-trip, against `handle-fixture.expected`, and the C struct round-trip, against `struct-fixture.expected`.

  Until phase 6 of `design/v2-static-values.md` it had a `--backend=static` mode, with a list of the examples that backend covered. That backend is the only one now, so the flag and the list are gone, and every example builds with it.
- *`handle-fixture.c`* and *`handle-fixture.jde`* are a stand-in *native* package that hands out opaque handles, plus the Jade program that exercises it. `cc` compiles the C file and `golden.sh` builds the program against it.

  They live here rather than under `examples/` for a reason worth stating. A handle only ever comes from a native C package, and a Jade package built with `--lib` cannot produce one. So no `.jde` fixture can reach the tag at all, and leaving it out of the gate would recreate the blind spot that let the `bytes` marshaller stay broken for three releases. The built library's file extension also differs by platform, which a committed `jade.toml` under `examples/` would have to hard-code.

  `jade_pkg_abi_version` in the C file must match `jade_runtime::RUNTIME_ABI_VERSION`. The gate reads that number from the runtime source rather than carrying a copy.

  The `jade.toml` the gate writes declares the fixture's functions in `[lib.handlefix.symbols]`, because a Jade-ABI library carries no types and the compiler needs them (design decision 11).
- *`struct-fixture.c`*, *`struct-fixture.h`* and *`struct-fixture.jde`* are a plain C library whose functions pass structs in, out and both ways, return one by value, and hand back several values at once, plus the program that calls it. `golden.sh` builds the library with `cc`, binds it as a C dependency with `jade pkg install`, which builds its shim, and compares the program's output to `struct-fixture.expected`. Each shape is one the shim packs into a record, which the static backend reads as a class.
- *`ffi-gate.sh`*, *`glib-fixture.jde`* and *`alloc-str-loop.jde`* are the FFI gate, described below.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `golden.sh` and `ffi-gate.sh` as required steps on every pull request. Run them locally before opening one.

*Depends on:* a built `jade` binary, defaulting to `./target/debug/jade`, the fixtures and `.expected` files in `examples/`, and `cc` for the handle fixture. A missing `cc` reports as a skip rather than a silent pass. Paths are relative to the current directory, so run the scripts from the repository root.

## Running them

```sh
cargo build
./compiler/src/scripts/golden.sh                    # uses ./target/debug/jade
./compiler/src/scripts/golden.sh /path/to/jade      # or a specific binary
./compiler/src/scripts/ffi-gate.sh
```

## `ffi-gate.sh`: a real C library, bound and run

`golden.sh` covers the language. This script covers the part of the toolchain whose correctness depends on code nobody here wrote: someone else's header, someone else's macros, and a C compiler's opinion of the shim generated from them.

It runs two checks, each catching a different kind of problem. Until phase 6 a third compiled v1's C runtime with glibc's fortify checks, which caught a `realpath` buffer a quarter of the size glibc requires; that runtime is gone.

*glib, bound whole and run.* glib is the fixture because it is big and ordinary: 1890 exported symbols, written the way widely-used libraries are actually written, with typedefs over everything and function-like macros shadowing declared functions. The seven tidy libraries the coverage survey used never produced either pattern.

Binding glib turned up two bugs in one afternoon. A callback parameter was checked against the typedef's name rather than its category. And a macro intercepted the call to the symbol that had been bound. Each one refused the whole dependency, so glib bound 1357 symbols and could not be used at all.

The gate binds the whole header, never a narrowed slice. A slice would cover only the shapes already handled, which is the opposite of the point. The fixture program itself, `glib-fixture.jde`, is deliberately dull. What is under test is that a large real header produces a shim which compiles, installs, and runs to completion.

Two symbols are then added to `jade.toml` by hand, and the package is reinstalled. That is not a shortcut around the generator. It is the other half of the workflow.

A string the caller owns is the one thing a header cannot express, so the generator refuses all 125 of glib's and names the spelling to use instead. This step is a user writing that spelling. Nothing else in the suite runs an `alloc_str` binding from end to end, and `examples/` cannot, because a real C library has to be installed for there to be anything to bind.

*Every run here is checked for its exit status, and that took four releases to become true.* A process killed by a signal *after* it has printed everything leaves an output file that looks perfectly correct.

This step used to assert only that the output was non-empty and free of the word "error". So a SIGSEGV under the old VM was reported as `ok` from v1.3.19 to v1.3.24. It was visible in every CI log, as a `Segmentation fault` line the *shell* printed, directly above a gate saying `4 ok, 0 failed`. The long run had the status test and the note explaining it from the start. This check simply never got one.

What was crashing is worth recording, because nothing in the toolchain was wrong. The fixture called `g_intern_static_string`, which does not copy. glib's global intern table keeps *the caller's pointer*, and its documentation says the string must never be freed.

Jade owns the buffer it passes into a native call and frees it afterwards. So the table was left holding a pointer into reused memory, and glib faulted while walking it at exit. It only showed up on Linux, and only under the VM that v1 had. When compiled, the literal lives in read-only static data and happens to satisfy the contract by accident.

The fixture now calls `g_intern_string`, which copies. The general lesson is that a binding cannot express "the callee keeps this pointer forever", so a function with that contract is one Jade's argument ownership cannot satisfy.

*One `alloc_str` binding, called until something breaks.* The check above calls each binding once, which proves the answer is right and nothing more.

What `alloc_str` actually promises is about many calls. The shim copies the string out and hands the original to the library's free function, so a long run should hold no more memory than a short one. Over 200,000 calls, the measurement was 62 MB held before the release was added and 42 MB after.

One call cannot tell a correct release from a leak, and cannot tell either of those from a crash that only appears under sustained churn. A user hit exactly that: a compiled binary taking a SIGSEGV at roughly 300,000 iterations over `g_uri_escape_string`.

So `alloc-str-loop.jde` calls `g_strdup` in a tight loop and compares every result against the string it asked for. A string released before it is copied out comes back as garbage of the right length rather than as a short answer, which is why the comparison matters.

The gate builds that program and runs it twice, at 200,000 and 600,000 calls. It asserts two things: that the process survives both runs and answers correctly, and that the larger run does not hold appreciably more memory than the smaller one. Memory growing in proportion to the number of calls is the signature of a leak.

Three notes on how that check is set up.

It is the long run: 800,000 calls between its two passes, which take about two seconds compiled.

It is a separate file from `glib-fixture.jde` because that fixture is meant to stay short and readable, and a run this long has no business in it.

It reuses the glib project the first check already built. Binding the whole header is nearly all of this script's running time, so binding it a second time would roughly double the gate.

The memory half needs a tool that reports peak resident memory: `/usr/bin/time -v` on GNU coreutils, or `/usr/bin/time -l` on macOS. Neither is guaranteed to be present, so the script probes for both and *skips that half with a reported reason* when it finds neither. It asserts survival either way.

The 16 MB allowance between the two runs comes from the measurement above rather than from tuning to one machine. A leak costs about 100 bytes per call, so the 400,000 extra calls would cost roughly 40 MB if they leak, against a few MB of ordinary allocator churn if they do not.

A missing glib or a missing C compiler produces a *skip*, reported rather than silent, so the script is safe to run anywhere.

```sh
./compiler/src/scripts/ffi-gate.sh                    # or pass a path to a jade binary
```

To drive the loop by hand at a different size, set `JADE_FFI_LOOP_ITERS` to the call count you want. The fixture defaults to 100,000 without it.
