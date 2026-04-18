use std::io::{self, BufRead, Write};

use crate::interpreter::{
    ast::Stmt,
    error::Span,
    eval::{self, Env, LlmOpts, Value},
};

/// `jade repl [-v]`
///
/// Uses the tree-walk evaluator so that definitions from one line persist into
/// subsequent ones without needing type-inference state to be threaded through.
pub async fn run_repl(_verbose: bool) {
    let cfg = crate::config::load_config();
    let backend = cfg.api_key.as_ref()
        .and_then(|key| crate::llm::build_backend(&cfg.provider, key, &cfg.model, cfg.max_parallel).ok());

    let opts = LlmOpts {
        backend,
        default_model: cfg.model.clone(),
        max_retries: cfg.max_retries,
    };

    // Seed the live Env that persists across all REPL inputs.
    let mut env = Env::new();
    env.inference_backend = opts.backend;
    env.max_retries = opts.max_retries;
    env.default_model = opts.default_model.clone();
    // Fix 3: populate session vars so Jade code can read __model__ and __max_retries__.
    env.set_session_var("__model__", Value::Str(opts.default_model));
    env.set_session_var("__max_retries__", Value::Int(opts.max_retries as i64));

    let version = env!("CARGO_PKG_VERSION");
    println!("jade {} repl — type 'exit' or press Ctrl+D to quit", version);
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    // Fix 2: acquire the stdin lock once before the outer loop so it is held
    // for the entire REPL session rather than re-acquired on every line read.
    let mut stdin_lock = stdin.lock();

    loop {
        print!("jade> ");
        stdout.flush().unwrap_or(());

        let mut input = String::new();
        let mut open_braces: i32 = 0;

        loop {
            let mut line = String::new();
            match stdin_lock.read_line(&mut line) {
                Ok(0) => {
                    // EOF (Ctrl+D).
                    println!();
                    println!("bye");
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("error: {}", e);
                    return;
                }
            }

            // Fix 1: count braces with a state machine that skips over string
            // literals so that `let s = "hello {"` does not enter continuation
            // mode.
            let mut in_str = false;
            let mut escape = false;
            for ch in line.chars() {
                if escape { escape = false; continue; }
                if ch == '\\' && in_str { escape = true; continue; }
                if ch == '"' { in_str = !in_str; continue; }
                if in_str { continue; }
                match ch {
                    '{' => open_braces += 1,
                    '}' => open_braces -= 1,
                    _ => {}
                }
            }
            input.push_str(&line);

            if open_braces <= 0 {
                break;
            }
            // Continuation prompt.
            print!("  ... ");
            stdout.flush().unwrap_or(());
        }

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            println!("bye");
            return;
        }

        match eval_snippet(trimmed, &mut env) {
            Ok(Some(display)) => println!("{}", display),
            Ok(None) => {}
            Err(msg) => eprintln!("error: {}", msg),
        }
    }
}

/// Evaluate one REPL snippet and return the last expression value for display.
fn eval_snippet(src: &str, env: &mut Env) -> Result<Option<String>, String> {
    // Lex + parse.
    let tokens = crate::interpreter::lexer::tokenize(src)
        .map_err(|e| e.to_string())?;
    let mut program = crate::interpreter::parser::parse(tokens)
        .map_err(|e| e.to_string())?;

    // If the last statement is a bare expression, capture it into a binding so
    // we can retrieve and display the result after execution.
    let capture = if let Some(last) = program.stmts.last() {
        matches!(last, Stmt::Expr(_))
    } else {
        false
    };

    if capture {
        if let Some(Stmt::Expr(expr)) = program.stmts.pop() {
            program.stmts.push(Stmt::Let {
                name: "__repl_result__".to_string(),
                value: expr,
                span: Span { line: 0, col: 0 },
            });
        }
    }

    eval::evaluate_incremental(program, env).map_err(|e| e.to_string())?;

    if capture {
        // Retrieve and immediately remove the temporary binding.
        if let Some(val) = env.globals_mut().remove("__repl_result__") {
            let display = match &val {
                Value::Str(s) => format!("{:?}", s),
                other => eval::value_to_str(other),
            };
            return Ok(Some(display));
        }
    }

    Ok(None)
}
