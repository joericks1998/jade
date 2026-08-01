use std::{
    path::{Path, PathBuf},
    process,
};

/// Where a build lands when the user did not say.
///
/// The default is the source file's own name without its extension, beside the
/// source.  A `--lib` build gets the platform's shared-library extension
/// instead, because `use <name>` resolves a package by stem and the loader
/// needs a real `.dylib`/`.so` to open.
pub(crate) fn output_path(source: &str, output: Option<&str>, lib: bool) -> PathBuf {
    if let Some(o) = output {
        return PathBuf::from(o);
    }
    let src = Path::new(source);
    let stem = src.file_stem().unwrap_or(src.as_os_str());
    let name = if lib {
        let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        PathBuf::from(format!("{}.{ext}", stem.to_string_lossy()))
    } else {
        PathBuf::from(stem)
    };
    src.parent().unwrap_or(Path::new(".")).join(name)
}

/// `jade build <file.jde>` — compile to a native binary.
///
/// Runs the whole pipeline in-process: lex → parse → type-infer → TIR, then
/// import resolution, LLVM code generation, and linking.
pub fn run_build(path: &str, output: Option<&str>, emit_ir: bool, lib: bool, exports: &[String]) {
    {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: could not read '{}': {}", path, e);
                process::exit(1);
            }
        };

        // Frontend: lex → parse → type-infer.
        let tokens = match crate::frontend::lexer::tokenize(&source) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{path}: lexer error: {e}");
                process::exit(1);
            }
        };
        let program = match crate::frontend::parser::parse(tokens) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{path}: parse error: {e}");
                process::exit(1);
            }
        };
        let tprogram = match crate::compiler::type_infer::infer(program) {
            Ok(tp) => tp,
            Err(e) => {
                eprintln!("{path}: type error: {e}");
                process::exit(1);
            }
        };

        // Absolute source path so imports resolve relative to it, not the CWD.
        let abs_source = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));

        let out = output_path(path, output, lib);
        // Make the output path absolute so the artifact lands where the user expects.
        let abs_out = if out.is_absolute() {
            out.clone()
        } else {
            std::env::current_dir()
                .map(|d| d.join(&out))
                .unwrap_or_else(|_| out.clone())
        };

        let emit = if emit_ir {
            crate::build::Emit::Ir
        } else if lib {
            crate::build::Emit::CDylib { exports: exports.to_vec() }
        } else {
            crate::build::Emit::Binary
        };

        if let Err(e) = crate::build::build(&tprogram, &abs_source, &abs_out, emit) {
            eprintln!("{path}: build error: {e}");
            process::exit(1);
        }

        if !emit_ir {
            eprintln!("built: {}", out.display());
        }
    }
}
