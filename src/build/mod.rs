//! Native compilation: TIR → LLVM → object → linked artifact.
//!
//! This used to be a client for a build daemon listening on
//! `$HOME/.jade/build.sock`, because code generation and the C runtime lived in
//! a separate repository and keeping LLVM out of the `jade` binary was worth a
//! socket for it. Both halves of that argument are gone: `src/aot/` owns the
//! backend and the C runtime now, so the daemon's only remaining job was
//! forwarding a request to a function this crate already exports.
//!
//! Compiling in-process removes the failure mode that motivated the change — a
//! daemon built from an older commit resolving imports differently from the CLI
//! calling it, which stays silent until the two disagree. There is now one
//! resolver, one code path, one version.
//!
//! The cost is that building `jade` needs LLVM 18 present. *Running* a released
//! binary does not — LLVM is linked in, not loaded.

use std::path::Path;

use crate::compiler::tir::TProgram;

/// What to produce.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Emit {
    /// A native executable.
    #[default]
    Binary,
    /// LLVM IR, printed to stdout rather than linked.
    Ir,
    /// A shared library exporting `jade_pkg_init` — a Jade package other
    /// projects can depend on. `exports` narrows which functions are bound;
    /// empty means all of them.
    CDylib { exports: Vec<String> },
}

impl From<&Emit> for crate::aot::CompileMode {
    fn from(emit: &Emit) -> Self {
        match emit {
            Emit::CDylib { exports } => {
                crate::aot::CompileMode::SharedLib { exports: exports.clone() }
            }
            // IR is printed rather than linked, so the mode only decides which
            // entry point gets generated — the binary shape is the right one.
            Emit::Binary | Emit::Ir => crate::aot::CompileMode::Binary,
        }
    }
}

/// Compile `program` to `out`.
///
/// `source_path` anchors import resolution: imports resolve relative to the file
/// being compiled, not the working directory.
/// Returns the native packages the artifact will look for at startup, so the
/// caller can copy exactly those beside it. Empty for an IR emit, which writes
/// no artifact to put anything beside.
pub fn build(
    program: &TProgram,
    source_path: &Path,
    out: &Path,
    emit: Emit,
) -> Result<Vec<std::path::PathBuf>, String> {
    let outcome = crate::aot::compile_with_mode(
        program.clone(),
        Some(source_path),
        out,
        emit == Emit::Ir,
        (&emit).into(),
    )?;

    // `compile_with_mode` returns the IR text only when asked for it; a binary
    // or package build writes to `out` and returns None.
    if let Some(text) = outcome.ir {
        crate::stdio::write_str(&text);
        crate::stdio::flush();
        return Ok(Vec::new());
    }

    Ok(outcome.dependencies)
}

#[cfg(test)]
mod tests;
