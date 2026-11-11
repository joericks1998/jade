# Jade

*The AI-native programming language.*

Most languages treat a model call like any other library call. You get a string back, and then you parse it, validate it, and write the retry loop yourself. Jade moves that work into the language. A prompt is a type. Dereferencing it calls the model. The type you ask for is a contract the compiler enforces.

```jade
prompt p = "How many moons does Mars have? Reply with just the number."

let n = ?p |> int      // n is an int, or the program raises. Never a string to parse.
```

That `|> int` runs in two stages. First, naming a type builds a grammar that limits how the model generates its reply. A reply shaped like prose or wrapped in a code fence is ruled out before a single token of it exists. Second, whatever comes back is coerced to the type you named. If it still does not fit, the program raises `PromptOverflow` instead of handing you a string to parse. The glue code you would otherwise write by hand is the language's job.

On this branch, Jade 2.0 compiles model inference into the program itself. A program that dereferences a prompt runs a model the dovata process holds, as Metal kernels on Apple Silicon, and `?p |> int`, `float`, `bool` and `char` read their value out of the reply. The grammar and `?p |> Class` are not compiled yet, and Linux and CUDA come next.

Prompts live wherever values live, including inside classes. You dereference them right where you use them.

```jade
class Agent {
    @public prompt system = "You are a careful code reviewer."
}

let a = Agent {}
let review = a~>system        // or the explicit a.(?system)
```

*This README is for people working on Jade.* If you want to use Jade, install it, learn the syntax, or browse the standard library, go to [jadelang.org](https://jadelang.org).

---

## What is in this repo

The repo holds two programs. Each is a directory with its own `Cargo.toml`, `README.md` and `src/`, and each has its own version and its own releases.

| directory | what it is |
|---|---|
| [`compiler/`](compiler/README.md) | `jade`, the compiler and the rest of the toolchain: lexer, parser, type checker, LLVM backend, package manager and CLI. Start here to work on the language. |
| [`dovata/`](dovata/README.md) | the process that holds a model's memory, plans where every tensor lives, and hands that memory to compiled programs. |

The rest is shared by both:

- [`examples/`](examples/README.md) is the `.jde` fixture suite. CI checks every one, and builds and runs most of them.
- [`design/`](design/README.md) holds design documents for work in progress.
- [`docs/`](docs/README.md) is the jadelang.org site, for people using Jade.
- [`bench/`](bench/README.md) holds benchmarks against CPython.
- [`.github/workflows/`](.github/workflows/README.md) tests and releases both programs.

The root `Cargo.toml` is the workspace and nothing else. A bare `cargo build` or `cargo test` here means the compiler; name dovata's packages with `-p`, as in `cargo build -p dovata`.

---

## Build from source

```sh
git clone https://github.com/joericks1998/jade
cd jade
cargo build              # the compiler, at target/debug/jade
cargo build -p dovata    # dovata, at target/debug/dovata
```

The compiler needs Rust 1.85 or later and LLVM 18; [`compiler/README.md`](compiler/README.md#build-from-source) says how to install them. dovata needs no LLVM.

---

## Sending a change

Open an issue before starting anything substantial. Bug fixes and small improvements can go straight to a pull request.

Run these before you open it:

```sh
# The compiler
cargo build
cargo test
cargo clippy --all-targets
cargo fmt -p jade --check

# dovata, if you changed it
cargo test -p dovata -p dovata-chat-template -p refback
cargo clippy -p dovata -p dovata-chat-template -p refback --all-targets
```

CI is defined in `.github/workflows/`, one workflow per program, and runs on every pull request against `main` that touches that program. The compiler's, `ci.yml`, runs its four commands above, then `jade check` and `jade fmt --check` over every fixture in `examples/`, then two gates: `compiler/src/scripts/golden.sh`, which builds every example and compares its output with the recorded `.expected` file, and `compiler/src/scripts/ffi-gate.sh`, which builds and runs real C bindings.

Clippy runs without `-D warnings`, so existing warnings do not fail the build. Do not add new ones.

One rule to know before you touch a version. *A version bump ships a release.* Each program releases from its own version: the compiler from `compiler/Cargo.toml`, tagged `v2.0.1`, and dovata from `dovata/Cargo.toml`, tagged `dovata-v3.2.0`. On merge to `main`, each program's CI reads its version, and if no matching tag exists it creates one and starts that program's release build. Only bump a version when you mean to publish it. The details are in [`.github/workflows/README.md`](.github/workflows/README.md).

---

## License

MIT. See [LICENSE](LICENSE).
