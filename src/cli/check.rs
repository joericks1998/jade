use std::{fs, path::Path, process};

use crate::{
    compiler::type_infer,
    frontend::{lexer, parser},
};

/// Checks that run after type inference, on both the cached and uncached paths.
///
/// Currently just `emit`, which is where shared-mutation-across-tasks is
/// rejected: the mutation opcodes (`SetGlobal`/`SetIndex`/`SetField`) only exist
/// in bytecode, because the AST's assignment expression cannot distinguish
/// rebinding a local from writing through a reference. Emitting here keeps
/// `jade check` an honest predictor of whether `jade run`/`jade build` will
/// accept the file, which the `*_error.jde` fixture convention depends on.
fn post_tir_checks(tprogram: &crate::compiler::tir::TProgram) -> Result<(), String> {
    crate::compiler::emit::emit(tprogram.clone()).map(|_| ()).map_err(|e| e.to_string())
}

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

    // L2: cached TIR means lex + parse + infer already succeeded for this exact
    // source. It does NOT mean the file still passes: the cache key is the file
    // hash, so an entry written before a new check existed would skip that check
    // forever. Re-run the post-TIR checks against the cached tree rather than
    // returning early — they are cheap next to the stages the cache saves.
    if let Some(ref h) = hash {
        if let Some(tprogram) = crate::cache::read_tir_cache(h) {
            if let Err(e) = post_tir_checks(&tprogram) {
                eprintln!("{}: {}", path, e);
                process::exit(1);
            }
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

    if let Err(e) = post_tir_checks(&tprogram) {
        eprintln!("{}: {}", path, e);
        process::exit(1);
    }

    // Write L2 cache only on success.
    if let Some(ref h) = hash {
        crate::cache::write_tir_cache(h, path, &tprogram);
    }

    println!("{}: ok", path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type-check a source string through the same stages as `run_check`, minus
    /// the caching and the `process::exit` calls.  Deliberately cache-free so the
    /// result depends only on the source.
    fn check_source(source: &str) -> Result<(), String> {
        let tokens = lexer::tokenize(source).map_err(|e| format!("lexer error: {e}"))?;
        let program = parser::parse(tokens).map_err(|e| format!("parse error: {e}"))?;
        let tprogram = type_infer::infer(program).map_err(|e| format!("{e}"))?;
        crate::compiler::emit::emit(tprogram).map_err(|e| format!("{e}"))?;
        Ok(())
    }

    /// Every `.jde` file under `examples/`, sorted for deterministic output.
    fn example_files() -> Vec<std::path::PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => panic!("cannot read {}: {e}", dir.display()),
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "jde") {
                    out.push(path);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();
        files
    }

    /// Fixtures named `*_error.jde` document a rejected program and are expected
    /// to fail; everything else is expected to type-check.
    fn expects_failure(path: &Path) -> bool {
        path.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with("_error"))
    }

    /// `examples/` is the fixture-first workflow's source of truth, so a stale
    /// example teaches the wrong thing. This pins every one of them to whatever
    /// the compiler actually does today.
    #[test]
    fn every_example_matches_its_expected_check_result() {
        let files = example_files();
        assert!(!files.is_empty(), "no .jde fixtures found under examples/");

        // Collect every mismatch rather than tripping on the first, so a single
        // run reports the full list.
        let mut problems = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .display()
                .to_string();
            let source = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    problems.push(format!("{rel}: unreadable: {e}"));
                    continue;
                }
            };
            match (check_source(&source), expects_failure(path)) {
                // Expected outcomes.
                (Ok(()), false) | (Err(_), true) => {}
                (Err(e), false) => problems.push(format!("{rel}: expected ok, got {e}")),
                (Ok(()), true) => problems.push(format!(
                    "{rel}: named '*_error' but type-checked cleanly — \
                     rename it or make it genuinely invalid"
                )),
            }
        }

        assert!(
            problems.is_empty(),
            "{} of {} example fixture(s) disagree with the compiler:\n  {}",
            problems.len(),
            files.len(),
            problems.join("\n  ")
        );
    }
}
