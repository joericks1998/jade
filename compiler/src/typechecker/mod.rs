//! The type checker: source AST → typed IR → static types.
//!
//! `type_infer` turns each file's AST into typed IR, and `infer` gives every
//! value of the merged program one static type. What consumes that answer,
//! `src/mir/` and `src/backend/`, lives alongside this module rather than
//! within it.

pub mod infer;
pub mod tir;
pub mod type_infer;

#[cfg(test)]
mod tests;
