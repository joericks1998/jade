use std::process;

/// `jade build <file.jde>` — compile to a native binary via the build daemon.
///
/// This repo runs the frontend (lex → parse → type-infer → TIR); the typed
/// program is then handed to the build daemon over `$HOME/.jade/build.sock`,
/// which performs import resolution, code generation, and linking.
pub fn run_build(path: &str, output: Option<&str>, emit_ir: bool, lib: bool, exports: &[String]) {
    {
        use std::path::{Path, PathBuf};

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

        // Absolute source path so the daemon resolves imports relative to it.
        let abs_source = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));

        // Output path: default to the input filename without its extension —
        // plus the platform's shared-library extension when building a package,
        // since `use <name>` resolves by stem and the loader needs a real
        // .dylib/.so.
        let out = match output {
            Some(o) => PathBuf::from(o),
            None => {
                let src = Path::new(path);
                let stem = src.file_stem().unwrap_or(src.as_os_str());
                let name = if lib {
                    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
                    PathBuf::from(format!("{}.{ext}", stem.to_string_lossy()))
                } else {
                    PathBuf::from(stem)
                };
                src.parent().unwrap_or(Path::new(".")).join(name)
            }
        };
        // Make the output path absolute so the daemon writes where the user expects.
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
            // A daemon predating package builds has no idea what "cdylib" means.
            // Say so plainly rather than leaving the user to decode its error —
            // the alternative failure mode is worse: an old daemon that ignores
            // the field and silently hands back an executable.
            if lib {
                eprintln!("{path}: build error: {e}");
                eprintln!(
                    "note: building a package needs a build daemon that supports \
                     `emit: cdylib`; run `jade upgrade` if yours predates it"
                );
            } else {
                eprintln!("{path}: build error: {e}");
            }
            process::exit(1);
        }

        if !emit_ir {
            eprintln!("built: {}", out.display());
        }
    }
}
