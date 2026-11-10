//! The `jade build` driver: one merged, checked program in, a binary out.
//!
//! `compile` inlines a program's imports (`imports.rs`), applies the class
//! member rules, fills struct defaults, and asks `compiler::infer` for every
//! value's static type. `mir` lowers that answer into typed functions,
//! `mir::taskcheck` refuses tasks that would race, and `backend` turns MIR
//! into LLVM IR, an object file and a program linked against the C library
//! alone. `would_build` runs the same steps short of LLVM, for `jade check`.
//!
//! Until phase 6 of `design/v2-static-values.md` this file also drove v1's
//! backend, which compiled bytecode against a runtime library. That backend is
//! gone, and this is the only path.

pub mod imports;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::compiler::infer::native::NativeSig;
use crate::compiler::tir::TProgram;

/// What a build produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Emit {
    /// A native executable linked against the C library alone.
    #[default]
    Binary,
    /// The LLVM IR, unoptimized.
    Ir,
    /// The MIR, as text.
    Mir,
}

/// What `compile` produces besides the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompileOutcome {
    /// The IR or MIR text, when that was asked for.
    pub ir: Option<String>,
    /// The native packages from the project's `libs/` the binary will look
    /// for beside itself, for the caller to copy there.
    pub dependencies: Vec<PathBuf>,
}

/// Purpose: This struct holds a program with its imports merged and its
/// defaults filled, ready for the checker.
struct Merged {
    program: TProgram,
    natives: Vec<imports::NativePkg>,
    sigs: HashMap<String, NativeSig>,
}

/// Purpose: This function merges a program's imports and applies the rules
/// that run before the checker.
///
/// Inputs:
/// - `program`: the entry file's typed tree.
/// - `source_path`: where it is, which imports resolve from; `None` for a
///   program with no file.
///
/// Outputs: the merged program, or why import resolution or the class rules
/// refused it.
fn merge(program: TProgram, source_path: Option<&Path>) -> Result<Merged, imports::ResolveError> {
    let (mut program, natives, sigs) = match source_path {
        Some(src) => {
            let r = imports::resolve_and_namespace(program.stmts, src)?;
            (TProgram { stmts: r.stmts }, r.native_pkgs, r.native_sigs)
        }
        None => (program, Vec::new(), HashMap::new()),
    };
    // Class member rules read what literals wrote, so they run before the
    // defaults are filled in. See `compiler::infer::members`.
    crate::compiler::infer::check_members(&program)
        .map_err(|e| imports::ResolveError::Program(e.to_string()))?;
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut program)
        .map_err(|e| imports::ResolveError::Program(e.to_string()))?;
    Ok(Merged { program, natives, sigs })
}

/// Purpose: This function lowers a merged program to MIR and runs the passes
/// over it.
///
/// Inputs:
/// - `m`: the merged program.
///
/// Outputs: the MIR, or the first error.
fn lower(m: &Merged) -> Result<crate::mir::Program, String> {
    let typed = crate::compiler::infer::check_program_with(&m.program, &m.sigs)
        .map_err(|e| e.to_string())?;
    let libs = m
        .natives
        .iter()
        .map(|p| crate::mir::NativeLib {
            id: p.id,
            name: p.name.clone(),
            rel: p.rel.clone(),
            abs: p.abs.clone(),
        })
        .collect();
    let mir = crate::mir::lower_program_with(&m.program, &typed, libs, &m.sigs)
        .map_err(|e| e.to_string())?;
    crate::mir::taskcheck::check(&mir).map_err(|e| e.to_string())?;
    Ok(mir)
}

/// Purpose: This function says whether a program would build, running every
/// step of `compile` short of LLVM.
///
/// Inputs:
/// - `program`: the entry file's typed tree.
/// - `source_path`: where it is.
///
/// Outputs: `Ok(())`, or the error `jade build` would report. An import that
/// does not resolve is not reported here: it means a dependency is not
/// installed yet, which `jade check`'s import walk says in better words.
pub fn would_build(program: &TProgram, source_path: Option<&Path>) -> Result<(), String> {
    let merged = match merge(program.clone(), source_path) {
        Ok(m) => m,
        Err(imports::ResolveError::Unresolved(_)) => return Ok(()),
        Err(imports::ResolveError::Program(m)) => return Err(m),
    };
    lower(&merged).map(|_| ())
}

/// Purpose: This function compiles a program.
///
/// Inputs:
/// - `program`: the entry file's typed tree.
/// - `source_path`: where it is, which imports resolve from.
/// - `output_path`: the binary to write.
/// - `emit`: what to produce.
///
/// Outputs: the IR or MIR text when asked for it, and after writing a binary,
/// the native packages it will look for beside itself; or the first error.
pub fn compile(
    program: TProgram,
    source_path: Option<&Path>,
    output_path: &Path,
    emit: Emit,
) -> Result<CompileOutcome, String> {
    let merged = merge(program, source_path).map_err(|e| e.to_string())?;
    let mir = lower(&merged)?;
    match emit {
        Emit::Mir => Ok(CompileOutcome { ir: Some(mir.to_string()), dependencies: Vec::new() }),
        Emit::Ir => {
            let ir = crate::backend::compile(&mir, output_path, crate::backend::Emit::Ir)?;
            Ok(CompileOutcome { ir, dependencies: Vec::new() })
        }
        Emit::Binary => {
            crate::backend::compile(&mir, output_path, crate::backend::Emit::Binary)?;
            // Only the packages from the project's `libs/` are the caller's to
            // copy beside the artifact.
            let dependencies = merged
                .natives
                .iter()
                .filter(|p| p.rel.is_some())
                .map(|p| PathBuf::from(&p.abs))
                .collect();
            Ok(CompileOutcome { ir: None, dependencies })
        }
    }
}

#[cfg(test)]
mod tests;
