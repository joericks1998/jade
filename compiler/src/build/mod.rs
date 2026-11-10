//! Native compilation: TIR → LLVM → object → linked artifact.
//!
//! This used to be a client for a build daemon listening on
//! `$HOME/.jade/build.sock`, because code generation and the C runtime lived in
//! a separate repository and keeping LLVM out of the `jade` binary was worth a
//! socket for it. Both halves of that argument are gone: `src/aot/` owns the
//! driver and `src/backend/` the code generation, so the daemon's only job was
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

use crate::typechecker::tir::TProgram;

pub use crate::aot::Emit;

/// Compile `program` to `out`.
///
/// `source_path` anchors import resolution: imports resolve relative to the file
/// being compiled, not the working directory.
/// Returns the native packages the artifact will look for at startup, so the
/// caller can copy exactly those beside it. Empty for an IR or MIR emit, which
/// writes no artifact to put anything beside.
pub fn build(
    program: &TProgram,
    source_path: &Path,
    out: &Path,
    emit: Emit,
) -> Result<Vec<std::path::PathBuf>, String> {
    let outcome = crate::aot::compile(program.clone(), Some(source_path), out, emit)?;
    if let Some(text) = outcome.ir {
        crate::stdio::write_str(&text);
        crate::stdio::flush();
        return Ok(Vec::new());
    }
    Ok(outcome.dependencies)
}

#[cfg(test)]
mod tests;
