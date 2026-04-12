mod cache;
mod cli;
mod compiler;
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
            eprintln!("       jade build <file.jde>");
            eprintln!("       jade --help");
            process::exit(1);
        }
        [_, flag] if flag == "-h" || flag == "--help" => {
            cli::help::print_help();
        }
        [_, cmd] if cmd == "configure" => {
            cli::configure::run_configure();
        }
        [_, cmd, filename] if cmd == "check" => {
            cli::check::run_check(filename);
        }
        // jade build <file.jde>
        [_, cmd, filename] if cmd == "build" => {
            cli::build::run_build(filename, None, false);
        }
        // jade build <file.jde> -o <output>
        [_, cmd, filename, flag, output] if cmd == "build" && (flag == "-o" || flag == "--output") => {
            cli::build::run_build(filename, Some(output), false);
        }
        // jade build <file.jde> --emit=ir
        [_, cmd, filename, flag] if cmd == "build" && flag == "--emit=ir" => {
            cli::build::run_build(filename, None, true);
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
