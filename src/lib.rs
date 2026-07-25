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

/// Phase-0 allocation profiler (feature `alloc-profile`). Host-only; see the
/// module docs. The `#[global_allocator]` itself is declared in `main.rs`.
#[cfg(feature = "alloc-profile")]
pub mod alloc_profile;

/// Phase-1 segregated free-list allocator. Installed as the `jade` binary's
/// global allocator in `main.rs` — host-only, never in `jade-runtime`, so it
/// cannot reach a dlopen'd package the way mimalloc did.
pub mod pool_alloc;

pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
/// AOT backend: bytecode `Chunk` → LLVM → object → linked artifact. `jade build`
/// calls straight into this, so LLVM 18 is a build-time requirement for the
/// toolchain (locate it with `LLVM_SYS_180_PREFIX`). The C runtime that emitted
/// binaries link against is `src/runtime_aot/` (C), built by this crate's `build.rs`.
pub mod aot;
pub mod compiler;
/// The instruction set both engines consume: the compiler emits a `Chunk`, the
/// VM interprets it and `aot` lowers it. It belongs to neither engine, which
/// is why it sits between them rather than inside `compiler`.
pub mod bytecode;
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
