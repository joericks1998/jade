# Jade for VS Code

Syntax highlighting for the [Jade programming language](https://jadelang.org), in `.jde` files.

The extension colors Jade's keywords, types, strings and f-string interpolation, numbers, decorators, function definitions and calls, and the prompt operators, with prompts in jade green. It also sets up comment toggling, bracket matching and auto-closing pairs for `.jde` files.

Install it from the Visual Studio Marketplace by searching for *Jade Language*, or with:

```sh
code --install-extension joericks.jadelang-extension
```

## Working on the extension

This directory is the extension's source. It moved here from `joericks1998/jadelang-extension` at 1.1.2, with its history, so it lives beside the compiler whose syntax it colors. It is not a Cargo crate and is not in the workspace; it has its own version, in `package.json`, and its own releases.

What each file does:

- `package.json` is the extension's manifest. It names the `jadelang` language, ties `.jde` files to it, points at the grammar, and holds the default color for each scope the grammar gives, under `configurationDefaults`.
- `syntaxes/jade.tmLanguage.json` is the TextMate grammar: the rules that split a line into scopes, such as `keyword.other.prompt.jade`.
- `language-configuration.json` sets comments, brackets and auto-closing pairs.
- `assets/Vector.svg` is the file icon for `.jde` files.
- `docs/jade-syntax-spec.md` lists every Jade syntax construct and whether the grammar colors it. It was written against Jade 1.1.12, so check it against the current language before trusting it.
- `examples/demo.jde` is a file to open while working on the grammar, to see each rule at once.
- `.vscodeignore` lists what the `.vsix` leaves out: the files above that are for working on the extension, not for running it.
- `.vscode/launch.json` runs the extension in a new VS Code window. Open this directory, `vscode/`, as the workspace folder and press F5.

To try a change, open `vscode/` in VS Code, press F5, and open `examples/demo.jde` in the window that starts. *Developer: Inspect Editor Tokens and Scopes* shows the scope under the cursor.

## Releasing

The extension releases from its own version, the same way the compiler and dovata do:

1. Work on a branch named for the new version, such as `vscode-1.1.4`.
2. Bump `version` in `package.json`, and add a `## vscode v1.1.4` heading to `docs/docs/changelog.md`.
3. Merge to `main`. `.github/workflows/vscode-ci.yml` packages the extension, and if the tag `vscode-v1.1.4` does not exist, it pushes the tag and starts `vscode-release.yml`, which publishes to the Marketplace and attaches the `.vsix` to a GitHub release.

Publishing needs the `VSCE_PAT` secret, a Marketplace token for the `joericks` publisher. `.github/workflows/README.md` explains both workflows.
