use std::process;

/// `jade build <file.jde>` — compile to a native binary via the build daemon.
///
/// This repo runs the frontend (lex → parse → type-infer → TIR); the typed
/// program is then handed to the build daemon over `$HOME/.jade/build.sock`,
/// which performs import resolution, code generation, and linking.
pub fn run_build(path: &str, output: Option<&str>, emit_ir: bool) {
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

        // Output path: default to the input filename without its extension.
        let out = match output {
            Some(o) => PathBuf::from(o),
            None => {
                let src = Path::new(path);
                let stem = src.file_stem().unwrap_or(src.as_os_str());
                src.parent().unwrap_or(Path::new(".")).join(stem)
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

        if let Err(e) = crate::build::build(&tprogram, &abs_source, &abs_out, emit_ir) {
            eprintln!("{path}: build error: {e}");
            process::exit(1);
        }

        if !emit_ir {
            eprintln!("built: {}", out.display());
        }
    }
}
