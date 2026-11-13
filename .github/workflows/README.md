# workflows: testing and releasing the compiler, dovata and the VS Code extension

This repo holds three things that ship, and each one is tested, tagged and released from its own version:

| what | lives in | version in | tags | workflows |
|---|---|---|---|---|
| `jade`, the compiler | `compiler/` | `compiler/Cargo.toml` | `v2.0.1` | `ci.yml`, `release.yml` |
| `dovata`, the model memory process | `dovata/` | `dovata/Cargo.toml` | `dovata-v3.2.0` | `dovata-ci.yml`, `dovata-release.yml` |
| the VS Code extension | `vscode/` | `vscode/package.json` | `vscode-v1.1.3` | `vscode-ci.yml`, `vscode-release.yml` |

`deploy-docs.yml` publishes jadelang.org from `docs/` and has nothing to do with any release.

## How a release happens

A release starts with a crate's version changing, and nothing else. For dovata:

1. Change `version` in `dovata/Cargo.toml`, for example to `3.2.1`, and run `cargo build -p dovata` so the repo's `Cargo.lock` follows. dovata's CI builds with `--locked`, so a bump without the lockfile fails.
2. Merge the change to `main`.
3. `dovata-ci.yml` builds and tests dovata. When that passes, its `tag-release` job reads `dovata/Cargo.toml`, and if the tag `dovata-v3.2.1` does not exist yet, it pushes the tag.
4. A tag pushed with the workflow's own token starts no other workflow, so `tag-release` sends a `repository_dispatch` event named `release-dovata` instead.
5. `dovata-release.yml` builds `dovata` and `gguf-inspect` on each platform, checks that `dovata --version` matches the tag, and publishes a GitHub release with the archives, `checksums.txt` and generated release notes.

The compiler works the same way, with `compiler/Cargo.toml`, `ci.yml`, a `v2.0.2` tag, the dispatch event `release`, and `release.yml`. So does the extension, with `vscode/package.json`, `vscode-ci.yml`, a `vscode-v1.1.3` tag, the dispatch event `release-vscode`, and `vscode-release.yml`, which publishes to the Visual Studio Marketplace.

A merge that changes no version tags nothing and releases nothing. A merge that changes one releases that one alone, so a dovata fix ships without a new compiler, and the other way around.

## Why each workflow only runs for its own directory

`ci.yml` ignores `dovata/**` and `vscode/**`, and `dovata-ci.yml` runs only for `dovata/**`, the root `Cargo.toml` and `Cargo.lock`, and its own workflow files. A change to dovata alone does not install LLVM 18 and build the compiler, and a change to the compiler does not run dovata's tests. The root `Cargo.toml` is the workspace's alone, with no version of its own, and the lockfile is shared, so a change to either runs both. `vscode-ci.yml` runs for `vscode/**`, its own workflow files and any `.jde` file, since its grammar test tokenizes every example, and builds no Rust.

Every cargo command in dovata's workflows names dovata's packages with `-p`, never `--workspace`, because the workspace holds the compiler too, and the compiler needs LLVM 18 to build. The compiler's commands need no `-p`: the workspace's `default-members` is `compiler`, so a bare `cargo build` at the root builds `jade` alone. The one exception is `cargo fmt -p jade --check`, since `cargo fmt --all` would reach dovata, which is not formatted with rustfmt.

## The latest release is the compiler's

GitHub marks one release per repo as the latest. `jade upgrade` and the compiler's `install.sh` ask GitHub for it and read its tag as jade's version, so it has to be the compiler's. `release.yml` publishes with `make_latest: true`, and `dovata-release.yml` and `vscode-release.yml` with `make_latest: false`, an input of `softprops/action-gh-release` (see its `action.yml`). `dovata/install.sh` lists the releases and takes the newest whose tag starts with `dovata-v`.

## Releasing by hand

Pushing a tag by hand also starts the matching release workflow: a `v*` tag starts `release.yml`, a `dovata-v*` tag starts `dovata-release.yml`, and a `vscode-v*` tag starts `vscode-release.yml`. A `v*` pattern does not match `dovata-v3.2.0` or `vscode-v1.1.3`, since a tag pattern matches from the start of the name. So does a manual run with a tag as input, such as `gh workflow run dovata-release.yml -f tag=dovata-v3.2.0`, which attaches the programs to that tag's release even if the release already exists.

Never re-cut an existing tag. A new version is a new tag.

## Files

- `ci.yml` builds, tests and lints the compiler on Ubuntu, then runs the example checks, the golden outputs and the FFI gate. On `main` it also runs `tag-release` for the compiler.
- `release.yml` builds `jade` for Linux x86_64 and Apple Silicon and publishes it.
- `dovata-ci.yml` builds and tests dovata's three crates on Ubuntu and macOS: the tests with every capability compiled out, refback's Metal tests on macOS, the quantize tests on both through the llama.cpp build `dovata setup` installs, `dovata machine info` on the runner, and clippy. On `main` it also runs `tag-release` for dovata.
- `dovata-release.yml` builds `dovata` and `gguf-inspect` for Linux x86_64 and Apple Silicon and publishes them. `refback` is the test-only reference backend and is never published.
- `vscode-ci.yml` checks that the extension's JSON files parse, runs `vscode/test/`, which tokenizes a case for each construct in the language and every `.jde` file in the repo with the engine VS Code uses, and packages the extension with `vsce`. On `main` it also runs `tag-release` for the extension.
- `vscode-release.yml` packages the extension, publishes it to the Visual Studio Marketplace with the `VSCE_PAT` secret, a Marketplace token for the `joericks` publisher, and attaches the `.vsix` to a GitHub release.
- `deploy-docs.yml` builds and publishes the docs site when `docs/` or `dovata/install.sh` changes on `main`. It copies `dovata/install.sh` into the site, so both installers come from jadelang.org: `/install.sh` for the compiler and `/dovata/install.sh` for dovata.

## Who uses it

`docs/static/install.sh` and `jade upgrade` download what `release.yml` publishes. `dovata/install.sh` and `dovata upgrade` download what `dovata-release.yml` publishes. A Rust project that takes dovata as a cargo dependency pins a `dovata-v*` tag.
