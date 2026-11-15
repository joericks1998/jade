//! The `jade build` driver: one merged, checked program in, a binary out.
//!
//! `compile` inlines a program's imports (`imports.rs`), applies the class
//! member rules, fills struct defaults, and asks `typechecker::infer` for every
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
use std::path::Path;

use crate::frontend::ast::Program;
use crate::typechecker::infer::native::NativeSig;

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
}

/// Purpose: This struct holds a program with its imports merged and its
/// defaults filled, ready for the checker.
struct Merged {
    program: Program,
    /// The Rust packages' staticlib, when the program calls one.
    rust: Option<crate::rustpkg::Link>,
    sigs: HashMap<String, NativeSig>,
}

/// Purpose: This function merges a program's imports and applies the rules
/// that run before the checker.
///
/// Inputs:
/// - `program`: the entry file's tree, after the per-file pass.
/// - `source_path`: where it is, which imports resolve from; `None` for a
///   program with no file.
///
/// Outputs: the merged program, or why import resolution or the class rules
/// refused it.
fn merge(program: Program, source_path: Option<&Path>) -> Result<Merged, imports::ResolveError> {
    let (mut program, rust, sigs) = match source_path {
        Some(src) => {
            let r = imports::resolve_and_namespace(program.stmts, src)?;
            (Program { stmts: r.stmts }, r.rust, r.native_sigs)
        }
        None => (program, None, HashMap::new()),
    };
    // Class member rules read what literals wrote, so they run before the
    // defaults are filled in. See `typechecker::infer::members`.
    crate::typechecker::infer::check_members(&program)
        .map_err(|e| imports::ResolveError::Program(e.to_string()))?;
    crate::typechecker::resolve::fill_struct_literal_defaults(&mut program)
        .map_err(|e| imports::ResolveError::Program(e.to_string()))?;
    // Each `?p` becomes a call into the inference prelude, added here.
    program.stmts =
        crate::infer::rewrite::inject(program.stmts).map_err(imports::ResolveError::Program)?;
    Ok(Merged { program, rust, sigs })
}

/// Purpose: This function lowers a merged program to MIR and runs the passes
/// over it.
///
/// Inputs:
/// - `m`: the merged program.
///
/// Outputs: the MIR, or the first error.
fn lower(m: &Merged) -> Result<crate::mir::Program, String> {
    let typed = crate::typechecker::infer::check_program_with(&m.program, &m.sigs)
        .map_err(|e| e.to_string())?;
    let mir = crate::mir::lower_program_with(&m.program, &typed, m.rust.clone(), &m.sigs)
        .map_err(|e| e.to_string())?;
    crate::mir::taskcheck::check(&mir).map_err(|e| e.to_string())?;
    Ok(mir)
}

/// Purpose: This function says whether a program would build, running every
/// step of `compile` short of LLVM.
///
/// Inputs:
/// - `program`: the entry file's tree, after the per-file pass.
/// - `source_path`: where it is.
///
/// Outputs: `Ok(())`, or the error `jade build` would report. An import that
/// does not resolve is not reported here: it means a dependency is not
/// installed yet, which `jade check`'s import walk says in better words.
pub fn would_build(program: &Program, source_path: Option<&Path>) -> Result<(), String> {
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
/// - `program`: the entry file's tree, after the per-file pass.
/// - `source_path`: where it is, which imports resolve from.
/// - `output_path`: the binary to write.
/// - `emit`: what to produce.
///
/// Outputs: the IR or MIR text when asked for it, or the first error.
pub fn compile(
    program: Program,
    source_path: Option<&Path>,
    output_path: &Path,
    emit: Emit,
) -> Result<CompileOutcome, String> {
    let merged = merge(program, source_path).map_err(|e| e.to_string())?;
    let mut mir = lower(&merged)?;
    if emit != Emit::Mir && mir.runs_a_model() {
        mir.infer = Some(model_plan(source_path)?);
    }
    match emit {
        Emit::Mir => Ok(CompileOutcome { ir: Some(mir.to_string()) }),
        Emit::Ir => {
            let ir = crate::backend::compile(&mir, output_path, crate::backend::Emit::Ir)?;
            Ok(CompileOutcome { ir })
        }
        Emit::Binary => {
            crate::backend::compile(&mir, output_path, crate::backend::Emit::Binary)?;
            Ok(CompileOutcome { ir: None })
        }
    }
}

/// Purpose: This function reads the plan of the model a program runs.
///
/// Inputs:
/// - `source_path`: the program's file, whose project may name the model.
///
/// Outputs: the plan, or a message: the machine cannot run a model yet, dovata
/// is not running, or its plan describes what the kernels cannot run.
fn model_plan(source_path: Option<&Path>) -> Result<crate::infer::plan::InferPlan, String> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("this program dereferences a prompt, and prompts compile for Apple \
                    Silicon Macs only so far; Linux and CUDA come next"
            .to_string());
    }
    let name = source_path
        .and_then(|p| p.parent())
        .and_then(crate::project::find_project_root_from)
        .and_then(|root| crate::project::load_project(&root).ok())
        .and_then(|m| m.model)
        .and_then(|m| m.name);
    crate::infer::plan::from_dovata(name.as_deref())
}

#[cfg(test)]
mod tests;
