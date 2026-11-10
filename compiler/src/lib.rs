//! The `jade` library crate.
//!
//! Exposes the whole toolchain: the frontend (lex → parse), the type checker,
//! MIR, the LLVM backend, prompts, and the package manager. The `jade` binary
//! (`src/main.rs`) is a thin CLI on top of this.

// Jade is Unix-only. `std/uhttp` speaks HTTP over a Unix domain socket, a
// prompt gets its model's memory from the dovata process over one, and the
// support code the backend writes into each program is written against POSIX,
// so a Windows build could only ever be a language subset with the interesting
// half stubbed out. Failing here is clearer than shipping that: a build error
// names the constraint, a silently degraded binary doesn't.
#[cfg(not(unix))]
compile_error!(
    "Jade supports Unix-like platforms only (macOS and Linux). \
     Windows is not a supported target; on Windows, build inside WSL2."
);

/// The driver: merges imports, then runs the checker, MIR, the task check and
/// the backend. `jade build` calls straight into this, so LLVM 18 is a
/// build-time requirement for the toolchain (locate it with
/// `LLVM_SYS_180_PREFIX`).
pub mod aot;
/// MIR → LLVM with no runtime library, linked by LLD against the C library
/// alone. Support code is emitted into each program as it is used.
pub mod backend;
pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
pub mod frontend;
/// Prompts: the model's plan from dovata, the Jade tokenizer prelude, and the
/// Metal kernels. Type inference is `typechecker`, not this.
pub mod infer;
/// The typed middle layer between the checked program and the backend: one
/// function per checked instance, typed locals, basic blocks, and reference
/// counting written out.
pub mod mir;
pub mod pkg;
pub mod project;
pub mod stdio;
/// The type checker: `type_infer` types each file into TIR, and `infer` gives
/// every value of the merged program one static type.
pub mod typechecker;

// Native built-in packages (the `std/*` + core intrinsic registry).
// The registry types live in `builtins`; each package is a flat top-level module.
pub mod array;
pub mod bytes;
pub mod core;
pub mod dict;
pub mod env;
pub mod fs;
pub mod future;
pub mod http;
pub mod json;
pub mod math;
pub mod path;
pub mod random;
pub mod sh;
pub mod string;
pub mod time;
pub mod uhttp;
