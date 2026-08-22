# `src/cli/`: the command implementations

## What this subtree is

This is one module per `jade` subcommand. `src/main.rs` is a thin clap definition that parses arguments and calls into here. Everything a command actually does lives in this directory.

## Why it is split this way

Keeping `main.rs` to argument parsing leaves the commands as ordinary library functions that tests can call directly. It also keeps the clap derive definitions readable as one overview of the whole CLI surface. And it lets commands reuse each other. `jade run` with no argument resolves a project entry point by calling the same `project/` helpers `jade test` uses.

## What each file does

- *`run.rs`* implements `jade run`. With no argument it finds the project root and runs the entry file. A `.jde` argument runs that file. Anything else is looked up as a named script in the `[scripts]` section of `jade.toml`.
- *`check.rs`* implements `jade check`, which type-checks without running anything. It deliberately runs two more things past inference, so that `check` stays an honest predictor of whether `run` and `build` will succeed.

  The first is `emit`, because shared mutation across tasks is rejected at emit time. The second is `project::walk_imports`, because import resolution is not a compile stage. The VM resolves a `use` when the Import opcode runs, so before v1.1.33, `use totally_made_up_module` reported `ok` and then failed at run time.

  Unlike `run`, the import walk does not call `pkg::ensure_ready`. Checking a file should not reach the network to fetch dependencies.
- *`build.rs`* implements `jade build`. It runs the whole pipeline inside one process: lex, parse, infer, resolve imports, generate LLVM IR, and link. It also handles `--emit ir` and `--lib`.

  It calls `pkg::ensure_ready` before resolving imports, exactly as `run` does. Without that call, the two engines disagreed about what a dependency is: `build` linked against whatever `libs/` happened to hold, while `run` installed first.

  With no file argument, `--lib` builds the project's `[package]`. `resolve_target` turns the manifest into an entry, an output path, and an export list, and `compare_sources` holds the declared file list to what the entry actually imports.
- *`repl.rs`* implements `jade repl`. It uses the VM, so the REPL and `jade run` share one implementation. A bare trailing expression is assigned to an internal capture slot and echoed. That slot's name starts with a NUL byte, so it can never collide with a user global.
- *`test.rs`* implements `jade test`. It finds every `test_*.jde` and `*_test.jde` under the project root, optionally filtered by a pattern.
- *`fmt.rs`* implements `jade fmt`. It works line by line on source *text* rather than on the token stream, because the lexer strips comments and a reprint would lose them. It is limited on purpose: it fixes indentation and trailing whitespace, and does not normalize operator spacing. `--check` exits 1 if anything would change.
- *`new.rs`* implements `jade new` and `jade init`. It sets up a project directory from either the `basic` or the `llm` template.
- *`pkg.rs`* implements `jade pkg add`, `bind`, `remove`, `install`, `update`, and `list`.

  Binding a C library is folded into `add` and `install`, rather than being a separate step to learn. Both the ABI and the header are discovered rather than demanded.

  `jade pkg add <name> --path <lib>` reads the artifact's symbol table to see whether it exports `jade_pkg_init`. That is the same symbol the loader requires, so detection and acceptance cannot disagree. For a plain C library it then derives the likely header from the library name, searches pkg-config and the usual include roots, checks the candidate against the library's own export table, and finally writes the symbol table and builds the shim, all in one command.

  `--header` overrides the search, and `install` fills in any dependency that names a header but has no symbols yet. `bind` remains for the cases with a real decision in them, such as re-running after a header changes or narrowing a large header with `--only`. `--dry-run` shows the report without touching the manifest.

  All three print what could not be bound and why, because a generator that quietly covers most of an API is how the rest gets found at run time.

  A header clang could *read* is recorded on the dependency even when every symbol in it was skipped. The skip report tells the user to write that stanza by hand, and without the header their `int` would mean Jade's width rather than the library's. So `add` keeps its entry in that one case rather than rolling it back, and says which of the two happened.

  The manifest is the source of truth, and `jade.lock` is derived from it. With no registry to query, "update" means reconciling the lock with the manifest rather than discovering a newer version. `install` re-hashes local `path` dependencies before installing, since those point at files the user rebuilds. `--locked` reports that drift as an error instead.

- *`register.rs`* implements `jade register` and `jade use`. It picks which inference provider `?p` uses and stores that provider's API key under `~/.jade`, for the whole machine. `install.sh` runs `jade register` interactively after an install, so this is often a new user's very first `jade` command. It stays chatty and forgiving on purpose.
- *`env.rs`* implements `jade env`, which prints the version, binary path, platform, cache statistics, and project information. `--json` makes the output easy to script against.
- *`cache.rs`* implements `jade cache info` and `jade cache clean`.
- *`upgrade.rs`* implements `jade upgrade`, which updates the toolchain itself from GitHub Releases. It is a different thing from `jade pkg update`, which is about a project's dependencies.
- *`mod.rs`* holds the module declarations plus `format_bytes`.
- *`tests.rs`* holds the CLI tests. See "Building and testing" below for what is and is not covered.

## Who uses it

*Depends on:* nearly every other module. `frontend/` and `compiler/` for the pipeline, `vm/` for execution, `build/` for compilation, `project/` for manifest and root resolution, `pkg/` for dependencies, `providers/` for the provider registry, and `cache/` for the on-disk cache.

*Used by:* `src/main.rs` only.

## Gotchas

A command exits the process directly on user error, by calling `process::exit(1)` after printing a message to stderr, rather than returning a `Result` to `main`. Match that when you add one.

`jade upgrade` and `jade pkg update` are easy to confuse in help text and in commit messages. Upgrade is the toolchain. Update is the project's dependencies.

*`jade pkg add` writes before it validates, so a failure has to undo the write.* Binding a C library reads the dependency back out of `jade.toml`, and resolving needs the entry to be there, so it cannot come last.

  What made the missing rollback worse than it sounds is that every other `pkg` command re-validates the whole manifest. One `add` that failed on a missing file made `install`, `list`, and even a later successful `add` fail on an orphan entry the user never managed to add, with nothing naming the cause.

  `fail_new_dependency` removes an entry the command created, and leaves one that was already there. `add` replaces an existing entry outright, and rolling that back would delete a working dependency to tidy up after a failed attempt to change it. Anything else that edits the manifest before validating needs the same treatment.

*The package commands are nested, so it is `jade pkg add` and never `jade add`.* This is not a style note. Before v1.1.35, every message telling a user how to recover named the unnested form. So the first thing a project with `[dependencies]` and no lock printed was an instruction to run a command that does not exist. Anything user-facing that names `add`, `remove`, `install`, `update`, or `list` needs the `pkg` in it, and that includes strings in `pkg/`, not only in this directory.

*`jade fmt` reads source text, and the text holds more than the token stream does.* The lexer throws comments away, so the formatter cannot work from tokens without losing them. That means it re-implements the parts of lexing that affect layout, and every one of them has caused a bug.

  Its scanner has to know about `//` comments, both quote characters, triple-quoted strings spanning several lines, and escapes. A `{` it misreads shifts every line after it. Three separate versions of that bug shipped before v1.1.35. The worst of them reindented the *inside* of a multi-line string and silently changed what the program printed.

  Two things guard it now. `run_fmt` re-lexes its own output and refuses to write a file whose tokens changed. And CI keeps `examples/` formatted, so the formatter meets about seventy real files on every push.

## Building and testing

```sh
cargo test cli::
./target/debug/jade env      # quickest smoke test that the binary is wired up
```

A subcommand handler cannot be tested in process. Every one ends in `process::exit`, and several read stdin, reach the network, or write under `~/.jade`.

So `tests.rs` covers the decision each command makes *before* it touches the world: how `fmt` formats, `build`'s default output path and its `[package] sources` comparison, the archive name `upgrade` picks for this platform, how `run -v` renders a value, and what `new` scaffolds. Extract a pure helper when you add a command, or nothing will cover it.

Two rules keep the tests safe under `cargo test`'s parallelism: never call `std::env::set_var`, and never change the working directory. Anything needing the filesystem uses the `TempDir` helper at the top of `tests.rs`, which gives each test a uniquely named directory and removes it on drop.
