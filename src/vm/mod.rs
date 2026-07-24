//! The bytecode VM — one of Jade's two execution engines.
//!
//! `jade run` interprets a compiled [`crate::bytecode::Chunk`] here; `jade build`
//! lowers the same chunk to LLVM in [`crate::aot`]. Neither is the reference
//! implementation of the other: `scripts/backend-parity.sh` runs every example
//! through both and diffs the output, because they have silently disagreed
//! before and the language is defined by what they agree on.
//!
//! Value semantics live in the shared `jade-runtime` crate rather than here, so
//! the two engines cannot drift on what a value *is*. What remains in this file
//! is interpretation: the dispatch loop, the call protocol, and the async and
//! prompt machinery that has no compiled counterpart.

// These imports are re-exported at `pub(crate)` so every submodule of `vm` can
// pull the whole shared set in with a single `use super::*;` — the split below
// keeps each file focused without repeating this preamble in every one.
pub(crate) use std::{sync::Arc, collections::{HashMap, HashSet}, path::PathBuf};
pub(crate) use parking_lot::Mutex;
pub(crate) use tokio::task::JoinHandle;

pub(crate) use crate::{
    builtins::{self, BuiltinFn, NativeBoundMethod, PrimType},
    compiler::{emit::CompiledProgram}, bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
    frontend::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{JadeError, Result, Span},
    },
    llm,
    native::NativeLibFn,
};
pub(crate) use jade_runtime::dynop;
pub(crate) use jade_runtime::coll::{ArrayObj, DictObj, StructObj};
pub(crate) use jade_runtime::grammarf::GrammarObj;
pub(crate) use jade_runtime::trust::JStr;

// ── Submodules (extracted from the former monolith; added incrementally) ──────
mod value;
mod state;
mod async_tasks;
mod ops;
mod chunk;
mod exceptions;
mod call;
mod llm_prompt;
mod coerce;
mod dispatch;
pub use value::*;
pub use state::*;
pub use chunk::*;
pub(crate) use async_tasks::*;
pub(crate) use ops::*;
pub(crate) use exceptions::*;
pub(crate) use call::*;
pub(crate) use llm_prompt::*;
pub(crate) use coerce::*;
pub(crate) use dispatch::*;
// Tests for this module live in `src/compiler/tests.rs` (`mod vm`).

#[cfg(test)]
mod tests;
