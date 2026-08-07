---
id: index
title: Jade Documentation
sidebar_label: Installation
slug: /
---

Jade is an AI-native programming language. A `prompt` is a type, dereferencing it with `?` calls a model, and the type you ask for is a contract the compiler enforces — see [LLM Integration](llm).

Around that it is an ordinary general-purpose language. It has value types (`int`, `float`, `bool`, `str`, `char`, `bytes`, arrays, dicts, and user-defined `struct`s), `let` bindings, `fn` definitions with `return`, closures (`|x| x * 2`), recursion, `if`/`elif`/`else`, `while` and `for` loops, `try`/`catch`/`raise`, `extend` blocks for methods, `interface` definitions, `async fn` and `await`, `yield` for streams, decorators, multi-file `use` imports, f-string interpolation, and the pipe operator (`|>`). The standard library covers math, strings, arrays, dicts, file I/O, time, HTTP, JSON, shell commands, environment variables, paths, and random numbers — each imported with `::` notation, such as `use std::math`.

Two engines run the same language: `jade run` interprets bytecode, and `jade build` compiles a native binary through LLVM.

## Installation

Jade runs on macOS and Linux. Windows is not supported — see [below](#windows).

### macOS and Linux (recommended)

The fastest way to install Jade is with the official install script. Open a terminal and run:

```bash
curl -fsSL https://jadelang.org/install.sh | sh
```

The script works out which prebuilt archive you need, downloads it from the [latest release](https://github.com/joericks1998/jade/releases/latest), verifies its checksum, and installs to `/usr/local/bin/jade`. Two builds ship: **macOS on Apple Silicon** and **Linux on x86_64**. On an Intel Mac, run the Apple Silicon build under Rosetta 2 or [build from source](#build-from-source).

Alongside the binary it installs the runtime archives `jade build` links into every executable it emits, plus the bundled inference providers, into `lib/jade` next to the install directory. `jade` finds them relative to itself, so the two must stay together.

To install somewhere else, set `JADE_INSTALL_DIR` before running the script:

```bash
JADE_INSTALL_DIR=~/.local/bin curl -fsSL https://jadelang.org/install.sh | sh
```

The script finishes by running `jade register`, which lists the bundled providers and asks for an API key. You can skip it and run `jade register` later; see [LLM Integration](llm#configuration).

### Windows

Jade does not support Windows. Native packages — including the inference provider — are loaded with `dlopen`, and the C runtime is written against POSIX, so there is no native Windows build.

Use [WSL2](https://learn.microsoft.com/en-us/windows/wsl/install) and follow the Linux instructions above.

### Build from Source

Two prerequisites:

- **Rust 1.85 or later** — install via [rustup.rs](https://rustup.rs). The crate uses edition 2024, so an older toolchain will not build it.
- **LLVM 18.** `jade build` compiles in-process, so LLVM is a build dependency of the toolchain rather than an optional extra. Point `LLVM_SYS_180_PREFIX` at your installation. A *released* `jade` binary needs nothing installed.

```bash
brew install llvm@18                        # macOS
sudo apt-get install llvm-18-dev libpolly-18-dev libzstd-dev    # Debian/Ubuntu

git clone https://github.com/joericks1998/jade
cd jade
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18   # or /usr/lib/llvm-18
cargo build --release
```

The binary is then at `target/release/jade`, and it works from there. `jade build` also links two runtime archives, `libJadeRuntime.a` and `libjade_runtime.a`, which the same `cargo build` leaves in `target/release`; a locally built `jade` remembers that path, so keep the checkout around. To move the toolchain onto another machine, copy the archives with it into `<prefix>/lib/jade` — the layout the release tarball uses, and the one `jade` looks for next to itself.

### Updating and removing

```bash
jade upgrade      # fetch and install the latest release
jade reinstall    # reinstall the current version, for a damaged installation
jade uninstall    # remove the binary and its runtime archives
```

`jade uninstall` prints every path before it removes anything, and leaves `~/.jade` — your API key, installed providers, and build cache — alone unless you pass `--purge`. See the [CLI Reference](cli) for the full flags.

### Verify

```bash
jade --help    # the list of subcommands
jade env       # version, cache, project, and which inference provider is active
```
