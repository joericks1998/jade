//! The compiler proper: source AST → typed IR → bytecode.
//!
//! Ends at a [`crate::bytecode::Chunk`]. What consumes that chunk — the VM or
//! the LLVM backend — is not this module's concern; both live alongside it
//! rather than within it.

pub mod emit;
pub mod escape;
pub mod taskcheck;
pub mod gbnf;
pub mod tir;
pub mod type_infer;

#[cfg(test)]
mod tests;
