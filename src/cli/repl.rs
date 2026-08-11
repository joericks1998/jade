use std::io::{self, BufRead, Write};

use crate::{
    compiler::{emit, type_infer},
    frontend::{
        ast::{Expr, Stmt},
        error::Span,
        lexer, parser,
    },
    vm::{self, VmOpts, VmState, value_to_display},
};

/// `jade repl [-v]`
///
/// Uses the VM backend so that `jade repl` and `jade run` share the same
/// execution semantics. Globals persist across snippets via `VmState`.
pub async fn run_repl(_verbose: bool) {
    let backend = crate::llm::select_backend();

    let opts = VmOpts { backend, ..VmOpts::default() };

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
                if escape {
                    escape = false;
                    continue;
                }
                if ch == '\\' && in_str {
                    escape = true;
                    continue;
                }
                if ch == '"' {
                    in_str = !in_str;
                    continue;
                }
                if in_str {
                    continue;
                }
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

    // Some expressions print their own output as they evaluate — a bare `?p`
    // (streams tokens live) and `stream(...)` (prints as it generates). Don't
    // echo their result on top of what they already wrote.
    let suppress_echo = capture
        && matches!(
            program.stmts.last(),
            Some(Stmt::Expr(e)) if prints_own_output(e)
        );

    if capture && let Some(Stmt::Expr(expr)) = program.stmts.pop() {
        program.stmts.push(Stmt::Let {
            name: vm::REPL_CAPTURE.to_string(),
            value: expr,
            span: Span { line: 0, col: 0 },
        });
    }

    // Pre-seed the type context with globals from previous REPL runs so that
    // cross-snippet references resolve. Unknown types are fine — the VM will
    // catch real type mismatches at runtime.
    let known: Vec<String> = state.globals.keys().cloned().collect();
    let tprogram = type_infer::infer_with_globals(program, &known).map_err(|e| e.to_string())?;
    let compiled = emit::emit(tprogram).map_err(|e| e.to_string())?;

    vm::run_incremental(compiled, state).await.map_err(|e| e.to_string())?;

    let captured = state.repl_capture.take();
    if capture
        && !suppress_echo
        && let Some(val) = captured
    {
        // Don't echo a void result — e.g. `print(...)` returns nil, and
        // echoing "nil" after its output is noise.
        if !matches!(val, vm::VmValue::Nil) {
            // Echo strings quoted (REPL convention), but Debug the *contents* —
            // `{:?}` on the JStr itself would print its struct form
            // (`JStr { text: …, trust: 0 }`) into user output.
            let display = match &val {
                vm::VmValue::Str(s) => format!("{:?}", s.as_str()),
                other => value_to_display(other),
            };
            return Ok(Some(display));
        }
    }

    Ok(None)
}

/// Whether an expression prints to stdout on its own as it evaluates, so the
/// REPL should not also echo its result: a bare `?p` (streams tokens live) or a
/// a dereference behind a `|>` stage (`?p |> g` still streams).
pub(crate) fn prints_own_output(expr: &Expr) -> bool {
    match expr {
        Expr::PromptDeref { .. } => true,
        // A grammar stage constrains generation and leaves the value a stream,
        // so it still prints as it goes. A *type* stage collapses it, so the
        // result is an ordinary value the REPL should echo.
        Expr::Pipe { value, .. } => prints_own_output(value),
        _ => false,
    }
}
