//! The `jade` library crate.
//!
//! Exposes the whole toolchain: the frontend (lex → parse), the type checker,
//! MIR, codegen, the linker, prompts, and the package manager. The `jade` binary
//! (`src/main.rs`) is a thin CLI on top of this.

// Jade is Unix-only. `std/uhttp` speaks HTTP over a Unix domain socket, a
// prompt gets its model's memory from the dovata process over one, and the
// support code codegen writes into each program is written against POSIX,
// so a Windows build could only ever be a language subset with the interesting
// half stubbed out. Failing here is clearer than shipping that: a build error
// names the constraint, a silently degraded binary doesn't.
#[cfg(not(unix))]
compile_error!(
    "Jade supports Unix-like platforms only (macOS and Linux). \
     Windows is not a supported target; on Windows, build inside WSL2."
);

/// The driver: merges imports, then runs the checker, MIR, the task check,
/// codegen and the linker. `jade build` calls straight into this, so LLVM 18
/// is a build-time requirement for the toolchain (locate it with
/// `LLVM_SYS_180_PREFIX`).
pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
/// MIR → LLVM IR → object files, with no runtime library: the support code
/// each program uses is written into it. LLVM turns the IR into machine code.
pub mod codegen;
pub mod frontend;
/// Prompts: the model's plan from dovata, the Jade tokenizer prelude, and the
/// Metal kernels. Type inference is `typechecker`, not this.
pub mod infer;
/// Object files → a binary, with LLD inside the compiler, against the C
/// library alone.
pub mod link;
/// The typed middle layer between the checked program and codegen: imports
/// merged into one program, the checked program lowered into one function
/// per checked instance with typed locals, basic blocks and reference
/// counting written out, and the task check over it.
pub mod mir;
pub mod pkg;
pub mod project;
/// Rust packages: the generated crate a project's Rust dependencies are built
/// in, cargo, and the description records read out of the built library.
pub mod rustpkg;
pub mod stdio;
/// The std library written in Jade: each package's `.jde` source, embedded,
/// and the registry the import pass loads them from. Its directory is
/// `src/lib/`, which a module named `lib` beside `lib.rs` cannot use.
#[path = "lib/mod.rs"]
pub mod stdlib;
/// The type checker: `resolve` rewrites each file's tree on its own, and
/// `infer` gives every value of the merged program one static type.
pub mod typechecker;

// Native built-in packages (the `std/*` + core intrinsic registry).
// The registry types live in `builtins`; each package is a flat top-level module.
pub mod array;
pub mod bytes;
pub mod core;
pub mod dict;
pub mod future;
pub mod string;
