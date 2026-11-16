# `src/cli/`: the command implementations

## What this subtree is

This is one module per `jade` subcommand. `src/main.rs` is a thin clap definition that parses arguments and calls into here. Everything a command actually does lives in this directory.

## Why it is split this way

Keeping `main.rs` to argument parsing leaves the commands as ordinary library functions that tests can call directly. It also keeps the clap derive definitions readable as one overview of the whole CLI surface. And it lets commands reuse each other. `jade run` with no argument resolves a project entry point by calling the same `project/` helpers `jade test` uses.

## What each file does

- *`run.rs`* implements `jade run`. With no argument it finds the project root and runs the entry file. A `.jde` argument runs that file. Anything else is looked up as a named script in the `[scripts]` section of `jade.toml`.

  Jade has one engine, the compiler, so running a file means building it. `run_file` builds into a scratch directory under the system temp dir, runs the binary with the same standard streams and any arguments given after `--`, removes the directory, and exits with the binary's status. A binary killed by a signal exits as a shell would report it, 128 plus the signal number, so a crash never reads as success. Until v1.4.8, `jade run` interpreted the file on a bytecode VM instead.
- *`check.rs`* implements `jade check`, which type-checks without running anything. It lexes, parses and runs the per-file pass, `typechecker::resolve`, and caches the resulting tree by the file's hash, so a second check of an unchanged file starts from the cached tree. It deliberately runs two more things past that pass, on a cached tree too, so that `check` stays an honest predictor of whether `run` and `build` will succeed.

  The first is `project::walk_imports`, because import resolution is not a type-checking stage. Before v1.1.33, `use totally_made_up_module` reported `ok` and then failed later. The second is `build::would_build`, which runs the whole build short of LLVM: the static checker, MIR lowering, and `mir::taskcheck`, which rejects shared mutation across tasks. Until phase 6 that rule ran in v1's bytecode emitter, so `check` ran the emitter instead.

  Unlike `run`, the import walk does not call `pkg::ensure_ready`. Checking a file should not reach the network to fetch dependencies.
- *`build.rs`* implements `jade build`. It runs the whole pipeline inside one process: lex, parse, the per-file pass, resolve imports, check static types, lower to MIR, generate LLVM IR, and link with the LLD built into `jade`. `--emit ir` prints the LLVM IR, `--emit mir` prints MIR, and `--emit asm` prints the assembly for the machine `jade` runs on, instead of writing a binary. `--lib` is gone, since a Jade package is shared as source; `main.rs` keeps the flag hidden only to say so.

  The compile step is `compile_file`, which `run` and `test` share. It calls `pkg::ensure_ready` before resolving imports, so a fresh clone installs its dependencies first rather than linking against whatever `libs/` happened to hold. `try_compile_file` is the same step returning its message instead of exiting, for `jade test`, where one failing file must not stop the rest. It is the one place that names the std library's cache, `codegen::default_lib_cache()`, which is `~/.jade/lib`; every build, run and test links the library's compiled instances from there.

  When the file being built is the entry of the project's `[package]`, `check_package_sources` holds the declared `sources` list to what the entry actually imports, through `compare_sources`.
- *`test.rs`* implements `jade test`. It finds every `test_*.jde` and `*_test.jde` under the project root, optionally filtered by a pattern, and builds and runs each one. A test passes when its binary exits 0.
- *`fmt.rs`* implements `jade fmt`. It works line by line on source *text* rather than on the token stream, because the lexer strips comments and a reprint would lose them. It is limited on purpose: it fixes indentation and trailing whitespace, and does not normalize operator spacing. `--check` exits 1 if anything would change. Since 2.0.10 it also puts a bare `if`, `elif` or `while` condition in parentheses, in `add_condition_parens`, which is what moves an older file to the new form in one command. That step finds each condition from the lexer's tokens, inserts the two characters into the text so the rest of the line is kept byte for byte, and checks that the result lexes to the old tokens plus the parentheses; a condition already in parentheses is left alone, so a program that compiles comes back unchanged.
- *`new.rs`* implements `jade new` and `jade init`. It sets up a project directory from either the `basic` or the `llm` template.
- *`pkg.rs`* implements `jade pkg add`, `remove`, `install`, `update`, and `list`.

  `jade pkg add <name> --path <dir>` decides the kind of dependency from what is in the directory: a `Cargo.toml` makes it a Rust package, and a `jade.toml` a Jade source package. `--rust` says so for a `--git` repository or a crates.io `--version`, which cannot be looked inside first, and `--package` gives a Rust package's own name when it differs. A Rust package is built on the spot (`rustpkg::build`), so `add` reports how many functions and structs it exports and refuses one that does not compile. A `--git` repository or a `.tar.gz` `--url` without `--rust` is a Jade source package, which `src/pkg/source.rs` installs and pins.

  `jade pkg bind` and `add`'s `--header`, `--c-abi`, `--include`, `--define` and `--only` went in 2.0.5 with the C bindings they generated (`design/v2-rust-packages.md`). A path to a `.dylib` or `.so` is refused with a message naming the replacement, a Rust crate that wraps the library.

  The manifest is the source of truth, and `jade.lock` is derived from it. With no registry to query, "update" means reconciling the lock with the manifest rather than discovering a newer version. `install` re-hashes local `path` dependencies before installing, since those point at directories the user edits. `--locked` reports that drift as an error instead.

- *`env.rs`* implements `jade env`, which prints the version, binary path, platform, cache statistics, and project information. `--json` makes the output easy to script against.
- *`cache.rs`* implements `jade cache info` and `jade cache clean`.
- *`upgrade.rs`* implements `jade upgrade`, which updates the toolchain itself from GitHub Releases. It is a different thing from `jade pkg update`, which is about a project's dependencies.
- *`mod.rs`* holds the module declarations plus `format_bytes`.
- *`tests.rs`* holds the CLI tests. See "Building and testing" below for what is and is not covered.

## Who uses it

*Depends on:* nearly every other module. `frontend/` and `typechecker/` for the pipeline, `build/` for compilation, `project/` for manifest and root resolution, `pkg/` for dependencies, and `cache/` for the on-disk cache.

*Used by:* `src/main.rs` only.

## Gotchas

A command exits the process directly on user error, by calling `process::exit(1)` after printing a message to stderr, rather than returning a `Result` to `main`. Match that when you add one.

`jade upgrade` and `jade pkg update` are easy to confuse in help text and in commit messages. Upgrade is the toolchain. Update is the project's dependencies.

*`jade pkg add` writes before it validates, so a failure has to undo the write.* Resolving a source package and building a Rust one both read the dependency back out of `jade.toml`, so the entry has to be there first.

  What made the missing rollback worse than it sounds is that every other `pkg` command re-validates the whole manifest. One `add` that failed on a missing file made `install`, `list`, and even a later successful `add` fail on an orphan entry the user never managed to add, with nothing naming the cause.

  `fail_new_dependency` removes an entry the command created, and leaves one that was already there. `add` replaces an existing entry outright, and rolling that back would delete a working dependency to tidy up after a failed attempt to change it. Anything else that edits the manifest before validating needs the same treatment.

*The package commands are nested, so it is `jade pkg add` and never `jade add`.* This is not a style note. Before v1.1.35, every message telling a user how to recover named the unnested form. So the first thing a project with `[dependencies]` and no lock printed was an instruction to run a command that does not exist. Anything user-facing that names `add`, `remove`, `install`, `update`, or `list` needs the `pkg` in it, and that includes strings in `pkg/`, not only in this directory.

*`jade fmt` reads source text, and the text holds more than the token stream does.* The lexer throws comments away, so the formatter cannot work from tokens without losing them. That means it re-implements the parts of lexing that affect layout, and every one of them has caused a bug.

  Its scanner has to know about `//` comments, both quote characters, triple-quoted strings spanning several lines, and escapes. A `{` it misreads shifts every line after it. Three separate versions of that bug shipped before v1.1.35. The worst of them reindented the *inside* of a multi-line string and silently changed what the program printed.

  Two things guard it now. `run_fmt` re-lexes its own output and refuses to write a file whose tokens changed. And CI keeps `examples/` formatted, so the formatter meets about seventy real files on every push.

  Adding parentheses to a condition is the one change that does alter the tokens, so it runs first and checks its own result, and the whitespace pass is compared against that. CI's `jade fmt --check` skips the `*_error.jde` fixtures in `examples/control_flow/conditions/`, which hold bare conditions on purpose; the formatter would add their parentheses.

## Building and testing

```sh
cargo test cli::
./target/debug/jade env      # quickest smoke test that the binary is wired up
```

A subcommand handler cannot be tested in process. Every one ends in `process::exit`, and several read stdin, reach the network, or write under `~/.jade`.

So `tests.rs` covers the decision each command makes *before* it touches the world: how `fmt` formats, `build`'s default output path and its `[package] sources` comparison, the archive name `upgrade` picks for this platform, and what `new` scaffolds. Extract a pure helper when you add a command, or nothing will cover it.

Two rules keep the tests safe under `cargo test`'s parallelism: never call `std::env::set_var`, and never change the working directory. Anything needing the filesystem uses the `TempDir` helper at the top of `tests.rs`, which gives each test a uniquely named directory and removes it on drop.
