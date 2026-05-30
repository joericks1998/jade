//! The `jade` library crate.
//!
//! Exposes the language frontend (lex → parse → type-infer → TIR) and compiler
//! internals so out-of-process tools — notably the Jade build daemon, which
//! owns LLVM codegen + linking — can reuse them instead of duplicating the
//! frontend. The `jade` binary (`src/main.rs`) is a thin CLI on top of this.

pub mod build;
pub mod cache;
pub mod cli;
pub mod compiler;
pub mod config;
pub mod frontend;
pub mod llm;
pub mod native;
pub mod project;
