use std::{fs, path::Path, process};

use crate::{
    compiler::type_infer,
    frontend::{lexer, parser},
};

/// Run `jade check <path>`: type-check a source file without executing it.
///
/// Cache strategy:
///   L2 hit (tir.bin)  → skip everything, file previously passed check.
///   L1 hit (ast.bin)  → skip lex + parse, still run type inference.
///   Full miss         → lex → parse → type-check → write L1 + L2 caches.
pub fn run_check(path: &str) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read '{}': {}", path, e);
            process::exit(1);
        }
    };

    let hash = crate::cache::file_hash(Path::new(path));

    // L2: if TIR is cached this file already passed a previous check run.
    if let Some(ref h) = hash {
        if crate::cache::read_tir_cache(h).is_some() {
            println!("{}: ok", path);
            return;
        }
    }

    // L1: try to skip lex + parse.
    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
    let program = match cached_ast {
        Some(p) => p,
        None => {
            let tokens = match lexer::tokenize(&source) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{}: lexer error: {}", path, e);
                    process::exit(1);
                }
            };
            let p = match parser::parse(tokens) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}: parse error: {}", path, e);
                    process::exit(1);
                }
            };
            if let Some(ref h) = hash {
                crate::cache::write_ast_cache(h, path, &p);
            }
            p
        }
    };

    // Type-check.
    let tprogram = match type_infer::infer(program) {
        Ok(tp) => tp,
        Err(e) => {
            eprintln!("{}: {}", path, e);
            process::exit(1);
        }
    };

    // Write L2 cache only on success.
    if let Some(ref h) = hash {
        crate::cache::write_tir_cache(h, path, &tprogram);
    }

    println!("{}: ok", path);
}
