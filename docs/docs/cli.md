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

Compile a Jade file to a native binary. The `jade` CLI runs the language frontend (lex → parse → type-infer → typed IR) and hands the typed program to the **build daemon** over a Unix socket at `$HOME/.jade/build.sock`. The daemon performs import resolution, native code generation, and linking. The build daemon must be running, and this subcommand is available on Unix only.

```bash
jade build program.jde
jade build program.jde --output mybin
jade build program.jde --emit ir
```

### Flags

| Flag | Description |
|------|-------------|
| `-o`, `--output <PATH>` | Output binary path. Defaults to the input filename without extension. |
| `--emit ir` | Print the generated IR text (returned by the daemon) to stdout instead of producing a binary. |

:::note
Native code generation, the C runtime, and linking live in the out-of-process build daemon, not in the `jade` binary itself. If the daemon is not reachable at `$HOME/.jade/build.sock`, `jade build` reports a build error. Run `jade env` to check daemon reachability (the `build` line reports `reachable` or `not running`).
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

## `jade env`

Show the Jade environment: version, config file location, cache directory, and project info.

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

## `jade model`

Manage LLM model configuration.

```bash
jade model list
jade model use anthropic/claude-3-5-sonnet
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `list` | List known LLM models by provider. |
| `use <provider/model>` | Set the default model, writing to `~/.jade/config.toml`. |

## `jade configure`

Run the interactive configuration wizard to set up an LLM backend. Stores provider, model, and API key in `~/.jade/config.toml`. The `JADE_API_KEY` environment variable can also be used to supply a key at runtime.

```bash
jade configure
```

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
