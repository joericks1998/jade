use std::{fs, path::Path, process};

use crate::compiler::{emit, type_infer, vm};

// ── Project-aware run ─────────────────────────────────────────────────────────

/// `jade run` with no argument: find project root and run its entry file.
pub async fn run_entry(verbose: bool) {
    let root = crate::project::find_project_root().unwrap_or_else(|| {
        // Fall back to running main.jde in CWD.
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    let manifest = match crate::project::load_project(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("warning: could not read jade.toml: {}", e);
            crate::project::ProjectManifest::default()
        }
    };
    let entry = root.join(manifest.entry_file());

    if !entry.exists() {
        eprintln!("error: entry file '{}' not found", entry.display());
        eprintln!("       run 'jade new <name>' to create a project, or 'jade run <file.jde>'");
        process::exit(1);
    }

    run_file(&entry.to_string_lossy(), verbose).await;
}

/// `jade run <script>`: run a named script from [scripts] in jade.toml.
pub fn run_script(name: &str) {
    let root = crate::project::find_project_root().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    let manifest = match crate::project::load_project(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    };

    let scripts = manifest.scripts.unwrap_or_default();
    let cmd = scripts.get(name).cloned().unwrap_or_else(|| {
        eprintln!("error: no script named '{}' in jade.toml", name);
        eprintln!("       available scripts: {}", scripts.keys().cloned().collect::<Vec<_>>().join(", "));
        process::exit(1);
    });

    // Execute via shell.
    #[cfg(unix)]
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status();
    #[cfg(windows)]
    let status = std::process::Command::new("cmd")
        .args(["/C", &cmd])
        .status();

    match status {
        Ok(s) => {
            if !s.success() {
                process::exit(s.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("error: could not run script: {}", e);
            process::exit(1);
        }
    }
}

pub async fn run_file(path: &str, verbose: bool) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read '{}': {}", path, e);
            process::exit(1);
        }
    };

    let hash = crate::cache::file_hash(Path::new(path));

    // L1 cache: try to skip lex + parse.
    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
    let program = match cached_ast {
        Some(p) => p,
        None => {
            let tokens = match crate::frontend::lexer::tokenize(&source) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{}: lexer error: {}", path, e);
                    process::exit(1);
                }
            };
            let p = match crate::frontend::parser::parse(tokens) {
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

    // L2 cache: try to skip type inference.
    let tprogram = if let Some(ref h) = hash {
        match crate::cache::read_tir_cache(h) {
            Some(tp) => tp,
            None => {
                let tp = match type_infer::infer(program) {
                    Ok(tp) => tp,
                    Err(e) => { eprintln!("{}: {}", path, e); process::exit(1); }
                };
                crate::cache::write_tir_cache(h, path, &tp);
                tp
            }
        }
    } else {
        match type_infer::infer(program) {
            Ok(tp) => tp,
            Err(e) => { eprintln!("{}: {}", path, e); process::exit(1); }
        }
    };

    // Emit bytecode.
    let compiled = match emit::emit(tprogram) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: compile error: {}", path, e);
            process::exit(1);
        }
    };

    // Build LLM config and backend.
    let cfg = crate::config::load_config();
    let backend = crate::llm::select_backend(&cfg);
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

    // Execute.
    let state = match vm::run(compiled, opts).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: runtime error: {}", path, e);
            process::exit(1);
        }
    };

    if verbose {
        let mut pairs: Vec<(&String, &vm::VmValue)> = state.global_entries().collect();
        pairs.sort_by_key(|(name, _)| name.as_str());
        for (name, val) in pairs {
            match val {
                vm::VmValue::Int(i)   => println!("{} = {}", name, i),
                vm::VmValue::Float(f) => {
                    let s = format!("{}", f);
                    if s.chars().all(|c| c.is_ascii_digit() || c == '-') {
                        println!("{} = {}.0", name, s);
                    } else {
                        println!("{} = {}", name, s);
                    }
                }
                vm::VmValue::Bool(b)  => println!("{} = {}", name, b),
                vm::VmValue::Str(s)   => println!("{} = \"{}\"", name, s),
                vm::VmValue::Fn(_) | vm::VmValue::Closure(_, _) => println!("{} = <fn>", name),
                vm::VmValue::Struct(rc) => {
                    let inst = rc.lock();
                    print!("{} = {} {{", name, inst.type_name);
                    let mut fields: Vec<_> = inst.fields.iter().collect();
                    fields.sort_by_key(|(k, _): &(&String, _)| k.as_str());
                    let mut first = true;
                    for (k, v) in fields {
                        if !first { print!(", "); }
                        match v {
                            vm::VmValue::Int(i)   => print!("{}: {}", k, i),
                            vm::VmValue::Float(f) => print!("{}: {}", k, f),
                            vm::VmValue::Bool(b)  => print!("{}: {}", k, b),
                            vm::VmValue::Str(s)   => print!("{}: \"{}\"", k, s),
                            _                     => print!("{}: ...", k),
                        }
                        first = false;
                    }
                    println!(" }}");
                }
                vm::VmValue::Array(arc) => {
                    let guard = arc.lock();
                    let parts: Vec<String> = guard.iter().map(vm::value_to_display).collect();
                    println!("{} = [{}]", name, parts.join(", "));
                }
                vm::VmValue::BoundMethod(_) => println!("{} = <bound method>", name),
                vm::VmValue::Prompt(_)      => println!("{} = <prompt>", name),
                vm::VmValue::Dict(_) => println!("{} = {}", name, vm::value_to_display(val)),
                vm::VmValue::NativeFn(_) | vm::VmValue::BuiltinFn(_) | vm::VmValue::NativeBoundMethod(_) => {} // not shown
                vm::VmValue::Future(_)      => println!("{} = <future>", name),
                vm::VmValue::TokenStream(_) => println!("{} = <token stream>", name),
                vm::VmValue::Grammar(_)     => {} // not shown
                vm::VmValue::TypeRef(_)     => {} // not shown
                vm::VmValue::Nil            => {} // not shown
            }
        }
    }
}
