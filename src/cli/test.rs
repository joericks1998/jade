use std::{path::PathBuf, process};

/// `jade test [pattern] [-v]`
pub fn run_test(pattern: Option<&str>, verbose: bool) {
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
        let name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();

        let path_str = file.to_string_lossy().to_string();

        print!("  {} ... ", name);

        match run_test_file(&path_str, verbose) {
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
fn run_test_file(path: &str, _verbose: bool) -> Result<(), String> {
    use std::path::Path;
    use crate::compiler::{emit, type_infer, vm};

    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read file: {}", e))?;

    // Lex + parse (with cache).
    let hash = crate::cache::file_hash(Path::new(path));
    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
    let program = match cached_ast {
        Some(p) => p,
        None => {
            let tokens = crate::interpreter::lexer::tokenize(&source)
                .map_err(|e| format!("lexer error: {}", e))?;
            let p = crate::interpreter::parser::parse(tokens)
                .map_err(|e| format!("parse error: {}", e))?;
            if let Some(ref h) = hash {
                crate::cache::write_ast_cache(h, path, &p);
            }
            p
        }
    };

    // Type inference.
    let tprogram = if let Some(ref h) = hash {
        match crate::cache::read_tir_cache(h) {
            Some(tp) => tp,
            None => {
                let tp = type_infer::infer(program)
                    .map_err(|e| format!("type error: {}", e))?;
                crate::cache::write_tir_cache(h, path, &tp);
                tp
            }
        }
    } else {
        type_infer::infer(program).map_err(|e| format!("type error: {}", e))?
    };

    // Emit + run.
    let compiled = emit::emit(tprogram)
        .map_err(|e| format!("compile error: {}", e))?;

    let cfg = crate::config::load_config();
    let backend = cfg.api_key.as_ref()
        .map(|key| crate::llm::build_backend(&cfg.provider, key, &cfg.model))
        .transpose()
        .map_err(|e| format!("config error: {}", e))?;

    let source_dir = Path::new(path)
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let opts = vm::VmOpts {
        backend,
        default_model: cfg.model,
        max_retries: cfg.max_retries,
        source_dir,
    };

    vm::run(compiled, opts).map_err(|e| e.to_string())?;
    Ok(())
}
