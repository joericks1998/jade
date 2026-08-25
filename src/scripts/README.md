# `src/scripts/`: development and CI gates

## What this subtree is

These files solve one problem between them: proving that Jade's two execution engines agree.

The directory lives under `src/` for tidiness rather than because it is source. Nothing here is compiled into the crate. `jade.toml` makes it a Jade project root, so the stub provider can reach the protocol submodule through a `[lib]` entry. The path in that entry is relative to *this* directory, so `../protocol/jade` means `src/protocol/jade`.

## Why they exist

Jade has two independent execution paths: the bytecode VM behind `jade run`, and the AOT LLVM backend behind `jade build`. They have drifted apart three times. The build daemon resolved imports against stale code. Imported `extend` methods reached the AOT path but not the VM. Imported field defaults did the same.

Somebody found every one of those by hand, because nothing ever ran the same program both ways and compared the output. The parity script does that.

The second file exists because the parity gate originally skipped everything under `examples/llm/`. A real model's output depends on the model rather than the backend, so it could not be diffed. That left the largest and most distinctive part of the language with no automated check that the two engines agree, and every divergence found so far had lived in exactly that kind of blind spot.

## What each file does

- *`backend-parity.sh`* runs every example on both engines and diffs stdout. It takes an optional path to a `jade` binary. It builds the stand-in provider once, installs it in a throwaway slot, and points `JADE_PROVIDER_ACTIVE` there for the whole run. It keeps a skip list, so read the header before assuming an example is covered.
- *`fake-provider.jde`* is a stand-in inference provider. It answers every prompt with the reply in `JADE_FAKE_REPLY`. It is built with `jade build --lib` and loaded exactly the way a released binary loads a real provider, so the gate exercises the real path. An example supplies its own reply as a `responses.txt` beside the `.jde`, and gets a default without one.
- *`handle-fixture.c`* and *`handle-fixture.jde`* are a stand-in *native* package that hands out opaque handles, plus the Jade program that exercises it. `cc` compiles the C file, and the gate runs the pair on both engines as an extra parity case after the examples.

  They live here rather than under `examples/` for a reason worth stating. A handle only ever comes from a native C package, and a Jade package built with `--lib` cannot produce one. So no `.jde` fixture can reach the tag at all.

  Leaving it in `examples/` would have created the same blind spot that let the `bytes` marshaller stay broken for three releases. Pointing this fixture at the tag immediately turned up the AOT path releasing its argument trees before reading the result. A native function returning a pointer into its own argument gave an empty string when compiled and the right one when interpreted.

  The built library's file extension also differs by platform, which a committed `jade.toml` under `examples/` would have to hard-code.

  `jade_pkg_abi_version` in the C file must match `jade_runtime::RUNTIME_ABI_VERSION`, or the loader refuses the package and the gate fails with a version message rather than a parity one.

This used to be `fake-jaded.py`, a stand-in *daemon* serving canned responses over a Unix socket. It was restarted between the VM and AOT runs so each engine read the same script from the top. The socket went away in v1.1.30, so the stub became a package. A package needs no restart, because it holds no position in a script.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `backend-parity.sh` as a required step on every pull request. Run it locally before opening one.

*Depends on:* a built `jade` binary, defaulting to `./target/debug/jade`. It also needs the fixtures in `examples/`, the `src/protocol` submodule the stub imports, and `cc` for the handle fixture. The C shim path already requires `cc`, and a missing one reports as a skip rather than a silent pass.

Both paths are relative to the current directory, so run the script from the repository root. Building the stand-in provider means the gate needs a working `jade build`, so an AOT regression fails here before it fails an example.

## Running them

```sh
cargo build
./src/scripts/backend-parity.sh                    # uses ./target/debug/jade
./src/scripts/backend-parity.sh /path/to/jade      # or a specific binary
```

To drive a Jade program against a canned reply by hand:

```sh
mkdir -p /tmp/slot && jade build src/scripts/fake-provider.jde --lib -o /tmp/slot/fake.so
JADE_PROVIDER_ACTIVE=/tmp/slot JADE_FAKE_REPLY="hello" jade run your.jde
```

## `ffi-gate.sh`: a real C library, bound and run

`backend-parity.sh` covers the language. This script covers the part of the toolchain whose correctness depends on code nobody here wrote: someone else's header, someone else's macros, and a C compiler's opinion of the shim generated from them.

It runs three checks, each catching a different kind of problem.

*The C runtime, compiled with optimizations.* glibc's `realpath` writes up to `PATH_MAX` bytes into the buffer it is handed, and aborts the process when that buffer is smaller. The check only exists in an optimised build, so `cargo test` and the parity gate both miss it. Every FFI package in a compiled binary died at startup on Linux for two releases.

glibc says what is wrong at compile time, so compiling with `-O2 -D_FORTIFY_SOURCE=3 -Werror=attribute-warning` is enough. That takes seconds, rather than the minutes a release build of the toolchain would take.

This check only bites on glibc. Apple's headers carry no such attribute, so on a Mac it passes on code that aborts on Linux. That asymmetry is how the bug shipped, and it is why the CI run is the one that counts.

*glib, bound whole and run on both engines.* glib is the fixture because it is big and ordinary: 1890 exported symbols, written the way widely-used libraries are actually written, with typedefs over everything and function-like macros shadowing declared functions. The seven tidy libraries the coverage survey used never produced either pattern.

Binding glib turned up two bugs in one afternoon. A callback parameter was checked against the typedef's name rather than its category. And a macro intercepted the call to the symbol that had been bound. Each one refused the whole dependency, so glib bound 1357 symbols and could not be used at all.

The gate binds the whole header, never a narrowed slice. A slice would cover only the shapes already handled, which is the opposite of the point. The fixture program itself, `glib-fixture.jde`, is deliberately dull. What is under test is that a large real header produces a shim which compiles, installs, and gives both engines the same answer.

Two symbols are then added to `jade.toml` by hand, and the package is reinstalled. That is not a shortcut around the generator. It is the other half of the workflow.

A string the caller owns is the one thing a header cannot express, so the generator refuses all 125 of glib's and names the spelling to use instead. This step is a user writing that spelling. Nothing else in the suite runs an `alloc_str` binding from end to end, and `examples/` cannot, because a real C library has to be installed for there to be anything to bind.

*Every run here is checked for its exit status, and that took four releases to become true.* A process killed by a signal *after* it has printed everything leaves an output file that looks perfectly correct. Both engines then agree about it, because both printed the same correct thing.

This step used to assert only that the VM's output was non-empty and free of the word "error". So a SIGSEGV under the VM was reported as `ok` from v1.3.19 to v1.3.24. It was visible in every CI log, as a `Segmentation fault` line the *shell* printed, directly above a gate saying `4 ok, 0 failed`. The third check had the status test and the note explaining it from the start. The second check simply never got one.

What was crashing is worth recording, because nothing in the toolchain was wrong. The fixture called `g_intern_static_string`, which does not copy. glib's global intern table keeps *the caller's pointer*, and its documentation says the string must never be freed.

Jade owns the buffer it passes into a native call and frees it afterwards. So the table was left holding a pointer into reused memory, and glib faulted while walking it at exit. It only showed up on Linux, and only under the VM. When compiled, the literal lives in read-only static data and happens to satisfy the contract by accident.

The fixture now calls `g_intern_string`, which copies. The general lesson is that a binding cannot express "the callee keeps this pointer forever", so a function with that contract is one Jade's argument ownership cannot satisfy.

*One `alloc_str` binding, called until something breaks.* The check above calls each binding once, which proves the answer is right and nothing more.

What `alloc_str` actually promises is about many calls. The shim copies the string out and hands the original to the library's free function, so a long run should hold no more memory than a short one. Over 200,000 calls, the measurement was 62 MB held before the release was added and 42 MB after.

One call cannot tell a correct release from a leak, and cannot tell either of those from a crash that only appears under sustained churn. A user hit exactly that: a compiled binary taking a SIGSEGV at roughly 300,000 iterations over `g_uri_escape_string`.

So `alloc-str-loop.jde` calls `g_strdup` in a tight loop and compares every result against the string it asked for. A string released before it is copied out comes back as garbage of the right length rather than as a short answer, which is why the comparison matters.

The gate builds that program and runs it twice, at 200,000 and 600,000 calls. It asserts two things: that the process survives both runs and answers correctly, and that the larger run does not hold appreciably more memory than the smaller one. Memory growing in proportion to the number of calls is the signature of a leak.

Three notes on how that check is set up.

It runs *compiled only*, not on both engines. The VM is far slower per FFI call, so matching these counts there would cost minutes rather than the two seconds it costs compiled. The failure being chased is also a compiled one. The second check already covers the two engines agreeing about `alloc_str`.

It is a separate file from `glib-fixture.jde` because that fixture's output is diffed line by line between the engines, and a run this long has no business in a file compared that way.

It reuses the glib project the second check already built. Binding the whole header is nearly all of this script's running time, so binding it a second time would roughly double the gate.

The memory half needs a tool that reports peak resident memory: `/usr/bin/time -v` on GNU coreutils, or `/usr/bin/time -l` on macOS. Neither is guaranteed to be present, so the script probes for both and *skips that half with a reported reason* when it finds neither. It asserts survival either way.

The 16 MB allowance between the two runs comes from the measurement above rather than from tuning to one machine. A leak costs about 100 bytes per call, so the 400,000 extra calls would cost roughly 40 MB if they leak, against a few MB of ordinary allocator churn if they do not.

A missing glib or a missing C compiler produces a *skip*, reported rather than silent, so the script is safe to run anywhere.

```sh
./src/scripts/ffi-gate.sh                    # or pass a path to a jade binary
```

To drive the loop by hand at a different size, set `JADE_FFI_LOOP_ITERS` to the call count you want. The fixture defaults to 100,000 without it.
