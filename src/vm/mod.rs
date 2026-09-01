//! The bytecode VM — one of Jade's two execution engines.
//!
//! `jade run` interprets a compiled [`crate::bytecode::Chunk`] here; `jade build`
//! lowers the same chunk to LLVM in [`crate::aot`]. Neither is the reference
//! implementation of the other: `src/scripts/backend-parity.sh` runs every example
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
pub(crate) use parking_lot::Mutex;
pub(crate) use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
pub(crate) use tokio::task::JoinHandle;

pub(crate) use crate::{
    builtins::{self, BuiltinFn, NativeBoundMethod, PrimType},
    bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
    compiler::emit::CompiledProgram,
    frontend::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{JadeError, Result, Span},
    },
    llm,
    native::NativeLibFn,
};
pub(crate) use jade_runtime::coll::{ArrayObj, DictObj, StructObj};
pub(crate) use jade_runtime::dynop;
pub(crate) use jade_runtime::grammarf::GrammarObj;
pub(crate) use jade_runtime::trust::JStr;

// ── Submodules (extracted from the former monolith; added incrementally) ──────
// `pub(crate)` for `value_type_name`, which the provider backend uses to name the
// type of a frame it cannot read.
pub mod async_tasks;
mod call;
mod chunk;
pub(crate) mod coerce;
mod dispatch;
mod exceptions;
mod llm_prompt;
mod ops;
mod state;
pub(crate) mod value;
pub(crate) use async_tasks::*;
pub(crate) use call::*;
pub use chunk::*;
pub(crate) use coerce::*;
pub(crate) use dispatch::*;
pub(crate) use exceptions::*;
pub(crate) use llm_prompt::*;
pub(crate) use ops::*;
pub use state::*;
pub use value::*;
// Tests for this module live in `src/compiler/tests.rs` (`mod vm`).

#[cfg(test)]
mod tests;
