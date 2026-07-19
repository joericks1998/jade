//! The `jade` library crate.
//!
//! Exposes the language frontend (lex → parse → type-infer → TIR) and compiler
//! internals so out-of-process tools — notably the Jade build daemon, which
//! owns LLVM codegen + linking — can reuse them instead of duplicating the
//! frontend. The `jade` binary (`src/main.rs`) is a thin CLI on top of this.

// Jade is Unix-only. The core of the toolchain is built on Unix domain sockets —
// `jade build` talks to the build daemon, and the `jade` inference provider talks
// to the LLM daemon — so a Windows build could only ever be a language subset
// with the interesting half stubbed out. Failing here is clearer than shipping
// that: a build error names the constraint, a silently degraded binary doesn't.
#[cfg(not(unix))]
compile_error!(
    "Jade supports Unix-like platforms only (macOS and Linux). \
     Windows is not a supported target; on Windows, build inside WSL2."
);

pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
/// AOT backend: bytecode `Chunk` → LLVM → native binary (needs LLVM 18). Merged
/// into this crate from the former `jade-codegen`; `jade-buildd` calls
/// `codegen::compile`. The C runtime AOT binaries link (`runtime_lib/`) is built
/// by this crate's `build.rs`.
///
/// Behind the `codegen` feature, off by default: linking LLVM 18 would otherwise
/// be a hard requirement for every build of the `jade` CLI, which never calls
/// into this module — `jade build` hands TIR to the daemon over a socket.
#[cfg(feature = "codegen")]
pub mod codegen;
pub mod compiler;
pub mod config;
pub mod frontend;
pub mod llm;
pub mod native;
pub mod project;
pub mod stdio;

// Native built-in packages (the `std/*` + core intrinsic registry).
// The registry types live in `builtins`; each package is a flat top-level module.
pub mod array;
pub mod core;
pub mod dict;
pub mod env;
pub mod fs;
pub mod grammar;
pub mod http;
pub mod json;
pub mod math;
pub mod path;
pub mod random;
pub mod sh;
pub mod string;
pub mod time;
pub mod uhttp;
