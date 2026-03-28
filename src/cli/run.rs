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
                    if s.contains('.') || s.contains('e') {
                        println!("{} = {}", name, s);
                    } else {
                        println!("{} = {}.0", name, s);
                    }
                }
                eval::Value::Bool(b) => println!("{} = {}", name, b),
                eval::Value::Fn(_)   => println!("{} = <fn>", name),
            }
        }
    }
}
