use std::{fs, path::Path, process};

use crate::{
    compiler::type_infer,
    frontend::{lexer, parser},
};

/// Checks that run after type inference, on both the cached and uncached paths.
///
/// Two of them. `emit` is where shared-mutation-across-tasks is rejected: the
/// mutation opcodes (`SetGlobal`/`SetIndex`/`SetField`) only exist in bytecode,
/// because the AST's assignment expression cannot distinguish rebinding a local
/// from writing through a reference. And the import walk confirms every `use`
/// names something that exists.
///
/// Both are here so `jade check` is an honest predictor of whether `jade run` /
/// `jade build` will accept the file, which the `*_error.jde` fixture convention
/// depends on. The import walk was missing until v1.1.33, and its absence made
/// that claim false for any file with an import: `use totally_made_up_module`
/// reported `ok` and then failed at run time.
fn post_tir_checks(tprogram: &crate::compiler::tir::TProgram, path: &str) -> Result<(), String> {
    crate::compiler::emit::emit(tprogram.clone()).map_err(|e| e.to_string())?;
    check_imports(tprogram, path)?;
    // Everything call-shaped is decided in the backend's resolver, not in type
    // inference: an unknown method, the wrong arity, a surplus argument to a
    // builtin. Without this, `check` answered `ok` for every one of them and
    // the build then refused — the wrong way round for the command that exists
    // to predict the build. Lowering into a throwaway module is what `jade
    // build` already does as its own probe, and it costs milliseconds.
    crate::aot::would_build(tprogram, Some(Path::new(path)))
}

/// Resolve every import reachable from this file, without loading any of them.
///
/// The project context is read from the *source file's* directory rather than
/// the current one, matching `jade run` and `jade build`: which project a file
/// belongs to is a property of the file. Reading it from the CWD is the bug
/// v1.1.31 fixed for the other two commands.
///
/// Unlike `jade run`, this does not call `pkg::ensure_ready` — checking a file
/// should not reach the network to fetch dependencies. A project whose `libs/`
/// has not been populated yet will therefore report its dependency imports as
/// unresolved, which is accurate: they cannot be loaded until `jade pkg install`
/// runs.
fn check_imports(tprogram: &crate::compiler::tir::TProgram, path: &str) -> Result<(), String> {
    use crate::compiler::tir::TStmt;

    let paths: Vec<_> = tprogram
        .stmts
        .iter()
        .filter_map(|s| match s {
            TStmt::Use { path, span, .. } | TStmt::FromUse { path, span, .. } => {
                Some((path.clone(), span.clone()))
            }
            _ => None,
        })
        .collect();

    let source_dir = Path::new(path)
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let project_root = crate::project::find_project_root_from(&source_dir);
    // A manifest error is a check failure, and it is read before the imports
    // are counted. `jade check` claims to predict what `jade run` will do, and
    // `jade run` reads jade.toml whether or not the file it was handed imports
    // anything — so returning early on a file with no `use` would have `check`
    // pass a file `run` refuses.
    let manifest = match project_root.as_ref().map(|root| crate::project::load_project(root)) {
        Some(Ok(m)) => Some(m),
        Some(Err(e)) => return Err(e.to_string()),
        None => None,
    };

    if paths.is_empty() {
        return Ok(());
    }

    // A dependency whose prototypes are still `"?"` is one `jade run` refuses,
    // and reading the manifest for it costs nothing beyond the read that just
    // happened. Reporting it here is what keeps `jade check` an honest
    // predictor without reaching for the network.
    if let Some(m) = &manifest {
        crate::pkg::check_symbols_resolved(m)?;
    }

    let libraries = project_root
        .as_ref()
        .zip(manifest.as_ref())
        .map(|(root, m)| crate::pkg::resolved_libraries(root, m))
        .unwrap_or_default();

    let ctx = crate::project::ImportContext {
        libraries: &libraries,
        project_root: project_root.as_deref(),
        source_dir,
    };
    crate::project::walk_imports(&paths, &ctx).map_err(|e| e.to_string())
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
            if let Err(e) = post_tir_checks(&tprogram, path) {
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

    if let Err(e) = post_tir_checks(&tprogram, path) {
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

    /// Check a file through the same stages as `run_check`, minus the caching and
    /// the `process::exit` calls. Deliberately cache-free so the result depends
    /// only on what is on disk.
    ///
    /// Takes a path rather than a source string because the import walk needs
    /// one: which file a `use` resolves to depends on the importing file's
    /// directory and project root. Passing source text alone is what let a
    /// fixture with a broken import look fine here.
    fn check_file(path: &Path) -> Result<(), String> {
        let source = fs::read_to_string(path).map_err(|e| format!("unreadable: {e}"))?;
        let tokens = lexer::tokenize(&source).map_err(|e| format!("lexer error: {e}"))?;
        let program = parser::parse(tokens).map_err(|e| format!("parse error: {e}"))?;
        let tprogram = type_infer::infer(program).map_err(|e| format!("{e}"))?;
        crate::compiler::emit::emit(tprogram.clone()).map_err(|e| format!("{e}"))?;
        check_imports(&tprogram, &path.to_string_lossy())
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
        path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.ends_with("_error"))
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
            let rel =
                path.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap_or(path).display().to_string();
            match (check_file(path), expects_failure(path)) {
                // Expected outcomes.
                (Ok(()), false) | (Err(_), true) => {}
                (Err(e), false) => problems.push(format!("{rel}: expected ok, got {e}")),
                (Ok(()), true) => problems.push(format!(
                    "{rel}: named '*_error' but check accepted it — \
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
