//! The compiler proper: source AST → typed IR → bytecode.
//!
//! Ends at a [`crate::bytecode::Chunk`]. What consumes that chunk, the LLVM
//! backend in `src/codegen/`, is not this module's concern; it lives alongside
//! it rather than within it.

pub mod emit;
pub mod escape;
pub mod gbnf;
pub mod taskcheck;
pub mod tir;
pub mod type_infer;

#[cfg(test)]
mod tests;
