//! The `jade` library crate.
//!
//! Exposes the whole toolchain: the language frontend (lex → parse →
//! type-infer → TIR), the bytecode VM, the AOT LLVM backend, and the package
//! manager. The `jade` binary (`src/main.rs`) is a thin CLI on top of this.

// Jade is Unix-only. The `jade` inference provider talks to the LLM daemon over
// a Unix domain socket, native packages are loaded with dlopen, and the C
// runtime is written against POSIX — so a Windows build could only ever be a
// language subset with the interesting half stubbed out. Failing here is clearer
// than shipping that: a build error names the constraint, a silently degraded
// binary doesn't.
#[cfg(not(unix))]
compile_error!(
    "Jade supports Unix-like platforms only (macOS and Linux). \
     Windows is not a supported target; on Windows, build inside WSL2."
);

pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
/// AOT backend: bytecode `Chunk` → LLVM → object → linked artifact. `jade build`
/// calls straight into this, so LLVM 18 is a build-time requirement for the
/// toolchain (locate it with `LLVM_SYS_180_PREFIX`). The C runtime that emitted
/// binaries link against (`runtime_lib/`) is built by this crate's `build.rs`.
pub mod codegen;
pub mod compiler;
pub mod config;
pub mod frontend;
pub mod llm;
pub mod native;
pub mod pkg;
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
