---
id: index
title: Jade Documentation
sidebar_label: Installation
slug: /
---

Jade is an AI-native programming language. A `prompt` is a type, dereferencing it with `?` calls a model, and the type you ask for is a contract the compiler enforces. See [LLM Integration](llm) for the whole story.

Around that idea, Jade is an ordinary general-purpose language.

*Values.* `int`, `float`, `bool`, `str`, `char`, `bytes`, arrays, dicts, and user-defined `struct`s, bound with `let`.

*Control flow.* `if`, `elif`, `else`, `while` and `for` loops, and `try`, `catch`, `raise` for exceptions.

*Functions.* `fn` definitions with `return`, closures written `|x| x * 2`, recursion, and decorators. `extend` blocks add methods to a type, and `interface` definitions describe what a type must provide.

*Concurrency.* `async fn` and `await`, plus `yield` for streams.

*Everything else.* Multi-file `use` imports, f-string interpolation, and the pipe operator `|>`.

The standard library covers math, strings, arrays, dicts, file I/O, time, HTTP, JSON, shell commands, environment variables, paths, and random numbers. You import each one with `::` notation, such as `use std::math`.

Two engines run the same language. `jade run` interprets bytecode, and `jade build` compiles a native binary through LLVM.

## Installation

Jade runs on macOS and Linux. Windows is not supported; see [below](#windows).

### macOS and Linux (recommended)

The fastest way to install Jade is the official install script. Open a terminal and run:

```bash
curl -fsSL https://jadelang.org/install.sh | sh
```

The script works out which prebuilt archive you need, downloads it from the [latest release](https://github.com/joericks1998/jade/releases/latest), verifies its checksum, and installs it to `/usr/local/bin/jade`. Two builds ship: macOS on Apple Silicon, and Linux on x86_64. On an Intel Mac, either run the Apple Silicon build under Rosetta 2 or [build from source](#build-from-source).

Alongside the binary, the script installs two more things into `lib/jade`, next to the install directory: the runtime archives that `jade build` links into every executable it emits, and the bundled inference providers. `jade` finds them by looking relative to itself, so the binary and that directory have to stay together.

To install somewhere else, set `JADE_INSTALL_DIR` before running the script:

```bash
JADE_INSTALL_DIR=~/.local/bin curl -fsSL https://jadelang.org/install.sh | sh
```

The script finishes by running `jade register`, which lists the bundled providers and asks for an API key. You can skip that step and run `jade register` later. See [LLM Integration](llm#configuration).

### Windows

Jade does not support Windows. Native packages, including the inference provider, are loaded with `dlopen`, and the C runtime is written against POSIX. There is no native Windows build.

Use [WSL2](https://learn.microsoft.com/en-us/windows/wsl/install) and follow the Linux instructions above.

### Build from Source

You need two things installed.

*Rust 1.85 or later.* Install it from [rustup.rs](https://rustup.rs). The crate uses edition 2024, so an older toolchain will not build it.

*LLVM 18.* `jade build` compiles in-process rather than shelling out to a separate compiler, so LLVM is a build dependency of the toolchain itself. A released `jade` binary needs nothing installed; only building from source does.

Install LLVM, then point `LLVM_SYS_180_PREFIX` at it. That variable is how the build finds your installation.

```bash
brew install llvm@18                                              # macOS
sudo apt-get install llvm-18-dev libpolly-18-dev libzstd-dev      # Debian and Ubuntu

git clone https://github.com/joericks1998/jade
cd jade
export LLVM_SYS_180_PREFIX=/opt/homebrew/opt/llvm@18   # or /usr/lib/llvm-18
cargo build --release
```

The binary lands at `target/release/jade` and works from there.

Keep the checkout after building. `jade build` links two runtime archives, `libJadeRuntime.a` and `libjade_runtime.a`, which the same `cargo build` leaves in `target/release`. A locally built `jade` remembers that path. To move the toolchain to another machine, copy the archives with it into `<prefix>/lib/jade`. That is the layout the release tarball uses, and it is where `jade` looks next to itself.

The repository's own [README](https://github.com/joericks1998/jade#build-from-source) covers the debug build and the extra tools the test suite needs.

### Updating and removing

```bash
jade upgrade      # fetch and install the latest release
jade reinstall    # reinstall the current version, for a damaged installation
jade uninstall    # remove the binary and its runtime archives
```

`jade uninstall` prints every path before it removes anything. It leaves `~/.jade` alone, which holds your API key, installed providers, and build cache, unless you pass `--purge`. See the [CLI Reference](cli) for the full flags.

### Verify

```bash
jade --help    # the list of subcommands
jade env       # version, cache, project, and which inference provider is active
```
