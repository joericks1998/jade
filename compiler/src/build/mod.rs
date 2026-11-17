//! The driver: one file's tree in, a binary out.
//!
//! `cli/build.rs` lexes and parses the entry file and runs the per-file pass,
//! `typechecker/resolve.rs`. `compile` takes it from there, one step per
//! directory:
//!
//! 1. `mir::imports` merges every file the program imports into one tree,
//!    then the class rules run, struct defaults are filled, and each `?p` is
//!    rewritten into a call into the inference prelude.
//! 2. `typechecker::infer` gives every value one static type.
//! 3. `mir::lower` lowers the tree and the checker's answer into MIR, and
//!    `mir::taskcheck` refuses tasks that would race.
//! 4. `codegen` turns MIR into LLVM IR, and LLVM turns that into object
//!    files, taking the std library's compiled instances from the cache under
//!    `~/.jade/lib` when the caller names one.
//! 5. `link` joins the object files and the C library into the binary.
//!
//! `would_build` runs steps 1 to 3, for `jade check`. `--emit ir`, `--emit
//! mir` and `--emit asm` stop before linking and print the text instead.
//!
//! All of it runs in-process. This used to be a client for a build daemon on
//! `$HOME/.jade/build.sock`, when code generation lived in another
//! repository; a daemon built from an older commit resolved imports
//! differently from the CLI calling it, and stayed silent until the two
//! disagreed. Now there is one resolver, one code path and one version. The
//! cost is that building `jade` needs LLVM 18; running a released binary
//! does not, since LLVM is linked in.

use std::collections::HashMap;
use std::path::Path;

use crate::frontend::ast::Program;
use crate::mir::imports;
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
    /// The assembly LLVM writes for this machine, optimized as a binary is.
    Asm,
}

/// What `compile` produces besides the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompileOutcome {
    /// The IR, MIR or assembly text, when that was asked for.
    pub text: Option<String>,
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
        // A program with no file has no imports to resolve, but a std package
        // written in Jade that it uses is loaded all the same.
        None if imports::uses_library(&program.stmts) => {
            let stmts = imports::namespace_program(program.stmts)?;
            (Program { stmts }, None, HashMap::new())
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
    // The prelude's library modules, if it used one the program uses too,
    // are defined once.
    program.stmts = crate::stdlib::dedupe(program.stmts);
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
/// - `lib_cache`: where compiled std library instances are kept and reused,
///   `crate::codegen::default_lib_cache()` for `jade build`; `None` compiles
///   the library with the program every time. `--emit ir`, `--emit mir` and
///   `--emit asm` never use it.
///
/// Outputs: the IR, MIR or assembly text when asked for it, or the first
/// error.
pub fn compile(
    program: Program,
    source_path: Option<&Path>,
    output_path: &Path,
    emit: Emit,
    lib_cache: Option<&Path>,
) -> Result<CompileOutcome, String> {
    let merged = merge(program, source_path).map_err(|e| e.to_string())?;
    let mut mir = lower(&merged)?;
    if emit != Emit::Mir && mir.runs_a_model() {
        mir.infer = Some(model_plan(source_path)?);
    }
    match emit {
        Emit::Mir => Ok(CompileOutcome { text: Some(mir.to_string()) }),
        Emit::Ir => Ok(CompileOutcome { text: Some(crate::codegen::ir(&mir)?) }),
        Emit::Asm => Ok(CompileOutcome { text: Some(crate::codegen::asm(&mir)?) }),
        Emit::Binary => {
            binary(&mir, output_path, lib_cache)?;
            Ok(CompileOutcome { text: None })
        }
    }
}

/// Purpose: This function turns MIR into a binary: codegen writes the object
/// files, through LLVM, and the linker joins them and the C library into the
/// program.
///
/// Inputs:
/// - `mir`: the program.
/// - `out`: the binary to write.
/// - `lib_cache`: where compiled std library instances are kept, or `None`
///   to compile the library into the program's own object.
///
/// Outputs: what the build did with the library cache, or the first error.
pub fn binary(
    mir: &crate::mir::Program,
    out: &Path,
    lib_cache: Option<&Path>,
) -> Result<crate::codegen::Report, String> {
    let objects = crate::codegen::objects(mir, out, lib_cache)?;
    let linked = crate::link::link(&objects.files, out, mir.rust.as_ref());
    objects.remove();
    let mut report = objects.report;
    if let Err(e) = linked {
        if report.hits() == 0 {
            return Err(e);
        }
        // A cached object that does not link may be damaged, the one thing a
        // cache adds, so the build tries once more with the library compiled
        // into the program's object.
        let whole = crate::codegen::objects(mir, out, None)?;
        let again = crate::link::link(&whole.files, out, mir.rust.as_ref());
        whole.remove();
        again?;
        report.relinked = Some(e);
    }
    if let Some(root) = lib_cache {
        report.warn(root);
    }
    Ok(report)
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

/// Compile `program` to `out`.
///
/// `source_path` anchors import resolution: imports resolve relative to the file
/// being compiled, not the working directory. `lib_cache` is where compiled std
/// library instances are kept, `crate::codegen::default_lib_cache()` for the
/// CLI; tests pass a directory of their own, or `None` for no cache. An IR,
/// MIR or assembly emit is written to stdout.
pub fn build(
    program: &Program,
    source_path: &Path,
    out: &Path,
    emit: Emit,
    lib_cache: Option<&Path>,
) -> Result<(), String> {
    let outcome = compile(program.clone(), Some(source_path), out, emit, lib_cache)?;
    if let Some(text) = outcome.text {
        crate::stdio::write_str(&text);
        crate::stdio::flush();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
