---
id: changelog
title: Changelog
sidebar_label: Changelog
---

## v1.1.9

- **Breaking:** module-path imports now use `::` as the separator instead of `.` — `use std::math`, `from std::math use floor`, `use utils::math` for `[lib]` libraries. The `.` form is no longer accepted in module-path position (`.` is reserved for field and method access on values); `use std.math` is now a parse error
- Namespaced decorators also use `::` — `@tools::register` instead of `@tools.register`
- Quoted file-path imports (`use "lib.jde" as lib`) are unchanged
- Added `null` as a third spelling of `nil` — `nil`, `None`, and `null` are interchangeable aliases for the same value; they compare equal and may be used as literals, default parameter values, and type annotations

## v1.1.8

- Native code generation moved out of the `jade` binary — `jade build` now runs the language frontend (lex → parse → type-infer → typed IR) and hands the typed program to the **build daemon** over `$HOME/.jade/build.sock`, which performs import resolution, code generation, and linking. The in-process LLVM backend and the `llvm` Cargo feature were removed; `jade env` now reports build-daemon reachability instead of LLVM status
- Stdlib package imports must now use dot notation — `use std.math`, `use std.fs`, etc.; string-literal forms (`use "std/math"`) are now a compile-time error. Applies to both `use` and `from … use` forms
- File-path imports now require an alias — `use "lib.jde" as lib`; bare string imports without `as name` are now a compile-time error
- Native packages declared in `jade.toml [native]` now require an `alias` field specifying the global binding name
- Fixed: functions exported from imported modules can now access stdlib packages the module imported (e.g. `use std.fs` in a module is visible when module functions are called in the parent scope)
- Improved error messages — type errors now include the actual type of the offending value; heterogeneous array literals, nested function definitions, and non-string prompt struct fields each emit a dedicated error
- Added empty struct test coverage (`struct Unit {}`)

## v1.1.7

- Added `std/sh` package — execute shell commands from Jade via `sh.exec`, `sh.run`, and `sh.output`
- Added `std/json` package — parse JSON strings into Jade values and serialize Jade values back to JSON with `json.parse`, `json.stringify`, and `json.stringify_pretty`
- Added `std/env` package — read and write environment variables (`env.get`, `env.set`), inspect command-line arguments (`env.args`), and get the working directory (`env.cwd`)
- Added `std/path` package — cross-platform path manipulation: `path.join`, `path.basename`, `path.dirname`, `path.ext`, `path.stem`, `path.abs`, `path.is_abs`
- Added `std/random` package — random number generation with `random.int`, `random.float`, `random.choice`, `random.shuffle`, and a seedable global RNG via `random.seed`

## v1.1.6

- Added `input(prompt?)` built-in — reads a line from stdin; the optional `prompt` argument prints to stdout without a trailing newline before reading. Returns an empty string on EOF.
- Added `write(str)` built-in — prints to stdout without a trailing newline and flushes immediately (complements `print`, which adds `\n`)
- Fixed array mutation semantics — mutations to an array are now visible through all aliases (reference semantics); previously mutations did not propagate to other variables pointing at the same array
- Added `llm.set_max_tokens(n)` via `use "llm"` — configure the maximum token limit for LLM inference at runtime
- Extended LLVM native codegen: typed `try`/`catch` arms and struct method calls (`obj.method(args)`) now compile and run correctly in native binaries

## v1.1.5

- Added single-quote string literals — `'hello'` and `'''triple'''` are now equivalent to their double-quote forms; `f'…{expr}…'` f-strings work too
- Fixed `jade.toml` config loading — a config-only file with only a `[model]` section (no `[project]`) is now correctly picked up
- Added `jade upgrade` command — downloads and atomically replaces the binary from the latest GitHub release

## v1.1.4

- Added `async fn` definitions and `await` expressions — concurrent LLM inference via `await` on prompt dereferences
- Added Jade OS as a supported LLM backend provider
- Added comprehensive error handling for async tasks — panics from spawned tasks produce `AsyncPanic` errors with source location
- Switched TLS backend to `rustls` (no OpenSSL dependency)

## v1.1.3

- Added official install script at `https://jadelang.org/install.sh` — detects OS and architecture, downloads the correct prebuilt binary, and installs to `/usr/local/bin/jade`
- Added Windows prebuilt binary: `jade-windows-x86_64.exe` available from the GitHub Releases page
- Updated documentation installation page to document the install script and Windows download path

## v1.0.9

- Added `try`/`catch`/`raise` exception handling — raise any value as an exception, catch by struct type name or with a catch-all arm, nested `try`/`catch` blocks, built-in runtime errors (division by zero, type errors, etc.) are automatically catchable
- Upgraded CLI to full subcommand structure: `jade run`, `jade check`, `jade build`, `jade repl`, `jade test`, `jade fmt`, `jade env`, `jade cache`, `jade model`, `jade new`, `jade init`
- Fixed implicit function return: the last bare expression in a function body is now returned automatically without needing an explicit `return` keyword

## v1.0.8

- Added anonymous closures: `|x| x * 2` (inline expression body) and `|x| { … }` (block body) with environment capture at creation time
- Added `for` loops: `for x in array { … }` iteration over arrays (via bytecode VM)
- Added `dict` type: dictionary literals (`{"key": value}`), key access (`d["key"]`), key assignment, and `len` support
- Added `use "path.jde"` for multi-file imports
- Added bytecode compiler and VM — programs now run through type inference, bytecode emission, and a register-based VM
- Added multi-level AST and TIR caching to skip redundant compilation passes

## v1.0.7

- Added `str` type: string literals, triple-quoted strings, concatenation with `+`, character indexing, equality and lexicographic ordering
- Added f-string interpolation: `f"…{expr}…"` and `f"""…{expr}…"""`
- Added array literals (`[1, 2, 3]`), index access (`arr[i]`), and index assignment (`arr[i] = expr`)
- Added `print` and `len` built-in functions
- Added pipe operator `|>` for chaining function calls
- Added `interface` definitions and `extend Type: Interface` conformance checking
- Added `elif` clause for chained conditionals
- Added `jade configure` command for LLM backend configuration
- Added `prompt` declarations and `?` dereference for LLM inference

## v1.0.6

- Added `struct` definitions with named fields, field access, and field mutation
- Added `extend` blocks for attaching methods, with `self` binding
- Added bare variable assignment (`x = expr`)
- Added `while` loops with boolean condition

## v1.0.5

- Added `struct` definitions with named fields
- Added struct instantiation with `TypeName { field: value, … }` literals
- Added field access (`obj.field`) and field mutation (`obj.field = expr`)
- Added `extend` blocks for attaching methods to struct types
- Added method calls (`obj.method(args)`) with automatic `self` binding
- Added bare variable assignment (`x = expr`) as an alternative to `let` rebinding

## v1.0.4

- Added `while` loops with boolean condition

## v1.0.3

- Added `fn` definitions with parameter lists and `return`
- Added function calls as first-class expressions
- Added first-class function values — functions can be assigned to variables and passed as arguments
- Added recursion — functions can call themselves
- Added `if`/`else` control flow

## v1.0.2

- Modulus operator: `%`
- Bitwise operators: `&`, `|`, `^`, `<<`, `>>`
- Unary bitwise NOT: `~`
- Float literals (`f64`) and unary negation for floats
- Boolean literals: `true`, `false`
- Logical operators: `&&`, `||`, `!` with short-circuit evaluation
- Comparison operators: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Runtime errors: remainder by zero, invalid shift amount (negative or ≥ 64)

## v1.0.1

- Initial interpreter release written in Rust
- `let` variable declarations with arithmetic expressions
- Operators: `+`, `-`, `*`, `/`
- Automatic semicolon insertion — no semicolons required
- Runtime errors: undefined variable, division by zero
- CLI: `jade <file>`, `--verbose`, `--help`
