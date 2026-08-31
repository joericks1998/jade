//! The `jade` library crate.
//!
//! Exposes the whole toolchain: the language frontend (lex → parse →
//! type-infer → TIR), the bytecode VM, the AOT LLVM backend, and the package
//! manager. The `jade` binary (`src/main.rs`) is a thin CLI on top of this.

// Jade is Unix-only. Native packages — the inference provider among them — are
// loaded with dlopen, `std/uhttp` speaks HTTP over a Unix domain socket, and the
// C runtime is written against POSIX — so a Windows build could only ever be a
// language subset with the interesting half stubbed out. Failing here is clearer
// than shipping that: a build error names the constraint, a silently degraded
// binary doesn't.
#[cfg(not(unix))]
compile_error!(
    "Jade supports Unix-like platforms only (macOS and Linux). \
     Windows is not a supported target; on Windows, build inside WSL2."
);

/// The `jade` binary's global allocators: the Phase-1 segregated free-list pool
/// (`alloc::pool`) and the Phase-0 profiler behind `--features alloc-profile`
/// (`alloc::profile`). Host-only — the `#[global_allocator]` declarations
/// themselves live in `main.rs`, never in `jade-runtime`, so neither can reach a
/// dlopen'd package the way mimalloc did.
pub mod alloc;

/// AOT driver: import resolution, then `codegen`, then object and link. `jade
/// build` calls straight into this, so LLVM 18 is a build-time requirement for
/// the toolchain (locate it with `LLVM_SYS_180_PREFIX`). The C runtime that
/// emitted binaries link against is `src/runtime_aot/` (C), built by this
/// crate's `build.rs`.
pub mod aot;
pub mod build;
pub mod builtins;
/// The instruction set both engines consume: the compiler emits a `Chunk`, the
/// VM interprets it and `codegen` lowers it. It belongs to neither engine,
/// which is why it sits between them rather than inside `compiler`.
pub mod bytecode;
pub mod cache;
pub mod cli;
/// Code generation: bytecode `Chunk` → LLVM IR. The half of `jade build` that
/// is about the language rather than about producing a file, which is why it
/// sits beside `aot` rather than inside it.
pub mod codegen;
pub mod compiler;
/// Bytecode interpreter — one of the two execution engines, peer to `aot`
/// rather than a phase of `compiler`. `jade run` uses this; `jade build` uses
/// `aot`. Backend parity exists to keep the two agreeing.
pub mod vm;
// `src/runtime_aot/` is C, not Rust, so it has no module declaration here. It
// is the runtime linked into AOT-compiled binaries only — the exception
// handler, the platform/concurrency shim, and the IPC and inference clients —
// and `build.rs` compiles it to `libJadeRuntime.a`. Distinct from
// `jade-runtime` (`src/runtime/`), which is Rust and shared by *both* engines.
pub mod frontend;
pub mod llm;
pub mod native;
pub mod pkg;
pub mod project;
pub mod providers;
pub mod stdio;

// Native built-in packages (the `std/*` + core intrinsic registry).
// The registry types live in `builtins`; each package is a flat top-level module.
pub mod array;
pub mod bytes;
pub mod core;
pub mod dict;
pub mod env;
pub mod fs;
pub mod future;
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
