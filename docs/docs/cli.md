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
```

### Flags

| Flag | Description |
|------|-------------|
| `-o`, `--output <PATH>` | Output binary path. Defaults to the input filename without extension. |
| `--emit ir` | Print the generated LLVM IR to stdout instead of producing a binary. |
| `--lib` | Build a shared library exporting `jade_pkg_init` — a package other Jade projects can depend on. |
| `--export <NAME>` | With `--lib`, bind only these functions (repeatable; default: all of them). |

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

### Flags

| Flag | Description |
|------|-------------|
| `[pattern]` | Only run tests whose name contains this string. |
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

Checks the GitHub releases page, compares the latest version against the running version, downloads the correct prebuilt binary for your platform, and atomically replaces the current executable. If the binary is in a system directory, re-run with `sudo jade upgrade`.

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

Manage project dependencies declared in `[dependencies]` of `jade.toml` and pinned by `jade.lock`. Dependencies are prebuilt native shared libraries installed into a project-local `libs/`; `jade run` and `jade test` install anything missing automatically.

```bash
jade pkg add fastmath --url https://example.com/fastmath-{platform}.so
jade pkg add mathlib --path ../mathlib
jade pkg install                 # install everything in the lock
jade pkg list
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `add <name>` | Add a dependency from `--url` or `--path` (with `--version`; `--c-abi` for a plain C library). |
| `remove <name>` | Remove a dependency from `jade.toml` and `jade.lock`. |
| `install` | Install all locked dependencies (`--locked` fails if the lock is out of date). |
| `update [name]` | Re-resolve and update a dependency (or all of them). |
| `list` | List declared dependencies and their resolved versions. |

:::note
LLM configuration lives in a provider package, not in the language. `jade register` installs one and stores your API key, `jade use` switches between installed providers, and `jade env` shows which is active. See [LLM Integration](llm).
:::

## Backward-Compatible File Execution

The old `jade <file.jde>` form (without the `run` subcommand) is still accepted as a hidden shorthand. It dispatches to `jade run <file>` internally. Prefer `jade run` in new scripts and documentation.

```bash
jade program.jde          # equivalent to: jade run program.jde
jade program.jde -v       # equivalent to: jade run program.jde --verbose
```

## Error Output

Errors are written to stderr with a source location prefix:

```
[line:col] error description
```

For example: `[3:5] undefined variable 'x'`. The phase that produced the error (lexer, parser, or evaluator) is implicit in the error message text.
