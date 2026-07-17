//! The `jade` library crate.
//!
//! Exposes the language frontend (lex → parse → type-infer → TIR) and compiler
//! internals so out-of-process tools — notably the Jade build daemon, which
//! owns LLVM codegen + linking — can reuse them instead of duplicating the
//! frontend. The `jade` binary (`src/main.rs`) is a thin CLI on top of this.

pub mod build;
pub mod builtins;
pub mod cache;
pub mod cli;
pub mod compiler;
pub mod config;
pub mod frontend;
pub mod llm;
pub mod native;
pub mod project;

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
#[cfg(unix)]
pub mod uhttp;
