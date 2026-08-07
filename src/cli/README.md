# `src/cli/` — the command implementations

## What this subtree is

One module per `jade` subcommand. `src/main.rs` is a thin clap definition that parses arguments and calls into here; everything a command actually does lives in this directory.

## Why it is split this way

Keeping `main.rs` to argument parsing means the commands are ordinary library functions that tests can call directly, and it keeps the clap derive definitions readable as a single overview of the CLI surface. It also means a command can be reused — `jade run` with no argument resolves a project entry point by calling the same `project/` helpers `jade test` uses.

## What each file does

- **`run.rs`** — `jade run`. With no argument it finds the project root and runs the entry file; a `.jde` argument runs that file; anything else is looked up as a named script in `jade.toml`'s `[scripts]`.
- **`check.rs`** — `jade check`. Type-checks without executing. It deliberately runs two things past inference, both so `check` stays an honest predictor of whether `run` and `build` will succeed: `emit`, because shared-mutation-across-tasks is rejected at emit time; and `project::walk_imports`, because import resolution is not a compile stage — the VM resolves a `use` when the Import opcode runs, so before v1.1.33 `use totally_made_up_module` reported `ok` and then failed at run time. Unlike `run`, the import walk does not call `pkg::ensure_ready`: checking a file should not reach the network to fetch dependencies.
- **`build.rs`** — `jade build`. Runs the whole pipeline in-process: lex, parse, infer, resolve imports, generate LLVM IR, link. Also handles `--emit-ir` and `--lib`. Calls `pkg::ensure_ready` before resolving imports, exactly as `run` does — without it the two engines disagreed about what a dependency is, `build` linking against whatever `libs/` was last left holding while `run` installed first. With no file argument, `--lib` builds the project's `[package]`: `resolve_target` turns the manifest into an entry, an output path, and an export list, and `compare_sources` holds the declared file list to what the entry actually imports.
- **`repl.rs`** — `jade repl`. Uses the VM so the REPL and `jade run` share one implementation. A bare trailing expression is assigned to an internal capture slot (a name starting with NUL, so it can never collide with a user global) and echoed.
- **`test.rs`** — `jade test`. Discovers `test_*.jde` and `*_test.jde` under the project root, optionally filtered by a pattern.
- **`fmt.rs`** — `jade fmt`. Works on source *text*, line-based, not on the token stream — the lexer strips comments, so a reprint would lose them. Limited by design: it fixes indentation and trailing whitespace but does not normalize operator spacing. `--check` exits 1 if anything would change.
- **`new.rs`** — `jade new` and `jade init`. Scaffolds a project directory from the `basic` or `llm` template.
- **`pkg.rs`** — `jade pkg add` / `bind` / `remove` / `install` / `update` / `list`. Binding a C library is folded into `add` and `install` rather than being a step to learn, and the header is discovered rather than demanded: `jade pkg add <name> --path <lib> --c-abi` derives the likely header from the library name, searches pkg-config and the usual include roots, checks the candidate against the library's own export table, then writes the symbol table and builds the shim in one command. `--header` overrides the search, and `install` fills in any dependency that names a header but has no symbols. `bind` remains for the cases with an actual decision in them — re-running after a header changes, or narrowing a large header with `--only` — and `--dry-run` shows the report without touching the manifest. All three print what could not be bound and why, because a generator that silently covers most of an API is how the rest is found at run time. The manifest is the source of truth and `jade.lock` is derived from it; with no registry to query, "update" means reconciling the lock with the manifest, not discovering a newer version. `install` re-hashes local `path` dependencies before installing, since those point at files the user rebuilds; `--locked` reports that drift as an error instead.
- **`register.rs`** — `jade register` and `jade use`. Picks which inference provider `?p` uses and stores its API key under `~/.jade`, machine-wide. `install.sh` runs `jade register` interactively after an install, so this is often a new user's very first `jade` command — it stays chatty and forgiving.
- **`env.rs`** — `jade env`. Version, binary path, platform, cache stats, project info. `--json` for scripting.
- **`cache.rs`** — `jade cache info` / `clean`.
- **`upgrade.rs`** — `jade upgrade`. Updates the toolchain itself from GitHub Releases. Distinct from `jade pkg update`, which is about project dependencies.
- **`mod.rs`** — module declarations plus `format_bytes`.
- **`tests.rs`** — CLI tests. See "Building and testing" below for what is and is not covered.

## Who uses it

*Depends on:* nearly every other module — `frontend/` and `compiler/` for the pipeline, `vm/` for execution, `build/` for compilation, `project/` for manifest and root resolution, `pkg/` for dependencies, `providers/` for the provider registry, `cache/` for the on-disk cache.

*Used by:* `src/main.rs` only.

## Gotchas

Commands exit the process directly on user error (`process::exit(1)`) with a message on stderr, rather than propagating a `Result` to `main`. Match that when adding one.

`jade upgrade` and `jade pkg update` are easy to confuse in help text and in commit messages. Upgrade is the toolchain; update is the project's dependencies.

**The package commands are nested — `jade pkg add`, never `jade add`.** This is not a style note. Before v1.1.35 every message that told a user how to recover named the unnested form, so the first thing a project with `[dependencies]` and no lock printed was an instruction to run a command that does not exist. Anything user-facing that names one of `add`, `remove`, `install`, `update`, or `list` needs the `pkg` in it, and that includes strings in `pkg/`, not just this directory.

**`jade fmt` reads source text, and the text has more in it than the token stream does.** The lexer throws away comments, so the formatter cannot work from tokens without losing them — which means it re-implements the parts of lexing that affect layout, and every one of them has bitten. Its scanner has to know about `//` comments, both quote characters, triple-quoted strings that span lines, and escapes, because a `{` it misreads shifts every line after it. Three separate versions of this bug shipped before v1.1.35, the worst of them reindenting the *inside* of a multi-line string and silently changing what the program printed. Two things guard it now: `run_fmt` re-lexes its own output and refuses to write a file whose tokens changed, and CI holds `examples/` formatted so the formatter meets 70-odd real files on every push.

## Building and testing

```sh
cargo test cli::
./target/debug/jade env      # quickest smoke test that the binary is wired up
```

A subcommand handler is not testable in process — every one ends in `process::exit`, and several read stdin, reach the network, or write under `~/.jade`. So `tests.rs` covers the decision each command makes *before* it touches the world: `fmt`'s formatting, `build`'s default output path and its `[package] sources` comparison, `upgrade`'s archive name for this platform, `run -v`'s value rendering, `new`'s scaffolding. Extract a pure helper when you add a command, or it will not be covered by anything.

Two rules make the tests safe under `cargo test`'s parallelism: no `std::env::set_var`, and no changing the working directory. Anything needing the filesystem uses the `TempDir` helper at the top of `tests.rs`, which gives each test a uniquely named directory and removes it on drop.
