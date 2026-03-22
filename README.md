# Jade

A programming language written in Rust. Jade is currently in Phase 1 — the core interpreter is working and supports integer arithmetic, bitwise operations, and variable assignment.

```
let x = 255 & 0x0f
let y = x << 2
let result = y + 1
```

```
jade program.jde --verbose
```

---

## Installation

Jade is built from source. Prebuilt binaries are planned for a future release.

**Requirements**
- Rust 1.70 or later — install via [rustup.rs](https://rustup.rs)
- Git

**Build**

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build --release
```

Copy the binary to your PATH:

```sh
cp target/release/jade /usr/local/bin/jade
```

**Verify**

```sh
jade --help
```

---

## Usage

```sh
jade <file.jde>               # Run a Jade source file
jade <file.jde> --verbose     # Run and print all variables after execution
jade --help                   # Show help
```

Errors are written to stderr with the format `<file>: <phase> error: <description>`, where phase is one of `lexer`, `parse`, or `runtime`.

---

## Language — What Works Today

| Feature | Status |
|---|---|
| `let` variable declarations | ✓ |
| Integer literals (i64) | ✓ |
| Arithmetic: `+` `-` `*` `/` `%` | ✓ |
| Bitwise: `&` `\|` `^` `~` `<<` `>>` | ✓ |
| Auto-semicolon insertion | ✓ |
| Functions | Planned |
| Control flow (`if`, `while`) | Planned |
| Strings, bools, floats | Planned |
| Type inference | Planned |

Operator precedence (tightest to loosest): `~` → `*` `/` `%` → `+` `-` → `<<` `>>` → `&` → `^` → `|`

---

## Codebase

```
src/
  main.rs                   CLI entry point — argument parsing and dispatch
  cli/
    help.rs                 Prints usage text
    run.rs                  Reads a .jde file and drives the pipeline
  interpreter/
    lexer.rs                Tokenizer — produces a token stream, inserts semicolons
    parser.rs               Recursive descent parser — produces an AST
    ast.rs                  AST node definitions (Stmt, Expr, BinOpKind, UnaryOpKind)
    eval.rs                 Tree-walking evaluator — produces a variable environment
    error.rs                Error types (JadeError, Span)
tests/
  math_variable_assignment.jde    Working example of Jade syntax
planning/
  REQUIREMENTS.md           Full build plan across all phases
docs/
  index.html                Documentation website (GitHub Pages)
  extras/logo.png           Project logo
```

The pipeline is: source text → `lexer::tokenize` → `parser::parse` → `eval::evaluate` → `Env`.

---

## Documentation

Full documentation is available at the project's GitHub Pages site. The docs cover installation, language reference (variables, expressions, operators, types), CLI reference, and changelog.

---

## Contributing

**Build and test**

```sh
cargo build
cargo test
jade tests/math_variable_assignment.jde --verbose
```

**Guidelines**

- Keep one concern per file — the lexer lexes, the parser parses, the evaluator evaluates. Cross-cutting changes should touch each layer independently.
- New language features follow this path: add token(s) to `lexer.rs` → add AST node(s) to `ast.rs` → add parse rule(s) to `parser.rs` → add evaluation to `eval.rs` → add error variant(s) to `error.rs` if needed.
- Operator precedence is encoded in the parser's function call chain (`parse_bitor` → `parse_bitxor` → ... → `parse_primary`). Add a new level by inserting a new function at the right position in the chain.
- All error cases must return a `JadeError` — no panics in the interpreter path.
- The `--verbose` flag is currently the only way to observe output. Any new output mechanism should go through `cli/run.rs`.

**Issues and PRs**

Open an issue before starting significant work. Bug fixes and small improvements can be submitted directly as a PR.

---

## License

MIT — see [LICENSE](LICENSE).
