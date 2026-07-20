use std::io::{self, BufRead, Write};

use crate::{
    compiler::{emit, type_infer}, vm::{self, VmOpts, VmState, value_to_display},
    frontend::{
        ast::{Expr, Stmt},
        error::Span,
        lexer, parser,
    },
};

/// `jade repl [-v]`
///
/// Uses the VM backend so that `jade repl` and `jade run` share the same
/// execution semantics. Globals persist across snippets via `VmState`.
pub async fn run_repl(_verbose: bool) {
    let cfg = crate::config::load_config();
    let backend = crate::llm::select_backend(&cfg);

    let opts = VmOpts {
        backend,
        ..VmOpts::default()
    };

    let mut state = VmState::new_for_repl(opts);

    let version = env!("CARGO_PKG_VERSION");
    println!("jade {} repl — type 'exit' or press Ctrl+D to quit", version);
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
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

            // Count braces, skipping string literals so `"hello {"` doesn't
            // trigger continuation mode.
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

        match eval_snippet_vm(trimmed, &mut state).await {
            Ok(Some(display)) => println!("{}", display),
            Ok(None) => {}
            Err(msg) => eprintln!("error: {}", msg),
        }
    }
}

/// Compile and execute one REPL snippet against the shared `VmState`.
///
/// Returns the display string for the last bare expression, if any.
/// Type inference errors are non-fatal — the user is shown the error and can
/// continue the session.
async fn eval_snippet_vm(src: &str, state: &mut VmState) -> Result<Option<String>, String> {
    let tokens = lexer::tokenize(src).map_err(|e| e.to_string())?;
    let mut program = parser::parse(tokens).map_err(|e| e.to_string())?;

    // Detect a bare expression as the last statement.
    let capture = matches!(program.stmts.last(), Some(Stmt::Expr(_)));

    // PromptDeref (`?p`) streams tokens live to stdout; suppress echoing the
    // result string since the output already appeared during inference.
    let is_prompt_deref = capture && matches!(
        program.stmts.last(),
        Some(Stmt::Expr(Expr::PromptDeref { .. }))
    );

    if capture {
        if let Some(Stmt::Expr(expr)) = program.stmts.pop() {
            program.stmts.push(Stmt::Let {
                name: "__repl_result__".to_string(),
                value: expr,
                span: Span { line: 0, col: 0 },
            });
        }
    }

    // Pre-seed the type context with globals from previous REPL runs so that
    // cross-snippet references resolve. Unknown types are fine — the VM will
    // catch real type mismatches at runtime.
    let known: Vec<String> = state.globals.keys().cloned().collect();
    let tprogram = type_infer::infer_with_globals(program, &known)
        .map_err(|e| e.to_string())?;
    let compiled = emit::emit(tprogram).map_err(|e| e.to_string())?;

    vm::run_incremental(compiled, state).await.map_err(|e| e.to_string())?;

    if capture && !is_prompt_deref {
        if let Some(val) = state.globals.remove("__repl_result__") {
            let display = match &val {
                vm::VmValue::Str(s) => format!("{:?}", s),
                other => value_to_display(other),
            };
            return Ok(Some(display));
        }
    } else if capture {
        state.globals.remove("__repl_result__");
    }

    Ok(None)
}
