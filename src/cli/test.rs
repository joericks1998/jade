use std::{path::PathBuf, process};

/// `jade test [pattern] [-v]`
pub async fn run_test(pattern: Option<&str>, verbose: bool) {
    // Find test files relative to project root (or CWD).
    let root = crate::project::find_project_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let files = crate::project::find_test_files(&root, pattern);

    if files.is_empty() {
        if let Some(pat) = pattern {
            eprintln!("no test files matching '{}' found", pat);
        } else {
            eprintln!("no test files found (expected test_*.jde or *_test.jde)");
        }
        return;
    }

    println!("running {} test{}", files.len(), if files.len() == 1 { "" } else { "s" });

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for file in &files {
        let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();

        print!("  {} ... ", name);

        // Fix 5: pass &Path directly instead of converting to an owned String.
        match run_test_file(file, verbose).await {
            Ok(()) => {
                println!("ok");
                passed += 1;
            }
            Err(msg) => {
                println!("FAILED");
                failures.push((name, msg));
                failed += 1;
            }
        }
    }

    println!();
    if failures.is_empty() {
        println!("test result: ok. {} passed; 0 failed", passed);
    } else {
        println!("failures:");
        for (name, msg) in &failures {
            println!("  {} — {}", name, msg);
        }
        println!();
        println!("test result: FAILED. {} passed; {} failed", passed, failed);
        process::exit(1);
    }
}

/// Run a single test file.  Returns `Ok(())` on clean exit, `Err(msg)` on any error.
///
/// Uses `select_backend` (same as `jade run`) so the daemon socket drives
/// inference, matching the path that regular `jade run` uses.
async fn run_test_file(path: &std::path::Path, _verbose: bool) -> Result<(), String> {
    use crate::{
        compiler::{emit, type_infer},
        vm,
    };

    let source =
        std::fs::read_to_string(path).map_err(|e| format!("could not read file: {}", e))?;

    // Lex + parse (with cache).
    let hash = crate::cache::file_hash(path);
    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
    let program = match cached_ast {
        Some(p) => p,
        None => {
            let tokens = crate::frontend::lexer::tokenize(&source)
                .map_err(|e| format!("lexer error: {}", e))?;
            let p = crate::frontend::parser::parse(tokens)
                .map_err(|e| format!("parse error: {}", e))?;
            if let Some(ref h) = hash {
                // to_string_lossy only where a &str is truly needed (cache key display).
                crate::cache::write_ast_cache(h, &path.to_string_lossy(), &p);
            }
            p
        }
    };

    // Type inference.
    let tprogram = if let Some(ref h) = hash {
        match crate::cache::read_tir_cache(h) {
            Some(tp) => tp,
            None => {
                let tp = type_infer::infer(program).map_err(|e| format!("type error: {}", e))?;
                crate::cache::write_tir_cache(h, &path.to_string_lossy(), &tp);
                tp
            }
        }
    } else {
        type_infer::infer(program).map_err(|e| format!("type error: {}", e))?
    };

    // Emit + run.
    let compiled = emit::emit(tprogram).map_err(|e| format!("compile error: {}", e))?;

    // Connect to the inference daemon if it's running, same as `jade run`.
    let backend = crate::llm::select_backend();

    let source_dir = path
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // Honor registered [lib] libraries and locked dependencies so test files
    // can import them too. Resolved from the test file's own directory, same rule
    // as `jade run` and `jade build`: a file belongs to the project it sits in.
    let project_root = crate::project::find_project_root_from(&source_dir);
    let libraries = project_root
        .as_ref()
        .and_then(|root| {
            // Reported rather than skipped: a manifest that will not parse
            // takes the project's `[lib]` entries and dependencies with it, and
            // every test then fails on an import instead of on the real cause.
            let manifest = crate::project::load_project(root).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
            if let Err(e) = crate::pkg::ensure_ready(root, &manifest) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            Some(crate::pkg::resolved_libraries(root, &manifest))
        })
        .unwrap_or_default();

    let opts = vm::VmOpts {
        backend,
        source_dir,
        project_root,
        libraries,
        #[cfg(test)]
        test_stdout: None,
        #[cfg(test)]
        max_call_depth: None,
    };

    vm::run(compiled, opts).await.map_err(|e| e.to_string())?;
    Ok(())
}
