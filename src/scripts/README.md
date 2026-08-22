# `src/scripts/` — development and CI gates

## What this subtree is

Four files, and they solve one problem between them: proving that Jade's two execution engines agree.

It lives under `src/` for tidiness rather than because it is source — nothing here is compiled into the crate. `jade.toml` makes the directory a Jade project root so the stub provider can reach the protocol submodule through a `[lib]` entry, which means the path in it is relative to *this* directory: `../protocol/jade` is `src/protocol/jade`.

## Why they exist

Jade has two independent execution paths — the bytecode VM (`jade run`) and the AOT LLVM backend (`jade build`) — and they have drifted three times: the build daemon resolving imports against stale code, imported `extend` methods reaching AOT but not the VM, and imported field defaults likewise. Every one of those was found by hand, because nothing ever ran the same program both ways and compared the output. The parity script does that.

The second file exists because the parity gate originally skipped everything under `examples/llm/`. A real model's output depends on the model rather than the backend, so it could not be diffed — which left the largest and most distinctive part of the language with no automated check that the two engines agree, and every backend divergence found so far had lived in exactly that kind of blind spot.

## What each file does

- **`backend-parity.sh`** — runs every example on both engines and diffs stdout. Takes an optional path to a `jade` binary. It builds the stand-in provider once, installs it in a throwaway slot, and points `JADE_PROVIDER_ACTIVE` there for the whole run. It maintains a skip list; read the header before assuming an example is covered.
- **`fake-provider.jde`** — a stand-in inference provider, answering every prompt with the reply in `JADE_FAKE_REPLY`. Built with `jade build --lib` and loaded exactly the way a released binary loads a real provider, so the gate exercises the real path. An example supplies its own reply as `responses.txt` beside the `.jde`; without one it gets a default.
- **`handle-fixture.c`** and **`handle-fixture.jde`** — a stand-in *native* package handing out opaque handles, and the Jade program that exercises it. Compiled with `cc` and run on both engines as an extra parity case after the examples.

  They are here rather than under `examples/` for a reason worth stating: a handle only ever comes from a native C package, and a Jade package built with `--lib` cannot mint one, so no `.jde` fixture can reach the tag at all. Leaving it there would be the same blind spot that let the `bytes` marshaller stay broken for three releases — and pointing this at it immediately turned up the AOT releasing its argument trees before reading the result, so a native function returning a pointer into its own argument gave an empty string compiled and the right one interpreted. The built library's extension also differs per platform, which a committed `jade.toml` under `examples/` would have to hard-code.

  `jade_pkg_abi_version` in the C file must match `jade_runtime::RUNTIME_ABI_VERSION`, or the loader refuses the package and the gate fails with a version message rather than a parity one.

This used to be `fake-jaded.py`, a stand-in *daemon* serving canned responses over a Unix socket, restarted between the VM and AOT runs so each engine read the same script from the top. The socket went away in v1.1.30, so the stub became a package — which needs no restart, since it holds no position in a script.

## Who uses it

*Used by:* `.github/workflows/ci.yml` runs `backend-parity.sh` as a required step on every pull request. Run it locally before opening one.

*Depends on:* a built `jade` binary (defaults to `./target/debug/jade`), the fixtures in `examples/`, the `src/protocol` submodule the stub imports, and `cc` for the handle fixture — which the C shim path already requires, and whose absence reports as a skip rather than a silent pass. Both paths are relative to the current directory, so run it from the repository root. Building the stand-in provider means the gate needs a working `jade build`, so an AOT regression fails here before it fails an example.

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

## `ffi-gate.sh` — a real C library, bound and run

`backend-parity.sh` covers the language. This covers the part of the toolchain whose correctness depends on code nobody here wrote: someone else's header, someone else's macros, and a C compiler's opinion of the shim generated from them.

Three checks, catching different classes.

**The C runtime, compiled optimised.** glibc's `realpath` writes up to `PATH_MAX` bytes into the buffer it is handed and aborts the process when that buffer is smaller — but the check only exists in an optimised build, so `cargo test` and the parity gate both miss it. Every FFI package in a compiled binary died at startup on Linux for two releases. glibc says what is wrong at compile time, so compiling with `-O2 -D_FORTIFY_SOURCE=3 -Werror=attribute-warning` is enough, and it takes seconds rather than the minutes a release build of the toolchain would. This half only bites on glibc: Apple's headers carry no such attribute, so on a Mac it passes on code that aborts on Linux. That asymmetry is how the bug shipped, and it is why CI is the run that counts.

**glib, bound whole and run on both engines.** glib is the fixture because it is big and ordinary — 1890 exported symbols written the way widely-used libraries are actually written, with typedefs over everything and function-like macros shadowing declared functions. The seven tidy libraries the coverage survey used never produced either. Binding glib turned up two bugs the same afternoon: a callback parameter checked against the typedef's name instead of its category, and a macro intercepting the call to the symbol that was bound. Each refused the whole dependency, so glib bound 1357 symbols and could not be used at all.

The whole header is bound, never a narrowed slice. A slice would cover only the shapes already handled, which is the opposite of the point. The fixture program itself (`glib-fixture.jde`) is deliberately dull: what is under test is that a large real header produces a shim that compiles, installs, and gives both engines the same answer.

Two symbols are then added to `jade.toml` by hand and the package reinstalled. That is not a shortcut around the generator — it is the other half of the workflow. A string the caller owns is the one thing a header cannot express, so the generator refuses all 125 of glib's and names the spelling, and this step is a user writing that spelling. Nothing else in the suite runs an `alloc_str` binding end to end, and `examples/` cannot: a real C library has to be installed for there to be anything to bind.

**Every run here is checked for its exit status, and that took four releases to be true.** A process killed by a signal *after* it has printed everything leaves an output file that looks perfectly correct, and both engines then agree about it because both printed the same correct thing. This step used to assert only that the VM's output was non-empty and free of the word "error", so a SIGSEGV under the VM was reported as `ok` from v1.3.19 to v1.3.24 — visible in every CI log as a `Segmentation fault` line the *shell* printed, directly above a gate saying `4 ok, 0 failed`. Step 3 had the status check and the note explaining it from the start; step 2 simply never got one.

What was crashing is worth recording, because nothing in the toolchain was wrong. The fixture called `g_intern_static_string`, which does not copy: glib's global intern table keeps *the caller's pointer*, and its documentation says the string must never be freed. Jade owns the buffer it passes into a native call and frees it afterwards, so the table was left holding a dangling pointer into reused memory, and glib faulted walking it at exit. It only showed up on Linux, and only under the VM — compiled, the literal lives in read-only static data and happens to satisfy the contract by accident. The fixture now calls `g_intern_string`, which copies. The general lesson is that a binding cannot express "the callee keeps this pointer forever", so a function with that contract is one Jade's argument ownership cannot satisfy.

**One `alloc_str` binding, called until something breaks.** The step above calls each binding once, which proves the answer is right and nothing else. What `alloc_str` actually promises is about many calls: the shim copies the string out and hands the original to the library's free function, so a long run should hold no more memory than a short one — 62 MB held before the release was added and 42 MB after, over 200,000 calls. One call cannot tell a correct release from a leak, and cannot tell either from a crash that only appears under sustained churn. A user hit exactly that, a compiled binary taking SIGSEGV at roughly 300,000 iterations over `g_uri_escape_string`.

So `alloc-str-loop.jde` calls `g_strdup` in a tight loop and compares every result against the string it asked for, because a string released before it is copied out comes back as garbage of the right length rather than as a short answer. The gate builds it and runs it twice, at 200,000 and 600,000 calls, and asserts two things: that the process survives both and answers correctly, and that the larger run does not hold appreciably more memory than the smaller one. Growth in proportion to calls made is the signature of a leak.

Three notes on how it is set up.

It runs *compiled only*, not on both engines. The VM is far slower per FFI call, so matching these counts there would cost minutes rather than the two seconds it costs compiled; and the failure being chased is a compiled one. Step 2 already covers the two engines agreeing about `alloc_str`.

It is a separate file from `glib-fixture.jde` because that fixture's output is diffed line by line between the engines, and a run this long has no business in a file being compared that way.

It reuses the glib project step 2 already built. Binding the whole header is nearly all of this script's runtime; binding it a second time would roughly double the gate.

The memory half needs a peak-RSS tool — `/usr/bin/time -v` on GNU coreutils, `-l` on macOS — and neither is guaranteed present, so the script probes for both and *skips that half with a reported reason* when there is none. Survival is asserted either way. The 16 MB allowance between the two runs is anchored on the measurement above rather than tuned to a machine: a leak costs about 100 bytes a call, so the 400,000 extra calls cost roughly 40 MB if they leak, against a few MB of ordinary allocator churn if they do not.

Missing glib or a missing C compiler is a *skip*, reported rather than silent, so the script is safe to run anywhere.

```sh
./src/scripts/ffi-gate.sh                    # or pass a path to a jade binary
```

To drive the loop by hand at a different size, `JADE_FFI_LOOP_ITERS` sets the call count and the fixture defaults to 100,000 without it.
