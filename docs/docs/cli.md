---
id: cli
title: CLI Reference
sidebar_label: CLI Reference
---

The `jade` command is built from subcommands. Every operation has a name, so there is no primary `jade <file>` form. The old bare form still works as a shorthand for `jade run`, kept for compatibility.

## `jade run`

Run a Jade source file, a named script from `jade.toml`, or the project entry point. With no argument, the entry point defaults to `main.jde`.

```bash
jade run program.jde          # run a specific file
jade run                      # run the project entry point (main.jde)
jade run build                # run the script named "build" in jade.toml
```

### Flags

| Flag | Description |
|------|-------------|
| `-v`, `--verbose` | Print all global variables and their final values after execution. Variables are printed in alphabetical order. |

```bash
jade run program.jde --verbose
jade run program.jde -v
```

## `jade check`

Type-check a source file without running it. It exits with code 0 when there are no errors.

```bash
jade check program.jde
```

## `jade build`

Compile a Jade file into a native binary. Every step runs inside the `jade` process: the frontend lexes, parses, infers types, and produces typed IR, then import resolution, LLVM code generation, and linking follow. Unix only.

```bash
jade build program.jde
jade build program.jde --output mybin
jade build program.jde --emit ir
jade build --lib                          # build the [package] in jade.toml
```

Every module the file `use`s is compiled into the same artifact, so a program or a package can span as many files as you like.

With no file given, `--lib` builds the project's `[package]` section. Its entry, its exports, and its artifact name all come from `jade.toml`. See [Packages](packages#jade-source-packages).

### Flags

| Flag | Description |
|------|-------------|
| `-o`, `--output <PATH>` | Output binary path. Defaults to the input filename without extension, or the package name for a `[package]` build. |
| `--emit ir` | Print the generated LLVM IR to stdout instead of producing a binary. |
| `--lib` | Build a shared library that exports `jade_pkg_init`, which is a package other Jade projects can depend on. |
| `--export <NAME>` | With `--lib`, bind only the named functions. Repeatable, and the default is all of them. It overrides `[package].exports` when you give both. |

:::note
Native code generation, the C runtime, and linking are all part of the `jade` binary. So building the toolchain from source needs LLVM 18 installed, found through `LLVM_SYS_180_PREFIX`. Running a released binary needs nothing.
:::

## `jade new` / `jade init`

`jade new <name>` creates a new project in a new directory called `<name>`. `jade init` sets up a project in the directory you are already in. Both take a `--template` flag.

```bash
jade new myapp
jade new myapp --template llm
jade init
jade init --template basic
```

### Flags

| Flag | Description |
|------|-------------|
| `--template <basic\|llm>` | Project template. Defaults to `basic`. |

## `jade repl`

Start an interactive REPL session. Each line is evaluated against the running environment, and definitions from earlier lines stay in scope.

```bash
jade repl
jade repl --verbose
```

### Flags

| Flag | Description |
|------|-------------|
| `-v`, `--verbose` | Print extra debug info for each evaluated expression. |

## `jade test`

Find and run every test file whose name matches `test_*.jde` or `*_test.jde`.

```bash
jade test
jade test my_feature
jade test --verbose
```

### Arguments and flags

| Argument or flag | Description |
|------|-------------|
| `[PATTERN]` | Only run tests whose name contains this string. |
| `-v`, `--verbose` | Show output from each test file. |

## `jade fmt`

Format Jade source files. It works on a single file, or on every `.jde` file in a directory and its subdirectories.

```bash
jade fmt program.jde
jade fmt src/
jade fmt src/ --check
```

### Flags

| Flag | Description |
|------|-------------|
| `--check` | Exit with code 1 if any file would change. Useful in CI. |

### What it changes

It changes indentation and trailing whitespace, and nothing else. Blocks get four spaces. Three or more blank lines in a row collapse to two, and the file ends with exactly one newline. Operator spacing, line length, and where you break a long expression stay as you wrote them.

Two kinds of line keep the exact whitespace they came with:

- *Inside a triple-quoted string.* That indentation is part of the text, so changing it would change what your program prints.
- *A wrapped expression*, meaning the continuation lines of an argument list, an array, or a struct literal. The position of a `{` decides which case you are in. A `{` that ends the line opens a block, so `let cfg = {` indents what follows. A `{` with more on the same line, such as `Result { name: name,`, leaves your alignment alone.

Formatting only moves whitespace, so the result must lex to the same tokens as the original. `jade fmt` checks that before writing, and leaves the file alone if the tokens differ. A file that does not lex at all is skipped without a report, so running the formatter mid-edit is safe.

## `jade register` / `jade use`

Choose the inference provider that `?p` calls, and store its API key. Both settings are global and per-user, kept under `~/.jade` rather than in a project.

```bash
jade register                    # list installed providers, pick one, enter a key
jade register anthropic sk-...   # or name the provider and key outright
jade register --list             # show what is installed and which is active
jade register --remove anthropic # forget a stored key

jade use openai                  # switch providers without re-entering a key
```

Providers ship with Jade, so there is usually nothing to install first. Until you register one, a `?` dereference fails with "no inference backend available".

### Flags

| Flag | Description |
|------|-------------|
| `--list` | List installed providers and the active selection, then exit. |
| `--remove` | Remove the provider's stored credential instead of setting one. |

## `jade env`

Show the Jade environment: version, binary path, platform, cache statistics, and project information.

```bash
jade env
jade env --json
```

### Flags

| Flag | Description |
|------|-------------|
| `--json` | Output environment information as JSON. |

## `jade upgrade`

Download and install the latest Jade release, replacing the current binary where it stands.

```bash
jade upgrade
```

It checks the GitHub releases page, compares the latest version against the one running, downloads the right prebuilt binary for your platform, and replaces the current executable in one atomic step. If the binary sits in a system directory, re-run it as `sudo jade upgrade`. When you are already on the latest version, it stops and does nothing.

:::note
`jade upgrade` updates the *toolchain*. `jade pkg update` updates a *project's dependencies*. The two are unrelated, and neither does the other's job.
:::

## `jade reinstall`

Fetch and install the latest release *even when it is the version already running*. Use it when an installation is damaged rather than out of date. `jade upgrade` returns immediately when you are current, which is no help in that case.

```bash
jade reinstall
jade reinstall --clean        # also wipe ~/.jade first
jade reinstall --clean --yes  # no confirmation prompt
```

### Flags

| Flag | Description |
|------|-------------|
| `--clean` | Remove `~/.jade` before reinstalling, which clears the cache, config, credentials, and installed providers. |
| `--yes` | Do not ask before removing anything. |

`--clean` takes your API key with it, so run `jade register` again afterwards.

## `jade uninstall`

Remove Jade from this machine. It deletes the binary and the `lib/jade` tree the installer put beside it, and prints every path before touching anything.

```bash
jade uninstall
jade uninstall --purge   # also remove ~/.jade
jade uninstall --yes
```

### Flags

| Flag | Description |
|------|-------------|
| `--purge` | Also remove `~/.jade`, which holds the cache, config, credentials, and installed providers. |
| `--yes` | Do not ask before removing anything. |

`~/.jade` holds your API key and your installed providers. None of that belongs to the toolchain, so it survives unless you ask for it to go with `--purge`.

:::note
Both `uninstall` and `reinstall --clean` refuse to run without a terminal to confirm at, unless you pass `--yes`. So a script that never asked for a deletion does not get one. Both follow symlinks to the real install path, because removing a link would leave the file behind. If a path is owned by root, run the command again with `sudo`.
:::

## `jade cache`

Manage the build cache. The cache stores compiled AST and bytecode so Jade can skip work it has already done.

```bash
jade cache info
jade cache clean
jade cache clean --older-than 30
jade cache clean --dry-run
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `info` | Show cache statistics: entry count and total size. |
| `clean` | Remove stale or old cache entries. |

### clean Flags

| Flag | Description |
|------|-------------|
| `--older-than <DAYS>` | Also remove entries older than this many days. |
| `--dry-run` | Show what would be removed without deleting. |

## `jade pkg`

Manage the dependencies declared in the `[dependencies]` section of `jade.toml`. A dependency is a Jade source package, pinned by `jade.lock` and installed into a project-local `libs/` directory, or a Rust package, which cargo builds into the program. `jade run`, `jade test`, and `jade build` install anything missing on their own.

:::warning The package commands are nested
It is `jade pkg add`, never `jade add`. The same is true of `remove`, `install`, `update`, and `list`. Every one of them lives under `jade pkg`.
:::

```bash
jade pkg add geometry --path ../geometry          # Jade source: the directory has a jade.toml
jade pkg add imgtools --path rust/imgtools        # Rust: the directory has a Cargo.toml
jade pkg add shapes --git https://github.com/me/shapes --tag v1.2.0
jade pkg add hex --rust --version 0.4.3           # a Rust package from crates.io
jade pkg install                                  # install everything in the lock
jade pkg list
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `add <name>` | Add a dependency, then install it. A `--path` directory holding a `Cargo.toml` is a Rust package, and one holding a `jade.toml` is Jade source. A Rust package is built on the spot, and `add` reports what it exports. |
| `remove <name>` | Remove a dependency from `jade.toml`, `jade.lock`, and `libs/`. |
| `install` | Fetch and verify every source package `jade.lock` pins, re-pinning any local `path` package whose source changed. `--locked` refuses to change the lock, and fails instead if the lock is out of date. Rust packages are left to cargo, at build time. |
| `update [name]` | Resolve dependencies against `jade.toml` again and rewrite `jade.lock`. It reconciles what you declared; it does not go looking for a newer version. |
| `list` | List locked source packages, whether they are installed here, and whether a local source has changed since it was pinned, then the Rust packages. |

### `add` flags

| Flag | Description |
|------|-------------|
| `--path <DIR>` | A directory, relative to the project root: a Jade source package or a Rust crate. |
| `--git <URL>` | A git repository, with one of `--tag`, `--rev` or `--branch`. Jade source, unless `--rust` is given. |
| `--url <URL>` | A `.tar.gz` archive of a Jade source package. Needs `--version`. |
| `--version <VERSION>` | An exact version. Required with `--url`; with `--rust` and no other source, the crates.io version to build. There are no version ranges. |
| `--rust` | The dependency is a Rust package. Unneeded for a `--path` holding a `Cargo.toml`. |
| `--package <NAME>` | A Rust package's own name, when it differs from the name a program writes, such as one with a `-`. |

`jade pkg bind` and the `--header`, `--c-abi`, `--include` and `--define` flags of `add` were removed in 2.0.5, with the C bindings they generated. A C library is reached through a Rust crate that wraps it.

See [Packages](packages) for the full workflow.

:::note
LLM configuration lives in a provider package, not in the language. `jade register` installs one and stores your API key, `jade use` switches between installed providers, and `jade env` shows which is active. See [LLM Integration](llm).
:::

## Backward-Compatible File Execution

The old `jade <file.jde>` form, written without the `run` subcommand, still works as a hidden shorthand. It calls `jade run <file>` for you. Prefer `jade run` in new scripts and documentation.

```bash
jade program.jde          # equivalent to: jade run program.jde
jade program.jde -v       # equivalent to: jade run program.jde --verbose
```

The shorthand is also the only interpreter form that accepts extra arguments. `jade run program.jde one two` is rejected, while `jade program.jde one two` passes the arguments through to `env.args()`. When you need real command-line arguments, build the program instead. See [`std/env`](stdlib#stdenv).

## Error Output

Errors are written to stderr with a source location prefix:

```
[line:col] error description
```

One example is `[3:5] undefined variable 'x'`. The message text tells you which phase produced the error, whether that was the lexer, the parser, or the evaluator.
