# Jade

A programming language written in Rust. Jade 1.1.5 compiles programs through a type inference pass and a register-based bytecode VM. It supports value types (`int`, `float`, `bool`, `str`, arrays, dicts, and user-defined `struct`s), `let` bindings, bare variable assignment, `fn` function definitions with `return`, anonymous closures, first-class functions, recursion, `if`/`elif`/`else` control flow, `while` loops, `for` loops over arrays, `try`/`catch`/`raise` exception handling, `struct` definitions with field access and mutation, `extend` blocks for methods, `interface` definitions, multi-file `use` imports, the `print` and `len` built-ins, f-string interpolation, the pipe operator `|>`, `prompt` declarations with LLM inference via `?`, and arithmetic, bitwise, logical, and comparison operators. String literals accept both double quotes (`"…"`) and single quotes (`'…'`), including triple-quoted variants.

```
fn factorial(n) {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}

struct Counter {
    count
}

extend Counter {
    fn increment(self) {
        self.count = self.count + 1
    }
    fn value(self) {
        return self.count
    }
}

let x = 10
let result = factorial(x)
print(f"factorial({x}) = {result}")

let c = Counter { count: 0 }
c.increment()
c.increment()
print(c.value())
```

```
jade program.jde --verbose
```

---

## Installation

### macOS and Linux (recommended)

```sh
curl -fsSL https://jadelang.org/install.sh | sh
```

The script detects your OS and architecture, downloads the correct prebuilt binary from the [latest release](https://github.com/joericks1998/jade/releases/latest), and installs it to `/usr/local/bin/jade`. Set `JADE_INSTALL_DIR` to override the destination.

### Windows

Download `jade-windows-x86_64.exe` from the [latest release](https://github.com/joericks1998/jade/releases/latest), rename it to `jade.exe`, and place it on your `PATH`.

### Build from source

Requires Rust 1.70+ — install via [rustup.rs](https://rustup.rs).

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build --release
cp target/release/jade /usr/local/bin/jade
```

**Verify**

```sh
jade --help
```

---

## Usage

```sh
jade run <file.jde>           # Run a Jade source file
jade run <file.jde> --verbose # Run and print all variables after execution
jade run                      # Run the project entry point (main.jde)
jade check <file.jde>         # Type-check without executing
jade repl                     # Start an interactive REPL
jade test                     # Discover and run test files
jade fmt <file.jde>           # Format source files
jade new myapp                # Create a new project
jade env                      # Show version, config, and cache info
jade upgrade                  # Upgrade jade to the latest release
jade --help                   # Show help
```

Errors are written to stderr with the format `<file>: <phase> error: <description>`, where phase is one of `lexer`, `parse`, or `runtime`.

---

## Language — What Works Today

| Feature | Status |
|---|---|
| `let` variable declarations | ✓ |
| Integer literals (`i64`) | ✓ |
| Float literals (`f64`) | ✓ |
| Boolean literals (`true`/`false`) | ✓ |
| Arithmetic: `+` `-` `*` `/` `%` | ✓ |
| Bitwise: `&` `\|` `^` `~` `<<` `>>` | ✓ |
| Logical: `&&` `\|\|` `!` | ✓ |
| Comparison: `==` `!=` `<` `>` `<=` `>=` | ✓ |
| `fn` definitions and calls | ✓ |
| `return` statement | ✓ |
| `if`/`else` control flow | ✓ |
| First-class functions | ✓ |
| Recursion | ✓ |
| Auto-semicolon insertion | ✓ |
| `while` loops | ✓ |
| Bare variable assignment (`x = expr`) | ✓ |
| `struct` definitions and instantiation | ✓ |
| Field access and field mutation | ✓ |
| `extend` blocks and method calls | ✓ |
| String literals (`str`) — double or single quotes, triple-quoted variants | ✓ |
| F-string interpolation (`f"…{expr}…"` or `f'…{expr}…'`) | ✓ |
| Array literals with index access and assignment | ✓ |
| `print` and `len` built-in functions | ✓ |
| Pipe operator `\|>` | ✓ |
| `interface` definitions and conformance checking | ✓ |
| `elif` chained conditionals | ✓ |
| `jade configure` for LLM backend setup | ✓ |
| `prompt` declarations and `?` LLM dereference | ✓ |
| `dict` type with key access and assignment | ✓ |
| Anonymous closures (`\|x\| expr`, block-body) | ✓ |
| `for` loops over arrays | ✓ |
| Multi-file `use` imports | ✓ |
| Bytecode compiler and register-based VM | ✓ |
| Type inference | ✓ |
| `try`/`catch`/`raise` exception handling | ✓ |
| `async fn` definitions and `await` expressions | ✓ |

Operator precedence (tightest to loosest): unary (`~` `!` `-`) → `*` `/` `%` → `+` `-` → `<<` `>>` → `&` → `^` → `|` → `==` `!=` `<` `>` `<=` `>=` → `&&` → `||` → `|>`

---

## Codebase

```
src/
  main.rs                   CLI entry point — argument parsing and dispatch
  cli/
    help.rs                 Prints usage text
    run.rs                  Reads a .jde file and drives the full pipeline
    configure.rs            Interactive wizard for LLM backend configuration
    check.rs                Static check command (jade check <file>)
  cache/                    Multi-level AST and TIR caching (file-hash keyed)
  compiler/
    type_infer.rs           Type inference pass — produces typed IR (TIR)
    emit.rs                 Bytecode emitter — lowers TIR to Chunk of Instructions
    bytecode.rs             Bytecode instruction set and Chunk definition
    vm.rs                   Register-based bytecode VM
    tir.rs                  Typed IR node definitions
  config/                   Loads jade.toml and environment variables
  llm/                      LLM inference backends (Anthropic, OpenAI)
  interpreter/
    lexer.rs                Tokenizer — produces a token stream, inserts semicolons
    parser.rs               Recursive descent parser — produces an AST
    ast.rs                  AST node definitions (Stmt, Expr, BinOpKind, UnaryOpKind)
    eval.rs                 Tree-walking evaluator (used by tests and check command)
    error.rs                Error types (JadeError, Span)
jade_evals/
  arithmatic/               Fixture files for arithmetic and bitwise operations
  arrays/                   Fixture files for array literals, indexing, and assignment
  assignment/               Fixture files for let bindings, assignment, and comparison expressions
  control_flow/             Fixture files for if/else/elif, nested if, while, and for loops
  functions/                Fixture files for fn definitions, calls, recursion, first-class fns
  interfaces/               Fixture files for interface definitions and conformance
  llm/                      Fixture files for prompt declarations and LLM dereference
  strings/                  Fixture files for string literals, f-strings, and indexing
  structs/                  Fixture files for struct definitions, field access, extend blocks
  dicts/                    Fixture files for dict literals, key access, and mutation
  imports/                  Fixture files for multi-file use imports
  pipe.jde                  Fixture file for the |> pipe operator
  closures.jde              Fixture file for anonymous closures
  for_loop.jde              Fixture file for for loops
planning/
  REQUIREMENTS.md           Full build plan across all phases
docs/
  index.html                Documentation website (jadelang.org)
  CNAME                     Custom domain configuration for GitHub Pages
  extras/logo.png           Project logo
```

The pipeline is: source text → `lexer::tokenize` → `parser::parse` → `type_infer::infer` → `emit::emit` → `vm::run`.

---

## Documentation

Full documentation is available at **[jadelang.org](https://jadelang.org)**. The docs cover installation, language reference (variables, expressions, operators, types), CLI reference, and changelog.

---

## Contributing

**Build and test**

```sh
cargo build
cargo test
jade run jade_evals/arithmatic/arithmetic.jde --verbose
jade run jade_evals/strings/fstrings.jde
```

**Guidelines**

- Keep one concern per file — the lexer lexes, the parser parses, the evaluator evaluates. Cross-cutting changes should touch each layer independently.
- New language features follow this path: add token(s) to `lexer.rs` → add AST node(s) to `ast.rs` → add parse rule(s) to `parser.rs` → add evaluation to `eval.rs` → add error variant(s) to `error.rs` if needed.
- Operator precedence is encoded in the parser's function call chain (`parse_bitor` → `parse_bitxor` → ... → `parse_primary`). Add a new level by inserting a new function at the right position in the chain.
- All error cases must return a `JadeError` — no panics in the interpreter path.
- Use `print(expr)` to produce output from Jade code. The `--verbose` flag prints all top-level variable bindings after execution. Both paths go through `cli/run.rs`.

**Issues and PRs**

Open an issue before starting significant work. Bug fixes and small improvements can be submitted directly as a PR.

---

## License

MIT — see [LICENSE](LICENSE).
