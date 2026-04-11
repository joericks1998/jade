use std::{fs, path::Path, process};

use crate::compiler::{emit, type_infer, vm};

pub fn run_file(path: &str, verbose: bool) {
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
            let tokens = match crate::interpreter::lexer::tokenize(&source) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{}: lexer error: {}", path, e);
                    process::exit(1);
                }
            };
            let p = match crate::interpreter::parser::parse(tokens) {
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
    let backend = cfg.api_key.as_ref()
        .map(|key| crate::llm::build_backend(&cfg.provider, key, &cfg.model))
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid configuration: {}", e);
            process::exit(1);
        });
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
    let state = match vm::run(compiled, opts) {
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
                vm::VmValue::Fn(_)    => println!("{} = <fn>", name),
                vm::VmValue::Struct(rc) => {
                    let inst = rc.borrow();
                    print!("{} = {} {{", name, inst.type_name);
                    let mut fields: Vec<_> = inst.fields.iter().collect();
                    fields.sort_by_key(|(k, _)| k.as_str());
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
                vm::VmValue::Array(vec) => {
                    let parts: Vec<String> = vec.iter().map(vm::value_to_display).collect();
                    println!("{} = [{}]", name, parts.join(", "));
                }
                vm::VmValue::BoundMethod(_) => println!("{} = <bound method>", name),
                vm::VmValue::Prompt(_)      => println!("{} = <prompt>", name),
                vm::VmValue::Dict(_) => println!("{} = {}", name, vm::value_to_display(val)),
                vm::VmValue::Nil    => {} // not shown
            }
        }
    }
}
