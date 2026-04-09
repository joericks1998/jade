mod cli;
mod config;
mod interpreter;
mod llm;

use std::{env, process};

fn main() {
    let args: Vec<String> = env::args().collect();

    match args.as_slice() {
        [_] => {
            eprintln!("Usage: jade <file.jde>");
            eprintln!("       jade configure");
            eprintln!("       jade --help");
            process::exit(1);
        }
        [_, flag] if flag == "-h" || flag == "--help" => {
            cli::help::print_help();
        }
        [_, cmd] if cmd == "configure" => {
            cli::configure::run_configure();
        }
        [_, filename] => {
            cli::run::run_file(filename, false);
        }
        [_, filename, flag] if flag == "-v" || flag == "--verbose" => {
            cli::run::run_file(filename, true);
        }
        _ => {
            eprintln!("error: unexpected arguments");
            eprintln!("Usage: jade <file.jde>");
            process::exit(1);
        }
    }
}
