use std::{fs, process};

use crate::interpreter::{eval, lexer, parser};

pub fn run_file(path: &str, verbose: bool) {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read '{}': {}", path, e);
            process::exit(1);
        }
    };

    let tokens = match lexer::tokenize(&source) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}: lexer error: {}", path, e);
            process::exit(1);
        }
    };

    let program = match parser::parse(tokens) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}: parse error: {}", path, e);
            process::exit(1);
        }
    };

    let env: eval::Env = match eval::evaluate(program) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{}: runtime error: {}", path, e);
            process::exit(1);
        }
    };

    if verbose {
        let mut pairs: Vec<(&String, &eval::Value)> = env.entries().collect();
        pairs.sort_by_key(|(name, _)| name.as_str());
        for (name, val) in pairs {
            match val {
                eval::Value::Int(i) => println!("{} = {}", name, i),
                eval::Value::Float(f) => {
                    let s = format!("{}", f);
                    // Append `.0` only when the formatted value looks like a bare integer
                    // (digits and optional leading minus only). This correctly leaves
                    // `inf`, `-inf`, `NaN`, and scientific-notation strings unchanged.
                    if s.chars().all(|c| c.is_ascii_digit() || c == '-') {
                        println!("{} = {}.0", name, s);
                    } else {
                        println!("{} = {}", name, s);
                    }
                }
                eval::Value::Bool(b) => println!("{} = {}", name, b),
                eval::Value::Fn(_)   => println!("{} = <fn>", name),
                eval::Value::Struct(rc) => {
                    let inst = rc.borrow();
                    print!("{} = {} {{", name, inst.type_name);
                    let mut pairs: Vec<_> = inst.fields.iter().collect();
                    pairs.sort_by_key(|(k, _)| k.as_str());
                    let mut first = true;
                    for (k, v) in pairs {
                        if !first { print!(", "); }
                        match v {
                            eval::Value::Int(i)   => print!("{}: {}", k, i),
                            eval::Value::Float(f) => print!("{}: {}", k, f),
                            eval::Value::Bool(b)  => print!("{}: {}", k, b),
                            _                     => print!("{}: ...", k),
                        }
                        first = false;
                    }
                    println!(" }}");
                }
                eval::Value::BoundMethod(_) => println!("{} = <bound method>", name),
            }
        }
    }
}
