---
id: index
title: Jade Documentation
sidebar_label: Installation
slug: /
---

Jade is a programming language written in Rust. It supports value types (`int`, `float`, `bool`, `str`, arrays, dicts, and user-defined `struct`s), `let` variable bindings, `fn` function definitions with `return`, anonymous closures (`|x| x * 2`), first-class functions, recursion, `if`/`elif`/`else` control flow, `while` loops, `for` loops over arrays, `try`/`catch`/`raise` exception handling, `struct` definitions with field access and mutation, `extend` blocks for methods, `interface` definitions, multi-file `use` imports, `print` and `len` built-in functions, f-string interpolation, the pipe operator (`|>`), `prompt` declarations and LLM inference via `?`, and a full operator set. The standard library covers math, string manipulation, file I/O, HTTP, JSON, shell commands, environment variables, path utilities, and random number generation — each available via dot-notation imports such as `use std.math`.

## Installation

### macOS and Linux (recommended)

The fastest way to install Jade is with the official install script. Open a terminal and run:

```bash
curl -fsSL https://jadelang.org/install.sh | sh
```

The script detects your OS and architecture, downloads the correct prebuilt binary from the [latest release](https://github.com/joericks1998/jade/releases/latest), and installs it to `/usr/local/bin/jade`. To install to a different location, set the `JADE_INSTALL_DIR` environment variable before running the script:

```bash
JADE_INSTALL_DIR=~/.local/bin curl -fsSL https://jadelang.org/install.sh | sh
```

### Windows

Download `jade-windows-x86_64.exe` from the [latest release](https://github.com/joericks1998/jade/releases/latest), rename it to `jade.exe`, and place it somewhere on your `PATH`.

### Build from Source

Requires Rust 1.70 or later — install via [rustup.rs](https://rustup.rs).

```bash
git clone https://github.com/joericks1998/jade
cd jade
cargo build --release
cp target/release/jade /usr/local/bin/jade
```

### Verify

```bash
jade --help
```

This prints the list of available subcommands. See the [CLI Reference](cli) for full details.
