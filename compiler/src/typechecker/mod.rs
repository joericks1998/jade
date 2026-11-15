//! The type checker: the parser's tree → the same tree, rewritten → static types.
//!
//! `resolve` rewrites each file's tree on its own: it flattens pipes, folds a
//! converter around a dereference into it, fills class literal defaults, and
//! refuses what one file can settle, such as an undefined name. `infer` gives
//! every value of the merged program one static type. What consumes that
//! answer, `src/mir/` and `src/backend/`, lives alongside this module rather
//! than within it.

pub mod infer;
pub mod resolve;

#[cfg(test)]
mod tests;
