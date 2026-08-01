# `src/cli/` — the command implementations

## What this subtree is

One module per `jade` subcommand. `src/main.rs` is a thin clap definition that parses arguments and calls into here; everything a command actually does lives in this directory.

## Why it is split this way

Keeping `main.rs` to argument parsing means the commands are ordinary library functions that tests can call directly, and it keeps the clap derive definitions readable as a single overview of the CLI surface. It also means a command can be reused — `jade run` with no argument resolves a project entry point by calling the same `project/` helpers `jade test` uses.

## What each file does

- **`run.rs`** — `jade run`. With no argument it finds the project root and runs the entry file; a `.jde` argument runs that file; anything else is looked up as a named script in `jade.toml`'s `[scripts]`.
- **`check.rs`** — `jade check`. Type-checks without executing. It deliberately runs two things past inference, both so `check` stays an honest predictor of whether `run` and `build` will succeed: `emit`, because shared-mutation-across-tasks is rejected at emit time; and `project::walk_imports`, because import resolution is not a compile stage — the VM resolves a `use` when the Import opcode runs, so before v1.1.33 `use totally_made_up_module` reported `ok` and then failed at run time. Unlike `run`, the import walk does not call `pkg::ensure_ready`: checking a file should not reach the network to fetch dependencies.
- **`build.rs`** — `jade build`. Runs the whole pipeline in-process: lex, parse, infer, resolve imports, generate LLVM IR, link. Also handles `--emit-ir` and `--lib`.
- **`repl.rs`** — `jade repl`. Uses the VM so the REPL and `jade run` share one implementation. A bare trailing expression is assigned to an internal capture slot (a name starting with NUL, so it can never collide with a user global) and echoed.
- **`test.rs`** — `jade test`. Discovers `test_*.jde` and `*_test.jde` under the project root, optionally filtered by a pattern.
- **`fmt.rs`** — `jade fmt`. Works on source *text*, line-based, not on the token stream — the lexer strips comments, so a reprint would lose them. Limited by design: it fixes indentation and trailing whitespace but does not normalize operator spacing. `--check` exits 1 if anything would change.
- **`new.rs`** — `jade new` and `jade init`. Scaffolds a project directory from the `basic` or `llm` template.
- **`pkg.rs`** — `jade add` / `remove` / `install` / `update` / `list`. The manifest is the source of truth and `jade.lock` is derived from it; with no registry to query, "update" means reconciling the lock with the manifest, not discovering a newer version.
- **`register.rs`** — `jade register` and `jade use`. Picks which inference provider `?p` uses and stores its API key under `~/.jade`, machine-wide. `install.sh` runs `jade register` interactively after an install, so this is often a new user's very first `jade` command — it stays chatty and forgiving.
- **`env.rs`** — `jade env`. Version, binary path, platform, cache stats, project info. `--json` for scripting.
- **`cache.rs`** — `jade cache info` / `clean`.
- **`upgrade.rs`** — `jade upgrade`. Updates the toolchain itself from GitHub Releases. Distinct from `jade update`, which is about project dependencies.
- **`mod.rs`** — module declarations plus `format_bytes`.
- **`help.rs`** — currently empty.
- **`tests.rs`** — CLI tests.

## Who uses it

*Depends on:* nearly every other module — `frontend/` and `compiler/` for the pipeline, `vm/` for execution, `build/` for compilation, `project/` for manifest and root resolution, `pkg/` for dependencies, `providers/` for the provider registry, `cache/` for the on-disk cache.

*Used by:* `src/main.rs` only.

## Gotchas

Commands exit the process directly on user error (`process::exit(1)`) with a message on stderr, rather than propagating a `Result` to `main`. Match that when adding one.

`jade upgrade` and `jade update` are easy to confuse in help text and in commit messages. Upgrade is the toolchain; update is the project's dependencies.

## Building and testing

```sh
cargo test cli::
./target/debug/jade env      # quickest smoke test that the binary is wired up
```
