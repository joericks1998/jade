use std::{fs, path::Path, process};

#[cfg(feature = "llvm")]
use crate::compiler::codegen;

pub fn run_build(path: &str, output: Option<&str>, emit_ir: bool) {
    #[cfg(not(feature = "llvm"))]
    {
        let _ = (path, output, emit_ir);
        eprintln!("error: jade was not compiled with LLVM support.");
        eprintln!("       Rebuild with:  cargo build --release --features llvm");
        process::exit(1);
    }

    #[cfg(feature = "llvm")]
    {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: could not read '{}': {}", path, e);
                process::exit(1);
            }
        };

        // Lex + parse.
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

        // Type inference.
        let tprogram = match crate::compiler::type_infer::infer(program) {
            Ok(tp) => tp,
            Err(e) => {
                eprintln!("{path}: type error: {e}");
                process::exit(1);
            }
        };

        // Determine output path.
        let out = match output {
            Some(o) => std::path::PathBuf::from(o),
            None => {
                // Strip extension from input file and use that as the binary name.
                let src = Path::new(path);
                let stem = src.file_stem().unwrap_or(src.as_os_str());
                src.parent()
                    .unwrap_or(Path::new("."))
                    .join(stem)
            }
        };

        // LLVM codegen + link.
        if let Err(e) = codegen::compile(tprogram, Some(std::path::Path::new(path)), &out, emit_ir) {
            eprintln!("{path}: build error: {e}");
            process::exit(1);
        }

        if !emit_ir {
            eprintln!("built: {}", out.display());
        }
    }
}
