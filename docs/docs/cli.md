---
id: cli
title: CLI Reference
sidebar_label: CLI Reference
---

The `jade` command uses a subcommand structure. Every operation is a named subcommand — there is no bare `jade <file>` primary form (though the old form is accepted as a backward-compatible shorthand via `jade run`).

## `jade run`

Run a Jade source file, a named script from `jade.toml`, or the project entry point (`main.jde` by default when no argument is given).

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

Type-check a source file without executing it. Exits with code 0 if there are no errors.

```bash
jade check program.jde
```

## `jade build`

Compile a Jade file to a native binary. Everything runs in-process: the language frontend (lex → parse → type-infer → typed IR), then import resolution, LLVM code generation, and linking. Unix only.

```bash
jade build program.jde
jade build program.jde --output mybin
jade build program.jde --emit ir
jade build --lib                          # build the [package] in jade.toml
```

Every module the file `use`s is compiled into the same artifact, so a program or a package can span as many files as it likes.

With no file, `--lib` builds the project's `[package]` section — its entry, its exports, and its artifact name all come from `jade.toml`. See [Packages](packages#declaring-the-package-in-jadetoml).

### Flags

| Flag | Description |
|------|-------------|
| `-o`, `--output <PATH>` | Output binary path. Defaults to the input filename without extension, or the package name for a `[package]` build. |
| `--emit ir` | Print the generated LLVM IR to stdout instead of producing a binary. |
| `--lib` | Build a shared library exporting `jade_pkg_init` — a package other Jade projects can depend on. |
| `--export <NAME>` | With `--lib`, bind only these functions (repeatable; default: all of them). Overrides `[package].exports` when both are given. |

:::note
Native code generation, the C runtime, and linking are part of the `jade` binary. Building the toolchain from source therefore needs LLVM 18 present (`LLVM_SYS_180_PREFIX`); running a released binary does not.
:::

## `jade new` / `jade init`

`jade new <name>` creates a new Jade project in a new directory named `<name>`. `jade init` initializes a project in the current directory. Both accept a `--template` flag.

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

Start an interactive REPL session. Each line is evaluated against the running environment; definitions from previous lines persist.

```bash
jade repl
jade repl --verbose
```

### Flags

| Flag | Description |
|------|-------------|
| `-v`, `--verbose` | Print extra debug info for each evaluated expression. |

## `jade test`

Discover and run test files matching `test_*.jde` or `*_test.jde` patterns.

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

Format Jade source files. Works on a single file or all `.jde` files in a directory recursively.

```bash
jade fmt program.jde
jade fmt src/
jade fmt src/ --check
```

### Flags

| Flag | Description |
|------|-------------|
| `--check` | Exit with code 1 if any file would be changed (useful for CI). |

### What it changes

Indentation and trailing whitespace, and nothing else. Four spaces per block. Three or more blank lines in a row become two, and the file ends in exactly one newline. Operator spacing, line length, and where you break a long expression are left as you wrote them.

Two kinds of line keep the exact whitespace they came with:

- **Inside a triple-quoted string.** That indentation is part of the text, so changing it would change what your program prints.
- **A wrapped expression** — the continuation lines of an argument list, an array, or a struct literal. Where a `{` sits decides which case you are in: one that ends the line opens a block, so `let cfg = {` indents what follows, while `Result { name: name,` leaves your alignment alone.

Formatting only moves whitespace, so the result has to lex to the same tokens as what you wrote. `jade fmt` checks that before writing and leaves the file alone if they differ. A file that does not lex at all is skipped rather than reported, so formatting while you are mid-edit is safe.

## `jade register` / `jade use`

Choose the inference provider that `?p` calls, and store its API key. Both are global and per-user, kept under `~/.jade` — not per project.

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

Show the Jade environment: version, binary path, platform, cache stats, and project info.

```bash
jade env
jade env --json
```

### Flags

| Flag | Description |
|------|-------------|
| `--json` | Output environment information as JSON. |

## `jade upgrade`

Download and install the latest Jade release, replacing the current binary in place.

```bash
jade upgrade
```

Checks the GitHub releases page, compares the latest version against the running version, downloads the correct prebuilt binary for your platform, and atomically replaces the current executable. If the binary is in a system directory, re-run with `sudo jade upgrade`. It stops without doing anything when you are already on the latest version.

:::note
`jade upgrade` updates the **toolchain**. `jade pkg update` updates a **project's dependencies**. The two are unrelated, and neither one does the other's job.
:::

## `jade reinstall`

Fetch and install the latest release *even when it is the version already running*. Reach for it when an installation is damaged rather than out of date — `jade upgrade` returns immediately when you are current, which is exactly no help then.

```bash
jade reinstall
jade reinstall --clean        # also wipe ~/.jade first
jade reinstall --clean --yes  # no confirmation prompt
```

### Flags

| Flag | Description |
|------|-------------|
| `--clean` | Remove `~/.jade` before reinstalling: cache, config, credentials, and installed providers. |
| `--yes` | Do not ask before removing anything. |

`--clean` takes your API key with it, so re-run `jade register` afterwards.

## `jade uninstall`

Remove Jade from this machine. It deletes the binary and the `lib/jade` tree the installer lays down beside it, and it prints every path before touching one.

```bash
jade uninstall
jade uninstall --purge   # also remove ~/.jade
jade uninstall --yes
```

### Flags

| Flag | Description |
|------|-------------|
| `--purge` | Also remove `~/.jade`: cache, config, credentials, and installed providers. |
| `--yes` | Do not ask before removing anything. |

`~/.jade` holds your API key and your installed providers, and none of that is part of the toolchain — so it survives unless you ask for it to go with `--purge`.

:::note
Both `uninstall` and `reinstall --clean` refuse to run without a terminal to confirm at unless you pass `--yes`, so a script that did not ask for a deletion does not get one. Both resolve the install path through symlinks, because removing a link would leave the file it pointed at behind. If a path is owned by root, re-run with `sudo`.
:::

## `jade cache`

Manage the build cache. The cache stores compiled AST and bytecode to skip redundant compilation passes.

```bash
jade cache info
jade cache clean
jade cache clean --older-than 30
jade cache clean --dry-run
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `info` | Show cache statistics (entry count, total size). |
| `clean` | Remove stale or old cache entries. |

### clean Flags

| Flag | Description |
|------|-------------|
| `--older-than <DAYS>` | Also remove entries older than this many days. |
| `--dry-run` | Show what would be removed without deleting. |

## `jade pkg`

Manage project dependencies declared in `[dependencies]` of `jade.toml` and pinned by `jade.lock`. Dependencies are prebuilt native shared libraries installed into a project-local `libs/`; `jade run`, `jade test`, and `jade build` install anything missing automatically.

:::warning The package commands are nested
It is `jade pkg add`, never `jade add`. The same goes for `bind`, `remove`, `install`, `update`, and `list` — every one of them lives under `jade pkg`.
:::

```bash
jade pkg add fastmath --url 'https://example.com/fastmath-{platform}.so' --version 1.2.0
jade pkg add mathlib --path ../mathlib/mathlib.dylib
jade pkg add sqlite --path /opt/homebrew/lib/libsqlite3.dylib   # a plain C library
jade pkg install                 # install everything in the lock
jade pkg list
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `add <name>` | Add a dependency from `--url` or `--path`, then install it. For a local artifact it works out on its own whether the library is a Jade package or plain C, finds the C header, and generates the binding. |
| `bind <name> --header <h>` | Re-generate a C dependency's symbol table from its header. `add` and `install` already do this; `bind` is for re-running after a header changes, or narrowing a large header with `--only`. |
| `remove <name>` | Remove a dependency from `jade.toml`, `jade.lock`, and `libs/`. |
| `install` | Fetch and verify everything `jade.lock` pins. Re-pins any local `path` dependency whose source has been rebuilt, and fills in the symbols of any C dependency that names a header but has none yet. `--locked` refuses to change the lock, failing instead if it is out of date. |
| `update [name]` | Re-resolve dependencies against `jade.toml` and rewrite `jade.lock`. There is no registry, so this reconciles — it does not discover a newer version. |
| `list` | List locked dependencies, whether they are installed here, and whether a local source has changed since it was pinned. |

### `add` flags

| Flag | Description |
|------|-------------|
| `--path <FILE>` | A local `.so`/`.dylib`, relative to the project root. |
| `--url <URL>` | Download URL. May contain `{platform}`. |
| `--version <VERSION>` | Exact version. Required with `--url`; optional with `--path`. There are no version ranges. |
| `--c-abi` | Force plain-C binding. Usually unnecessary — a local artifact is recognised by whether it exports `jade_pkg_init`. Needed for a `--url` C library, since there is no local file to read yet. |
| `--header <FILE>` | The C library's header. Implies `--c-abi`, and binds it on the spot. Pass it when the automatic search would miss. |
| `-I`, `--include <DIR>` | Extra include directory for the header. Repeatable. |

### `bind` flags

| Flag | Description |
|------|-------------|
| `--header <FILE>` | The library's header, e.g. `/opt/homebrew/include/sqlite3.h`. Required. |
| `-I`, `--include <DIR>` | Extra include directory. Repeatable. |
| `--only <TEXT>` | Only bind symbols whose name contains this. |
| `--dry-run` | Show what would be written without changing `jade.toml`. |

`bind` merges into the existing symbol table rather than replacing it, so taking a large header a piece at a time with `--only` is a normal way to work.

See [Packages](packages) for the full workflow.

:::note
LLM configuration lives in a provider package, not in the language. `jade register` installs one and stores your API key, `jade use` switches between installed providers, and `jade env` shows which is active. See [LLM Integration](llm).
:::

## Backward-Compatible File Execution

The old `jade <file.jde>` form (without the `run` subcommand) is still accepted as a hidden shorthand. It dispatches to `jade run <file>` internally. Prefer `jade run` in new scripts and documentation.

```bash
jade program.jde          # equivalent to: jade run program.jde
jade program.jde -v       # equivalent to: jade run program.jde --verbose
```

The shorthand is also the only interpreter form that accepts extra arguments — `jade run program.jde one two` is rejected, while `jade program.jde one two` passes them through to `env.args()`. Build the program when you need real command-line arguments; see [`std/env`](stdlib#stdenv).

## Error Output

Errors are written to stderr with a source location prefix:

```
[line:col] error description
```

For example: `[3:5] undefined variable 'x'`. The phase that produced the error (lexer, parser, or evaluator) is implicit in the error message text.
