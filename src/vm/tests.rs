//! Unit tests for the bytecode VM.
//!
//! Moved here from `compiler/tests.rs` when the VM became its own module:
//! it is an execution engine, not a compiler phase.
#![allow(clippy::all)]

use crate::frontend::error::{JadeError, Result};
use crate::vm::*;
use crate::{
    compiler::{emit, type_infer},
    frontend::{lexer, parser},
};

fn run_src(src: &str) -> Result<VmState> {
    let tokens = lexer::tokenize(src).expect("lex failed");
    let program = parser::parse(tokens).expect("parse failed");
    let tprogram = type_infer::infer(program).expect("type inference failed");
    let compiled = emit::emit(tprogram).expect("emit failed");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run(compiled, VmOpts::default()))
}

fn get_int(state: &VmState, name: &str) -> i64 {
    match state.globals.get(name).expect("var not found") {
        VmValue::Int(i) => *i,
        v => panic!("expected Int, got {:?}", v),
    }
}

fn get_float(state: &VmState, name: &str) -> f64 {
    match state.globals.get(name).expect("var not found") {
        VmValue::Float(f) => *f,
        v => panic!("expected Float, got {:?}", v),
    }
}

fn get_bool(state: &VmState, name: &str) -> bool {
    match state.globals.get(name).expect("var not found") {
        VmValue::Bool(b) => *b,
        v => panic!("expected Bool, got {:?}", v),
    }
}

fn get_char(state: &VmState, name: &str) -> char {
    match state.globals.get(name).expect("var not found") {
        VmValue::Char(c) => c.ch(),
        v => panic!("expected Char, got {v:?}"),
    }
}

fn get_str<'a>(state: &'a VmState, name: &str) -> &'a str {
    match state.globals.get(name).expect("var not found") {
        VmValue::Str(s) => s,
        v => panic!("expected Str, got {:?}", v),
    }
}

#[test]
fn test_vm_int_literal() {
    let s = run_src("let x = 42").unwrap();
    assert_eq!(get_int(&s, "x"), 42);
}

#[test]
fn test_vm_float_literal() {
    let s = run_src("let x = 3.14").unwrap();
    assert!((get_float(&s, "x") - 3.14).abs() < 1e-10);
}

#[test]
fn test_vm_bool_literal() {
    let s = run_src("let x = true").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_str_literal() {
    let s = run_src("let x = \"hello\"").unwrap();
    assert_eq!(get_str(&s, "x"), "hello");
}

#[test]
fn test_vm_add_int() {
    let s = run_src("let x = 3 + 4").unwrap();
    assert_eq!(get_int(&s, "x"), 7);
}

#[test]
fn test_vm_add_float() {
    let s = run_src("let x = 1.5 + 2.5").unwrap();
    assert!((get_float(&s, "x") - 4.0).abs() < 1e-10);
}

#[test]
fn test_vm_add_int_float() {
    let s = run_src("let x = 1 + 2.5").unwrap();
    assert!((get_float(&s, "x") - 3.5).abs() < 1e-10);
}

#[test]
fn test_vm_sub_mul_div() {
    let s = run_src("let x = 10 - 3\nlet y = 4 * 5\nlet z = 10 / 2").unwrap();
    assert_eq!(get_int(&s, "x"), 7);
    assert_eq!(get_int(&s, "y"), 20);
    assert_eq!(get_int(&s, "z"), 5);
}

#[test]
fn test_vm_mod() {
    let s = run_src("let x = 10 % 3").unwrap();
    assert_eq!(get_int(&s, "x"), 1);
}

#[test]
fn test_vm_comparison() {
    let s = run_src("let a = 3 < 5\nlet b = 5 > 3\nlet c = 3 == 3\nlet d = 3 != 4").unwrap();
    assert!(get_bool(&s, "a"));
    assert!(get_bool(&s, "b"));
    assert!(get_bool(&s, "c"));
    assert!(get_bool(&s, "d"));
}

#[test]
fn test_vm_logical_and_or() {
    let s = run_src("let a = true && false\nlet b = false || true").unwrap();
    assert!(!get_bool(&s, "a"));
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_short_circuit_and() {
    let s = run_src("let a = false && true").unwrap();
    assert!(!get_bool(&s, "a"));
}

#[test]
fn test_vm_short_circuit_or() {
    let s = run_src("let a = true || false").unwrap();
    assert!(get_bool(&s, "a"));
}

#[test]
fn test_vm_if_true() {
    let s = run_src("let x = 0\nif true {\n  x = 1\n}").unwrap();
    assert_eq!(get_int(&s, "x"), 1);
}

#[test]
fn test_vm_if_false() {
    let s = run_src("let x = 0\nif false {\n  x = 1\n}").unwrap();
    assert_eq!(get_int(&s, "x"), 0);
}

#[test]
fn test_vm_if_else() {
    let s = run_src("let x = 0\nif false {\n  x = 1\n} else {\n  x = 2\n}").unwrap();
    assert_eq!(get_int(&s, "x"), 2);
}

#[test]
fn test_vm_while_loop() {
    let s =
        run_src("let i = 0\nlet sum = 0\nwhile i < 5 {\n  sum = sum + i\n  i = i + 1\n}").unwrap();
    assert_eq!(get_int(&s, "sum"), 10);
}

#[test]
fn test_vm_function_call() {
    let s = run_src("fn add(a, b) {\n  return a + b\n}\nlet x = add(3, 4)").unwrap();
    assert_eq!(get_int(&s, "x"), 7);
}

#[test]
fn test_vm_recursive_fn() {
    let s = run_src("fn fact(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * fact(n - 1)\n}\nlet x = fact(5)").unwrap();
    assert_eq!(get_int(&s, "x"), 120);
}

#[test]
fn test_vm_array_literal() {
    let s = run_src("let a = [1, 2, 3]\nlet x = a[1]").unwrap();
    assert_eq!(get_int(&s, "x"), 2);
}

#[test]
fn test_vm_array_assign() {
    let s = run_src("let a = [1, 2, 3]\na[0] = 10\nlet x = a[0]").unwrap();
    assert_eq!(get_int(&s, "x"), 10);
}

#[test]
fn test_vm_str_concat() {
    let s = run_src("let a = \"hello\"\nlet b = \" world\"\nlet c = a + b").unwrap();
    assert_eq!(get_str(&s, "c"), "hello world");
}

#[test]
fn test_vm_fstr() {
    let s = run_src("let name = \"jade\"\nlet x = f\"hello, {name}!\"").unwrap();
    assert_eq!(get_str(&s, "x"), "hello, jade!");
}

#[test]
fn test_vm_bitwise() {
    let s = run_src("let a = 5 & 3\nlet b = 5 | 3\nlet c = 5 ^ 3").unwrap();
    assert_eq!(get_int(&s, "a"), 1);
    assert_eq!(get_int(&s, "b"), 7);
    assert_eq!(get_int(&s, "c"), 6);
}

#[test]
fn test_vm_unary_neg() {
    let s = run_src("let x = -5\nlet y = -3.14").unwrap();
    assert_eq!(get_int(&s, "x"), -5);
    assert!((get_float(&s, "y") - (-3.14)).abs() < 1e-10);
}

#[test]
fn test_vm_struct() {
    let s = run_src("struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 10, y: 20 }\nlet px = p.x")
        .unwrap();
    assert_eq!(get_int(&s, "px"), 10);
}

#[test]
fn test_vm_struct_field_assign() {
    let s = run_src(
        "struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 1, y: 2 }\np.x = 99\nlet px = p.x",
    )
    .unwrap();
    assert_eq!(get_int(&s, "px"), 99);
}

#[test]
fn test_vm_dict() {
    let s = run_src("let d = {\"key\": 42}\nlet x = d[\"key\"]").unwrap();
    assert_eq!(get_int(&s, "x"), 42);
}

#[test]
fn test_vm_len_array() {
    let s = run_src("let a = [1, 2, 3]\nlet n = len(a)").unwrap();
    assert_eq!(get_int(&s, "n"), 3);
}

#[test]
fn test_vm_len_str() {
    let s = run_src("let s = \"hello\"\nlet n = len(s)").unwrap();
    assert_eq!(get_int(&s, "n"), 5);
}

#[test]
fn test_vm_extend_method() {
    let src = "struct Counter {\n  val,\n}\nextend Counter {\n  fn inc(self) {\n    self.val = self.val + 1\n  }\n}\nlet c = Counter { val: 0 }\nc.inc()\nlet v = c.val";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 1);
}

#[test]
fn test_vm_prompt_decl() {
    let s = run_src("prompt p = \"hello\"").unwrap();
    match s.globals.get("p").unwrap() {
        VmValue::Prompt(t) => assert_eq!(t, "hello"),
        v => panic!("expected Prompt, got {:?}", v),
    }
}

#[test]
fn test_vm_div_by_zero() {
    let res = run_src("let x = 1 / 0");
    assert!(res.is_err());
}

// ── Implicit return tests ─────────────────────────────────────────────────

/// A function whose body is a single bare expression returns that value.
#[test]
fn test_vm_implicit_return_bare_expr() {
    let s = run_src("fn answer() {\n  42\n}\nlet x = answer()").unwrap();
    assert_eq!(get_int(&s, "x"), 42);
}

/// A function with let bindings followed by a bare expression returns the
/// expression value, not nil.
#[test]
fn test_vm_implicit_return_after_let() {
    let s =
        run_src("fn double(n) {\n  let result = n * 2\n  result\n}\nlet x = double(5)").unwrap();
    assert_eq!(get_int(&s, "x"), 10);
}

/// A function ending with an explicit `return` still works correctly; the
/// emitter must not append a second `Return(None)` instruction after it.
#[test]
fn test_vm_explicit_return_no_dead_instruction() {
    let s = run_src("fn add(a, b) {\n  return a + b\n}\nlet x = add(3, 4)").unwrap();
    assert_eq!(get_int(&s, "x"), 7);
}

/// A function with an empty body falls off the end and returns nil.
#[test]
fn test_vm_empty_body_returns_nil() {
    let s = run_src("fn noop() {}\nlet x = noop()").unwrap();
    match s.globals.get("x").unwrap() {
        VmValue::Nil => {}
        v => panic!("expected Nil, got {:?}", v),
    }
}

// ── helpers for ported eval.rs tests ─────────────────────────────────────

/// Like `run_src` but propagates errors from every stage (lex, parse,
/// type_infer, emit, vm) so error-path tests return `Err` rather than
/// panicking at the `expect` call.
fn try_run_src(src: &str) -> Result<VmState> {
    let tokens = lexer::tokenize(src)?;
    let program = parser::parse(tokens)?;
    let tprogram = type_infer::infer(program)?;
    let compiled = emit::emit(tprogram)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run(compiled, VmOpts::default()))
}

fn run_src_with_mock(src: &str, responses: Vec<&str>) -> Result<VmState> {
    run_src_with_mock_inner(src, responses, None)
}

/// Like `run_src_with_mock` but also returns a string of everything written to
/// stdout by `vm_drain_token_stream_printing` (i.e. printing a stream).
fn run_src_with_stdout_capture(src: &str, responses: Vec<&str>) -> Result<(VmState, String)> {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let state = run_src_with_mock_inner(src, responses, Some(std::sync::Arc::clone(&buf)))?;
    let printed = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    Ok((state, printed))
}

fn run_src_with_mock_inner(
    src: &str,
    responses: Vec<&str>,
    test_stdout: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Result<VmState> {
    let tokens = lexer::tokenize(src).expect("lex failed");
    let program = parser::parse(tokens).expect("parse failed");
    let tprogram = type_infer::infer(program).expect("type inference failed");
    let compiled = emit::emit(tprogram).expect("emit failed");
    let opts = VmOpts {
        backend: Some(std::sync::Arc::new(crate::llm::MockBackend::new(responses))),
        test_stdout,
        ..VmOpts::default()
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run(compiled, opts))
}

// ── REPL incremental state ────────────────────────────────────────────────

#[test]
fn test_vm_repl_state_persists() {
    // Tests that run_incremental preserves globals across two separate runs.
    // Each snippet is compiled independently (no cross-snippet references)
    // because the type inferrer is stateless — cross-snippet variable
    // references require a stateful type inferrer (future work).
    use crate::compiler::{emit, type_infer};
    fn repl_run(src: &str, state: &mut VmState) {
        let tokens = lexer::tokenize(src).expect("lex");
        let program = parser::parse(tokens).expect("parse");
        let tprogram = type_infer::infer(program).expect("infer");
        let compiled = emit::emit(tprogram).expect("emit");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio")
            .block_on(run_incremental(compiled, state))
            .expect("run_incremental");
    }
    let mut state = VmState::new_for_repl(VmOpts::default());
    repl_run("let x = 42", &mut state);
    repl_run("let y = 100", &mut state);
    // Both globals must be present after two independent incremental runs
    match state.globals.get("x").unwrap() {
        VmValue::Int(42) => {}
        v => panic!("expected Int(42), got {:?}", v),
    }
    match state.globals.get("y").unwrap() {
        VmValue::Int(100) => {}
        v => panic!("expected Int(100), got {:?}", v),
    }
}

#[test]
fn test_vm_repl_result_capture_and_remove() {
    use crate::{
        compiler::{emit, type_infer},
        frontend::ast::Stmt,
        frontend::error::Span,
    };
    let src = "1 + 1";
    let tokens = lexer::tokenize(src).expect("lex");
    let mut program = parser::parse(tokens).expect("parse");
    // Wrap the bare trailing expression the way the REPL does, so its value is
    // routed into `repl_capture` (out-of-band, never a global).
    if let Some(Stmt::Expr(expr)) = program.stmts.pop() {
        program.stmts.push(Stmt::Let {
            name: crate::vm::REPL_CAPTURE.to_string(),
            value: expr,
            span: Span { line: 0, col: 0 },
        });
    }
    let tprogram = type_infer::infer(program).expect("infer");
    let compiled = emit::emit(tprogram).expect("emit");
    let mut state = VmState::new_for_repl(VmOpts::default());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio")
        .block_on(run_incremental(compiled, &mut state))
        .expect("run_incremental");
    // Captured out-of-band, and never leaked into the global namespace.
    assert!(matches!(state.repl_capture, Some(VmValue::Int(2))));
    assert!(state.globals.get(crate::vm::REPL_CAPTURE).is_none());
}

// ── arithmetic (ported from eval.rs) ─────────────────────────────────────

#[test]
fn test_vm_div_float() {
    let s = run_src("let x = 5.0 / 2.0").unwrap();
    assert!((get_float(&s, "x") - 2.5).abs() < 1e-10);
}

#[test]
fn test_vm_mod_float() {
    let s = run_src("let x = 5.0 % 2.0").unwrap();
    assert!((get_float(&s, "x") - 1.0).abs() < 1e-10);
}

#[test]
fn test_vm_mul_promotes_to_float() {
    let s = run_src("let x = 2 * 1.5").unwrap();
    assert!((get_float(&s, "x") - 3.0).abs() < 1e-10);
}

#[test]
fn test_vm_shl() {
    let s = run_src("let x = 1 << 3").unwrap();
    assert_eq!(get_int(&s, "x"), 8);
}

#[test]
fn test_vm_shr() {
    let s = run_src("let x = 16 >> 2").unwrap();
    assert_eq!(get_int(&s, "x"), 4);
}

#[test]
fn test_vm_bitnot_zero() {
    let s = run_src("let x = ~0").unwrap();
    assert_eq!(get_int(&s, "x"), -1);
}

#[test]
fn test_vm_neg_paren_ok() {
    let s = run_src("let x = -(3 + 4)").unwrap();
    assert_eq!(get_int(&s, "x"), -7);
}

// ── error conditions (ported from eval.rs) ────────────────────────────────

#[test]
fn test_vm_div_by_zero_float() {
    assert!(try_run_src("let x = 5.0 / 0.0").is_err());
}

#[test]
fn test_vm_remainder_by_zero_int() {
    let err = try_run_src("let x = 5 % 0").err().expect("expected error");
    assert!(matches!(err, JadeError::RemainderByZero { .. }));
}

#[test]
fn test_vm_remainder_by_zero_float() {
    assert!(try_run_src("let x = 5.0 % 0.0").is_err());
}

#[test]
fn test_vm_invalid_shift_too_large() {
    let err = try_run_src("let x = 1 << 64").err().expect("expected error");
    assert!(matches!(err, JadeError::InvalidShift { amount: 64, .. }));
}

#[test]
fn test_vm_invalid_shift_negative() {
    let err = try_run_src("let x = 1 >> -1").err().expect("expected error");
    assert!(matches!(err, JadeError::InvalidShift { amount: -1, .. }));
}

#[test]
fn test_vm_type_error_bitand_float() {
    assert!(try_run_src("let x = 1.0 & 2.0").is_err());
}

#[test]
fn test_vm_type_error_bitnot_float() {
    assert!(try_run_src("let x = ~1.0").is_err());
}

#[test]
fn test_vm_type_error_neg_bool() {
    assert!(try_run_src("let x = -true").is_err());
}

#[test]
fn test_vm_type_error_add_bool() {
    assert!(try_run_src("let x = true + 1").is_err());
}

#[test]
fn test_vm_undefined_variable() {
    assert!(try_run_src("let x = y").is_err());
}

#[test]
fn test_vm_variable_chain() {
    let s = run_src("let add = 1 + 1\nlet result = add * 2").unwrap();
    assert_eq!(get_int(&s, "add"), 2);
    assert_eq!(get_int(&s, "result"), 4);
}

// ── boolean / logical ops (ported from eval.rs) ───────────────────────────

#[test]
fn test_vm_not_true() {
    let s = run_src("let x = !true").unwrap();
    assert!(!get_bool(&s, "x"));
}

#[test]
fn test_vm_not_false() {
    let s = run_src("let x = !false").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_double_not() {
    let s = run_src("let x = !!true").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_type_error_and_on_int() {
    assert!(try_run_src("let x = 1 && 0").is_err());
}

#[test]
fn test_vm_type_error_not_on_int() {
    assert!(try_run_src("let x = !1").is_err());
}

// ── comparison (ported from eval.rs) ─────────────────────────────────────

#[test]
fn test_vm_bool_lt_false_true() {
    let s = run_src("let x = false < true").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_bool_gt_true_false() {
    let s = run_src("let x = true > false").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_bool_eq() {
    let s = run_src("let x = true == true").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_eq_mixed_type_error() {
    assert!(try_run_src("let x = 1 == 1.0").is_err());
}

#[test]
fn test_vm_type_error_lt_bool_int() {
    assert!(try_run_src("let x = true < 1").is_err());
}

#[test]
fn test_vm_compare_chain() {
    let s = run_src("let x = 1 < 2 && 3 > 0").unwrap();
    assert!(get_bool(&s, "x"));
}

#[test]
fn test_vm_float_lt_promotes() {
    let s = run_src("let x = 1 < 2.5").unwrap();
    assert!(get_bool(&s, "x"));
}

// ── functions — scope & first-class (ported from eval.rs) ────────────────

#[test]
fn test_vm_fn_square() {
    let s = run_src("fn square(x) {\n  return x * x\n}\nlet sq = square(5)").unwrap();
    assert_eq!(get_int(&s, "sq"), 25);
}

#[test]
fn test_vm_fn_multiply_three() {
    let s = run_src("fn multiply(a, b, c) {\n  return a * b * c\n}\nlet r = multiply(2, 3, 4)")
        .unwrap();
    assert_eq!(get_int(&s, "r"), 24);
}

#[test]
fn test_vm_fn_chained_calls() {
    let src = "fn add(a, b) {\n  return a + b\n}\nfn square(x) {\n  return x * x\n}\nlet r = add(square(2), square(3))";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "r"), 13);
}

#[test]
fn test_vm_fn_local_let() {
    let s = run_src("fn get_local() {\n  let x = 42\n  return x\n}\nlet a = get_local()").unwrap();
    assert_eq!(get_int(&s, "a"), 42);
}

#[test]
fn test_vm_fn_uses_param() {
    let s = run_src("fn uses_param(x) {\n  return x + 1\n}\nlet b = uses_param(9)").unwrap();
    assert_eq!(get_int(&s, "b"), 10);
}

#[test]
fn test_vm_fn_local_shadow() {
    let s =
        run_src("fn local_shadow(x) {\n  let y = x * 2\n  return y\n}\nlet c = local_shadow(5)")
            .unwrap();
    assert_eq!(get_int(&s, "c"), 10);
}

#[test]
fn test_vm_fn_assign_to_let() {
    let src = "fn double(x) {\n  return x * 2\n}\nlet f = double\nlet a = f(5)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "a"), 10);
}

#[test]
fn test_vm_fn_pass_as_arg() {
    let src = "fn double(x) {\n  return x * 2\n}\nfn apply(f, x) {\n  return f(x)\n}\nlet b = apply(double, 6)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "b"), 12);
}

#[test]
fn test_vm_fn_compose() {
    let src = "fn double(x) {\n  return x * 2\n}\nfn compose(f, g, x) {\n  return f(g(x))\n}\nlet d = compose(double, double, 3)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "d"), 12);
}

// ── factorial / fibonacci / sum (ported from eval.rs) ────────────────────

#[test]
fn test_vm_fn_factorial_0() {
    let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f0 = factorial(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "f0"), 1);
}

#[test]
fn test_vm_fn_factorial_1() {
    let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f1 = factorial(1)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "f1"), 1);
}

#[test]
fn test_vm_fn_factorial_7() {
    let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f7 = factorial(7)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "f7"), 5040);
}

#[test]
fn test_vm_fn_fib_0() {
    let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib0 = fib(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "fib0"), 0);
}

#[test]
fn test_vm_fn_fib_1() {
    let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib1 = fib(1)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "fib1"), 1);
}

#[test]
fn test_vm_fn_fib_10() {
    let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib10 = fib(10)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "fib10"), 55);
}

#[test]
fn test_vm_fn_sum_to_0() {
    let src = "fn sum_to(n) {\n  if n <= 0 {\n    return 0\n  }\n  return n + sum_to(n - 1)\n}\nlet s0 = sum_to(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "s0"), 0);
}

#[test]
fn test_vm_fn_sum_to_10() {
    let src = "fn sum_to(n) {\n  if n <= 0 {\n    return 0\n  }\n  return n + sum_to(n - 1)\n}\nlet s10 = sum_to(10)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "s10"), 55);
}

// ── if / elif (ported from eval.rs) ──────────────────────────────────────

#[test]
fn test_vm_if_max() {
    let src = "fn max(a, b) {\n  if a > b {\n    return a\n  } else {\n    return b\n  }\n}\nlet m1 = max(3, 7)\nlet m2 = max(10, 2)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "m1"), 7);
    assert_eq!(get_int(&s, "m2"), 10);
}

#[test]
fn test_vm_if_is_positive() {
    let src = "fn is_positive(x) {\n  if x > 0 {\n    return true\n  } else {\n    return false\n  }\n}\nlet pos = is_positive(5)\nlet neg = is_positive(-3)";
    let s = run_src(src).unwrap();
    assert!(get_bool(&s, "pos"));
    assert!(!get_bool(&s, "neg"));
}

#[test]
fn test_vm_if_clamp() {
    let src = "fn clamp(x, lo, hi) {\n  if x < lo {\n    return lo\n  }\n  if x > hi {\n    return hi\n  }\n  return x\n}\nlet lo = clamp(1, 5, 10)\nlet mid = clamp(7, 5, 10)\nlet hi = clamp(15, 5, 10)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "lo"), 5);
    assert_eq!(get_int(&s, "mid"), 7);
    assert_eq!(get_int(&s, "hi"), 10);
}

#[test]
fn test_vm_nested_if_sign() {
    let src = "fn sign(x) {\n  if x > 0 {\n    return 1\n  } else {\n    if x < 0 {\n      return -1\n    } else {\n      return 0\n    }\n  }\n}\nlet s1 = sign(10)\nlet s2 = sign(-5)\nlet s3 = sign(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "s1"), 1);
    assert_eq!(get_int(&s, "s2"), -1);
    assert_eq!(get_int(&s, "s3"), 0);
}

#[test]
fn test_vm_nested_if_quadrant() {
    let src = "fn quadrant(a, b) {\n  if a > 0 {\n    if b > 0 {\n      return 1\n    } else {\n      return 4\n    }\n  } else {\n    if b > 0 {\n      return 2\n    } else {\n      return 3\n    }\n  }\n}\nlet q1 = quadrant(1, 1)\nlet q2 = quadrant(-1, 1)\nlet q3 = quadrant(-1, -1)\nlet q4 = quadrant(1, -1)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "q1"), 1);
    assert_eq!(get_int(&s, "q2"), 2);
    assert_eq!(get_int(&s, "q3"), 3);
    assert_eq!(get_int(&s, "q4"), 4);
}

#[test]
fn test_vm_elif_classify() {
    let src = "fn classify(x) {\n  if x > 0 {\n    return 1\n  } elif x < 0 {\n    return -1\n  } else {\n    return 0\n  }\n}\nlet pos = classify(5)\nlet neg = classify(-3)\nlet zero = classify(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "pos"), 1);
    assert_eq!(get_int(&s, "neg"), -1);
    assert_eq!(get_int(&s, "zero"), 0);
}

#[test]
fn test_vm_elif_chain() {
    let src = "fn grade(sc) {\n  if sc >= 90 {\n    return 4\n  } elif sc >= 80 {\n    return 3\n  } elif sc >= 70 {\n    return 2\n  } elif sc >= 60 {\n    return 1\n  } else {\n    return 0\n  }\n}\nlet a = grade(95)\nlet b = grade(85)\nlet c = grade(75)\nlet d = grade(65)\nlet f = grade(50)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "a"), 4);
    assert_eq!(get_int(&s, "b"), 3);
    assert_eq!(get_int(&s, "c"), 2);
    assert_eq!(get_int(&s, "d"), 1);
    assert_eq!(get_int(&s, "f"), 0);
}

#[test]
fn test_vm_elif_no_else() {
    let src = "fn check(x) {\n  if x == 1 {\n    return 10\n  } elif x == 2 {\n    return 20\n  }\n  return 0\n}\nlet r1 = check(1)\nlet r2 = check(2)\nlet r3 = check(3)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "r1"), 10);
    assert_eq!(get_int(&s, "r2"), 20);
    assert_eq!(get_int(&s, "r3"), 0);
}

#[test]
fn test_vm_nested_calls_pipeline() {
    let src = "fn add(a, b) {\n  return a + b\n}\nfn double(x) {\n  return x * 2\n}\nfn square(x) {\n  return x * x\n}\nfn pipeline(a, b) {\n  return double(square(add(a, b)))\n}\nlet pipe = pipeline(1, 2)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "pipe"), 18);
}

// ── function error cases (ported from eval.rs) ────────────────────────────

#[test]
fn test_vm_arity_mismatch() {
    let err =
        try_run_src("fn f(a) {\n  return a\n}\nlet x = f(1, 2)").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
}

#[test]
fn test_vm_not_callable() {
    let err = try_run_src("let x = 5\nlet y = x(1)").err().expect("expected error");
    assert!(matches!(err, JadeError::NotCallable { .. }));
}

// ── integer overflow (ported from eval.rs) ────────────────────────────────

// A Jade integer is 63-bit, not i64: the compiled representation spends one
// bit on the value tag and the language follows it, so both engines accept
// the same programs. These used to be written against i64::MAX, which the
// lexer now rejects as a literal before any arithmetic runs.
const INT_MAX: i64 = jade_runtime::value::JadeValue::INT_MAX;

#[test]
fn test_vm_integer_overflow_add() {
    let err = try_run_src(&format!("let x = {INT_MAX} + 1")).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

#[test]
fn test_vm_integer_overflow_sub() {
    let err = try_run_src(&format!("let x = -{INT_MAX} - 2")).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

#[test]
fn test_vm_integer_overflow_mul() {
    let err = try_run_src(&format!("let x = {INT_MAX} * 2")).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

/// Past the 63-bit range a *literal* is rejected at lex time, before any
/// arithmetic — one error for "too large to be a Jade integer" whether it
/// is written down or computed.
#[test]
fn test_literal_beyond_the_integer_range_is_rejected() {
    let err = try_run_src(&format!("let x = {}", i64::MAX)).err().expect("expected error");
    assert!(matches!(err, JadeError::LiteralOverflow { .. }), "got {err:?}");
}

/// The boundary itself is a perfectly ordinary integer.
#[test]
fn test_integer_range_boundary_is_usable() {
    try_run_src(&format!("let x = {INT_MAX}\nlet y = x - 1")).expect("INT_MAX must be usable");
}

#[test]
fn test_vm_nested_fn_ok() {
    // Nested function definitions are now a parse error.
    let tokens = crate::frontend::lexer::tokenize(
        "fn outer() {\n  fn inner() {\n    return 1\n  }\n  return 2\n}",
    )
    .expect("lex");
    let result = crate::frontend::parser::parse(tokens);
    assert!(matches!(result, Err(crate::frontend::error::JadeError::NestedFunction { .. })));
}

// ── while loops (ported from eval.rs) ────────────────────────────────────

#[test]
fn test_vm_while_condition_false_from_start() {
    let s = run_src("let never = 99\nwhile never < 0 {\n  never = never + 1\n}").unwrap();
    assert_eq!(get_int(&s, "never"), 99);
}

#[test]
fn test_vm_while_accumulate_sum() {
    let s = run_src("let sum = 0\nlet i = 1\nwhile i <= 10 {\n  sum = sum + i\n  i = i + 1\n}")
        .unwrap();
    assert_eq!(get_int(&s, "sum"), 55);
}

#[test]
fn test_vm_while_boolean_flag() {
    let s = run_src("let flag = true\nlet steps = 0\nwhile flag {\n  steps = steps + 1\n  if steps == 3 {\n    flag = false\n  }\n}").unwrap();
    assert_eq!(get_int(&s, "steps"), 3);
    assert!(!get_bool(&s, "flag"));
}

#[test]
fn test_vm_while_in_fn_factorial() {
    let src = "fn factorial(n) {\n  let result = 1\n  let i = 1\n  while i <= n {\n    result = result * i\n    i = i + 1\n  }\n  return result\n}\nlet f5 = factorial(5)\nlet f0 = factorial(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "f5"), 120);
    assert_eq!(get_int(&s, "f0"), 1);
}

#[test]
fn test_vm_while_return_propagates() {
    let src = "fn first_above(threshold) {\n  let n = 1\n  while n * n <= threshold {\n    n = n + 1\n  }\n  return n\n}\nlet r = first_above(9)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "r"), 4);
}

#[test]
fn test_vm_while_nested() {
    let src = "let total = 0\nlet i = 0\nwhile i < 3 {\n  let j = 0\n  while j < 3 {\n    total = total + 1\n    j = j + 1\n  }\n  i = i + 1\n}";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "total"), 9);
}

#[test]
fn test_vm_while_type_error_condition() {
    assert!(try_run_src("while 1 {\n}").is_err());
}

// ── struct error cases (ported from eval.rs) ──────────────────────────────

#[test]
fn test_vm_undefined_type_error() {
    let err = try_run_src("let p = Foo { x: 1 }").err().expect("expected error");
    assert!(matches!(err, JadeError::UndefinedType { .. }));
}

#[test]
fn test_vm_missing_field_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1 }")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::MissingField { .. }));
}

#[test]
fn test_vm_extra_field_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, z: 3 }")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_field_access_on_non_struct_error() {
    let err = try_run_src("let x = 5\nlet v = x.y").err().expect("expected error");
    assert!(matches!(
        err,
        JadeError::NotAStruct { .. }
            | JadeError::TypeMismatch { .. }
            | JadeError::UndefinedField { .. }
    ));
}

#[test]
fn test_vm_undefined_field_access_error() {
    let err =
        try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2 }\nlet v = p.z")
            .err()
            .expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

// ── strings (ported from eval.rs) ─────────────────────────────────────────

#[test]
fn test_vm_str_eq_true() {
    let s = run_src(r#"let b = "abc" == "abc""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_str_eq_false() {
    let s = run_src(r#"let b = "abc" == "xyz""#).unwrap();
    assert!(!get_bool(&s, "b"));
}

#[test]
fn test_vm_str_ne() {
    let s = run_src(r#"let b = "abc" != "xyz""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_str_lt() {
    let s = run_src(r#"let b = "abc" < "abd""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_str_gt() {
    let s = run_src(r#"let b = "b" > "a""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_str_le_equal() {
    let s = run_src(r#"let b = "abc" <= "abc""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_str_ge() {
    let s = run_src(r#"let b = "z" >= "a""#).unwrap();
    assert!(get_bool(&s, "b"));
}

/// Indexing a string yields a `char`, not a one-character `str`. Breaking as
/// of v1.2.1, and the reason `char` compares equal to the string spelling it.
#[test]
fn test_vm_str_index() {
    let s = run_src("let sv = \"hello\"\nlet h = sv[0]").unwrap();
    assert_eq!(get_char(&s, "h"), 'h');
}

#[test]
fn test_vm_str_index_last() {
    let s = run_src("let sv = \"hello\"\nlet o = sv[4]").unwrap();
    assert_eq!(get_char(&s, "o"), 'o');
}

/// A character of a tainted string is still tainted. Without this a loop
/// rebuilding a string character by character would launder it past `sh.exec`,
/// and nothing in `examples/trust/` would notice — those only use whole strings.
#[test]
fn a_char_taken_from_a_tainted_string_is_tainted() {
    let s = run_src("let sv = \"hi\"\nlet c = sv[0]").unwrap();
    match s.globals.get("c").expect("var not found") {
        VmValue::Char(c) => assert!(!c.is_tainted(), "a source literal is trusted"),
        v => panic!("expected Char, got {v:?}"),
    }
}

#[test]
fn test_vm_str_index_out_of_bounds() {
    let err = try_run_src("let sv = \"hi\"\nlet x = sv[10]").err().expect("expected error");
    assert!(matches!(err, JadeError::IndexOutOfBounds { index: 10, len: 2, .. }));
}

#[test]
fn test_vm_str_index_negative() {
    let err = try_run_src("let sv = \"hi\"\nlet x = sv[-1]").err().expect("expected error");
    assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
}

#[test]
fn test_vm_str_add_int_type_error() {
    assert!(try_run_src(r#"let x = "hello" + 1"#).is_err());
}

#[test]
fn test_vm_str_sub_type_error() {
    assert!(try_run_src(r#"let x = "a" - "b""#).is_err());
}

#[test]
fn test_vm_str_escape_tab() {
    let s = run_src(r#"let sv = "a\tb""#).unwrap();
    assert_eq!(get_str(&s, "sv"), "a\tb");
}

#[test]
fn test_vm_str_escape_newline() {
    let s = run_src(r#"let sv = "a\nb""#).unwrap();
    assert_eq!(get_str(&s, "sv"), "a\nb");
}

#[test]
fn test_vm_str_escape_quote() {
    let s = run_src(r#"let sv = "say \"hi\"""#).unwrap();
    assert_eq!(get_str(&s, "sv"), r#"say "hi""#);
}

#[test]
fn test_vm_print_builtin() {
    let s = run_src("let r = 0\nprint(\"hello\")").unwrap();
    assert_eq!(get_int(&s, "r"), 0);
}

#[test]
fn test_vm_print_arity_error() {
    // print() with no args is an error
    let err = try_run_src(r#"print()"#).err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
    // print() with 3 args is an error (max 2: value + optional end=)
    let err = try_run_src(r#"print("a", "b", "c")"#).err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_vm_triple_quote_simple() {
    let s = run_src(r#"let sv = """hello""""#).unwrap();
    assert_eq!(get_str(&s, "sv"), "hello");
}

#[test]
fn test_vm_triple_quote_with_inner_quotes() {
    let s = run_src(r#"let sv = """he said "hi" to her""""#).unwrap();
    assert_eq!(get_str(&s, "sv"), r#"he said "hi" to her"#);
}

#[test]
fn test_vm_triple_quote_concat() {
    let s = run_src(r#"let sv = """foo""" + """bar""""#).unwrap();
    assert_eq!(get_str(&s, "sv"), "foobar");
}

#[test]
fn test_vm_triple_quote_equals_regular() {
    let s = run_src(r#"let b = """abc""" == "abc""#).unwrap();
    assert!(get_bool(&s, "b"));
}

#[test]
fn test_vm_fstr_literal_only() {
    let s = run_src(r#"let sv = f"hello""#).unwrap();
    assert_eq!(get_str(&s, "sv"), "hello");
}

#[test]
fn test_vm_fstr_str_var() {
    let s = run_src("let name = \"Joe\"\nlet g = f\"hi {name}\"").unwrap();
    assert_eq!(get_str(&s, "g"), "hi Joe");
}

#[test]
fn test_vm_fstr_bool_var() {
    let s = run_src("let b = true\nlet sv = f\"b={b}\"").unwrap();
    assert_eq!(get_str(&s, "sv"), "b=true");
}

#[test]
fn test_vm_fstr_multiple_slots() {
    let s = run_src("let x = 1\nlet y = 2\nlet sv = f\"({x}, {y})\"").unwrap();
    assert_eq!(get_str(&s, "sv"), "(1, 2)");
}

#[test]
fn test_vm_fstr_field_access() {
    let s = run_src(
        "struct Point {\n  x,\n  y\n}\nlet p = Point { x: 3, y: 4 }\nlet sv = f\"({p.x}, {p.y})\"",
    )
    .unwrap();
    assert_eq!(get_str(&s, "sv"), "(3, 4)");
}

#[test]
fn test_vm_fstr_triple_quote() {
    let s = run_src("let name = \"Joe\"\nlet sv = f\"\"\"hi {name}\"\"\"").unwrap();
    assert_eq!(get_str(&s, "sv"), "hi Joe");
}

#[test]
fn test_vm_fstr_no_slots_equals_plain_str() {
    let s = run_src("let a = f\"hello\"\nlet b = \"hello\"").unwrap();
    assert_eq!(get_str(&s, "a"), "hello");
    assert_eq!(get_str(&s, "b"), "hello");
}

// ── pipe operator (ported from eval.rs) ───────────────────────────────────

#[test]
fn test_vm_pipe_simple() {
    let src = "fn double(x) {\n  return x * 2\n}\nlet n = 5 |> double";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "n"), 10);
}

#[test]
fn test_vm_pipe_chained() {
    let src = "fn double(x) {\n  return x * 2\n}\nlet m = 3 |> double |> double";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "m"), 12);
}

#[test]
fn test_vm_pipe_with_extra_arg() {
    let src = "fn add(a, b) {\n  return a + b\n}\nlet r = 5 |> add(3)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "r"), 8);
}

#[test]
fn test_vm_pipe_with_string() {
    let src = "fn greet(name) {\n  return f\"hello, {name}!\"\n}\nlet g = \"Jade\" |> greet";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "g"), "hello, Jade!");
}

#[test]
fn test_vm_pipe_arithmetic_lhs() {
    let src = "fn double(x) {\n  return x * 2\n}\nlet x = (2 + 3) |> double";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "x"), 10);
}

// ── arrays (ported from eval.rs) ──────────────────────────────────────────

#[test]
fn test_vm_array_empty() {
    let s = run_src("let a = []").unwrap();
    match s.globals.get("a").unwrap() {
        VmValue::Array(v) => assert!(v.lock().is_empty()),
        v => panic!("expected Array, got {:?}", v),
    }
}

#[test]
fn test_vm_array_int_elements() {
    let s = run_src("let a = [10, 20, 30]").unwrap();
    match s.globals.get("a").unwrap() {
        VmValue::Array(v) => {
            let guard = v.lock();
            assert!(matches!(guard[0], VmValue::Int(10)));
            assert!(matches!(guard[1], VmValue::Int(20)));
            assert!(matches!(guard[2], VmValue::Int(30)));
        }
        v => panic!("expected Array, got {:?}", v),
    }
}

#[test]
fn test_vm_array_index_last() {
    let s = run_src("let a = [10, 20, 30]\nlet x = a[2]").unwrap();
    assert_eq!(get_int(&s, "x"), 30);
}

#[test]
fn test_vm_array_index_out_of_bounds() {
    let err = try_run_src("let a = [1]\nlet x = a[1]").err().expect("expected error");
    assert!(matches!(err, JadeError::IndexOutOfBounds { index: 1, len: 1, .. }));
}

#[test]
fn test_vm_array_index_negative() {
    let err = try_run_src("let a = [1]\nlet x = a[-1]").err().expect("expected error");
    assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
}

#[test]
fn test_vm_array_reference_semantics() {
    // Arrays are Arc-wrapped: assigning creates an alias, not a copy.
    let s = run_src("let a = [1, 2]\nlet b = a\nb[0] = 42\nlet x = a[0]").unwrap();
    assert_eq!(get_int(&s, "x"), 42);
}

#[test]
fn test_vm_array_nested() {
    let s = run_src("let m = [[1, 2], [3, 4]]\nlet x = m[0][1]").unwrap();
    assert_eq!(get_int(&s, "x"), 2);
}

#[test]
fn test_vm_array_trailing_comma() {
    let s = run_src("let a = [1, 2, 3,]").unwrap();
    match s.globals.get("a").unwrap() {
        VmValue::Array(v) => assert_eq!(v.lock().len(), 3),
        v => panic!("expected Array, got {:?}", v),
    }
}

#[test]
fn test_vm_len_empty_array() {
    let s = run_src("let n = len([])").unwrap();
    assert_eq!(get_int(&s, "n"), 0);
}

#[test]
fn test_vm_len_type_error() {
    assert!(try_run_src("let n = len(42)").is_err());
}

// ── interfaces (ported from eval.rs) ──────────────────────────────────────

#[test]
fn test_vm_interface_basic() {
    let src = concat!(
        "interface Displayable {\n",
        "    fn to_str(self)\n",
        "}\n",
        "struct Point {\n  x,\n  y\n}\n",
        "extend Point: Displayable {\n",
        "    fn to_str(self) {\n",
        "        return \"point\"\n",
        "    }\n",
        "}\n",
        "let p = Point { x: 1, y: 2 }\n",
        "let sv = p.to_str()\n",
    );
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "sv"), "point");
}

#[test]
fn test_vm_interface_missing_method_error() {
    let src = concat!(
        "interface Displayable {\n",
        "    fn to_str(self)\n",
        "}\n",
        "struct Point {\n  x,\n  y\n}\n",
        "extend Point: Displayable {\n",
        "    fn area(self) {\n",
        "        return 0\n",
        "    }\n",
        "}\n",
    );
    let err = try_run_src(src).err().expect("expected error");
    assert!(matches!(err, JadeError::MissingInterfaceMethod { .. }));
}

#[test]
fn test_vm_interface_undefined_error() {
    let src = concat!(
        "struct Point {\n  x,\n  y\n}\n",
        "extend Point: Displayable {\n",
        "    fn to_str(self) {\n",
        "        return \"point\"\n",
        "    }\n",
        "}\n",
    );
    let err = try_run_src(src).err().expect("expected error");
    assert!(matches!(err, JadeError::UndefinedInterface { .. }));
}

// ── LLM / prompt (ported from eval.rs) ────────────────────────────────────

#[test]
fn test_vm_prompt_deref_no_backend_returns_error() {
    let err = try_run_src("prompt p = \"hi\"\nlet x = ?p").err().expect("expected error");
    assert!(matches!(err, JadeError::NoInferenceBackend { .. }));
}

#[test]
fn test_vm_prompt_deref_not_a_prompt_returns_error() {
    // The type checker catches `?x` where x: int before the VM runs;
    // the error is TypeMismatch (not the treewalk's runtime NotAPrompt).
    assert!(try_run_src("let x = 5\nlet y = ?x").is_err());
}

#[test]
fn test_vm_prompt_deref_field_access_no_backend() {
    let err = try_run_src(
        "struct Agent {\n  prompt system = \"helpful\"\n}\nlet a = Agent {}\nlet r = a.(?system)",
    )
    .err()
    .expect("expected error");
    assert!(matches!(err, JadeError::NoInferenceBackend { .. }));
}

#[test]
fn test_vm_prompt_deref_field_access_not_a_prompt() {
    let err = run_src_with_mock("struct S {\n  x,\n}\nlet s = S { x: 42 }\nlet r = s.(?x)", vec![])
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::NotAPrompt { .. }));
}

#[test]
fn test_vm_prompt_deref_field_access_with_mock() {
    let s = run_src_with_mock(
        "struct Agent {\n  prompt system = \"Say hello\"\n}\nlet a = Agent {}\nlet r = a.(?system)",
        vec!["hello!"],
    )
    .unwrap();
    assert_eq!(get_str(&s, "r"), "hello!");
}

#[test]
fn test_vm_postfix_deref_with_mock() {
    for deref in ["a.(?system)", "a~>system"] {
        let src = format!(
            "struct Agent {{\n  prompt system = \"Say hello\"\n}}\nlet a = Agent {{}}\nlet r = {deref}"
        );
        let s = run_src_with_mock(&src, vec!["hello!"]).unwrap();
        assert_eq!(get_str(&s, "r"), "hello!", "wrong result for {deref}");
    }
}

#[test]
fn test_vm_postfix_deref_typed_with_mock() {
    for deref in ["q.(?ask)", "q~>ask"] {
        let src = format!(
            "struct Q {{\n  prompt ask = \"What is 2+2?\"\n}}\nlet q = Q {{}}\nlet n = {deref} |> int"
        );
        let s = run_src_with_mock(&src, vec!["4"]).unwrap();
        assert_eq!(get_int(&s, "n"), 4, "wrong result for {deref}");
    }
}

#[test]
fn test_vm_typed_deref_int_success() {
    let s = run_src_with_mock("prompt p = \"What is 2+2?\"\nlet n = ?p |> int", vec!["4"]).unwrap();
    assert_eq!(get_int(&s, "n"), 4);
}

#[test]
fn test_vm_typed_deref_float_success() {
    let s = run_src_with_mock("prompt p = \"pi\"\nlet n = ?p |> float", vec!["3.14"]).unwrap();
    assert!((get_float(&s, "n") - 3.14).abs() < 0.001);
}

#[test]
fn test_vm_typed_deref_bool_success() {
    let s = run_src_with_mock("prompt p = \"true?\"\nlet n = ?p |> bool", vec!["true"]).unwrap();
    assert!(get_bool(&s, "n"));
}

#[test]
fn test_vm_typed_deref_str_success() {
    let s = run_src_with_mock("prompt p = \"hello\"\nlet n = ?p |> str", vec!["world"]).unwrap();
    assert_eq!(get_str(&s, "n"), "world");
}

#[test]
fn test_vm_typed_deref_overflow() {
    let err = run_src_with_mock(
        "prompt p = \"bad\"\nlet n = ?p |> int",
        vec!["oops", "still wrong", "nope", "nah"],
    )
    .err()
    .expect("expected error");
    assert!(matches!(err, JadeError::PromptOverflow { .. }));
}

#[test]
fn test_vm_untyped_deref_returns_str() {
    let s = run_src_with_mock("prompt p = \"test\"\nlet x = ?p", vec!["result"]).unwrap();
    assert_eq!(get_str(&s, "x"), "result");
}

#[test]
fn test_vm_typed_deref_is_single_shot() {
    // A typed deref no longer re-asks: the first non-coercing reply raises
    // PromptOverflow rather than triggering a correction round. (The daemon owns
    // any retry policy now; grammar-constrained sampling shapes the reply.)
    let err =
        run_src_with_mock("prompt p = \"number?\"\nlet n = ?p |> int", vec!["not a number", "42"])
            .err()
            .expect("expected error");
    assert!(matches!(err, JadeError::PromptOverflow { attempts: 1, .. }));
}

// ── Grammar ──────────────────────────────────────────────────────────────

#[test]
fn test_grammar_new_returns_grammar_value() {
    let s = run_src(r#"let g = Grammar.new('"yes" | "no"')"#).unwrap();
    match s.globals.get("g").unwrap() {
        VmValue::Grammar(g) => {
            assert_eq!(g.pattern, r#""yes" | "no""#);
            assert_eq!(g.anchor, None);
        }
        v => panic!("expected Grammar, got {:?}", v),
    }
}

#[test]
fn test_grammar_constrained_deref() {
    let s = run_src_with_mock(
        r#"
prompt p = "yes or no?"
let g = Grammar.new('"yes" | "no"')
let answer = ?p |> g
"#,
        vec!["yes"],
    )
    .unwrap();
    assert_eq!(get_str(&s, "answer"), "yes");
}

#[test]
fn test_grammar_new_with_anchor() {
    let s = run_src(r#"let g = Grammar.new('"yes" | "no"', anchor = "Answer:")"#).unwrap();
    match s.globals.get("g").unwrap() {
        VmValue::Grammar(g) => {
            assert_eq!(g.pattern, r#""yes" | "no""#);
            assert_eq!(g.anchor.as_deref(), Some("Answer:"));
        }
        v => panic!("expected Grammar, got {:?}", v),
    }
}

// ── dicts (ported from eval.rs) ───────────────────────────────────────────

#[test]
fn test_vm_dict_empty() {
    let s = run_src("let d = {}").unwrap();
    match s.globals.get("d").unwrap() {
        VmValue::Dict(m) => assert!(m.is_empty()),
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_vm_dict_string_values() {
    let s = run_src(r#"let d = {"name": "jade", "lang": "cool"}"#).unwrap();
    match s.globals.get("d").unwrap() {
        VmValue::Dict(m) => {
            assert!(matches!(m.get("name"), Some(VmValue::Str(s)) if s == "jade"));
            assert!(matches!(m.get("lang"), Some(VmValue::Str(s)) if s == "cool"));
        }
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_vm_dict_index_read_string_value() {
    let s = run_src("let d = {\"a\": \"hello\"}\nlet v = d[\"a\"]").unwrap();
    assert_eq!(get_str(&s, "v"), "hello");
}

#[test]
fn test_vm_dict_key_not_found() {
    let err = try_run_src("let d = {\"x\": 1}\nlet v = d[\"y\"]").err().expect("expected error");
    assert!(matches!(err, JadeError::KeyNotFound { key, .. } if key == "y"));
}

#[test]
fn test_vm_dict_index_assign_existing_key() {
    let s = run_src("let d = {\"v\": 1}\nd[\"v\"] = 99").unwrap();
    match s.globals.get("d").unwrap() {
        VmValue::Dict(m) => assert!(matches!(m.get("v"), Some(VmValue::Int(99)))),
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_vm_dict_index_assign_new_key() {
    let s = run_src("let d = {}\nd[\"k\"] = 5").unwrap();
    match s.globals.get("d").unwrap() {
        VmValue::Dict(m) => assert!(matches!(m.get("k"), Some(VmValue::Int(5)))),
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_vm_dict_len() {
    let s = run_src("let d = {\"a\": 1, \"b\": 2, \"c\": 3}\nlet n = len(d)").unwrap();
    assert_eq!(get_int(&s, "n"), 3);
}

#[test]
fn test_vm_dict_len_empty() {
    let s = run_src("let d = {}\nlet n = len(d)").unwrap();
    assert_eq!(get_int(&s, "n"), 0);
}

#[test]
fn test_vm_dict_value_semantics() {
    let src = "let d = {\"x\": 1}\nlet d2 = d\nd2[\"x\"] = 99";
    let s = run_src(src).unwrap();
    match s.globals.get("d").unwrap() {
        VmValue::Dict(m) => assert!(matches!(m.get("x"), Some(VmValue::Int(1)))),
        v => panic!("expected Dict, got {:?}", v),
    }
    match s.globals.get("d2").unwrap() {
        VmValue::Dict(m) => assert!(matches!(m.get("x"), Some(VmValue::Int(99)))),
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_vm_dict_variable_key() {
    let src = "let k = \"name\"\nlet d = {k: \"jade\"}\nlet v = d[\"name\"]";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "v"), "jade");
}

#[test]
fn test_vm_dict_non_string_index_type_error() {
    assert!(try_run_src("let d = {\"x\": 1}\nlet v = d[0]").is_err());
}

// ── struct field defaults (ported from eval.rs) ───────────────────────────

#[test]
fn test_vm_struct_default_omitted() {
    let s = run_src(
        "struct Config {\n  let host = \"localhost\"\n}\nlet c = Config {}\nlet h = c.host",
    )
    .unwrap();
    assert_eq!(get_str(&s, "h"), "localhost");
}

#[test]
fn test_vm_struct_default_overridden() {
    let s = run_src("struct Config {\n  let host = \"localhost\"\n}\nlet c = Config { host: \"example.com\" }\nlet h = c.host").unwrap();
    assert_eq!(get_str(&s, "h"), "example.com");
}

#[test]
fn test_vm_struct_all_defaults_empty_literal() {
    let s = run_src("struct Config {\n  let host = \"localhost\"\n  let port = 8080\n}\nlet c = Config {}\nlet h = c.host\nlet p = c.port").unwrap();
    assert_eq!(get_str(&s, "h"), "localhost");
    assert_eq!(get_int(&s, "p"), 8080);
}

#[test]
fn test_vm_struct_required_still_required() {
    let err = try_run_src("struct Mixed {\n  x,\n  let label = \"origin\"\n}\nlet m = Mixed {}")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::MissingField { .. }));
}

#[test]
fn test_vm_struct_mixed_fields() {
    let s = run_src("struct Mixed {\n  x,\n  y,\n  let label = \"origin\"\n}\nlet m = Mixed { x: 1, y: 2 }\nlet lbl = m.label").unwrap();
    assert_eq!(get_str(&s, "lbl"), "origin");
}

#[test]
fn test_vm_struct_prompt_field_default() {
    let s = run_src("struct Agent {\n  prompt system = \"You are helpful\"\n}\nlet a = Agent {}\nlet sv = a.system").unwrap();
    match s.globals.get("sv").unwrap() {
        VmValue::Prompt(t) => assert_eq!(t, "You are helpful"),
        v => panic!("expected Prompt, got {:?}", v),
    }
}

#[test]
fn test_vm_struct_prompt_field_override() {
    let s = run_src("struct Agent {\n  prompt system = \"You are helpful\"\n}\nlet a = Agent { system: \"Custom\" }\nlet sv = a.system").unwrap();
    match s.globals.get("sv").unwrap() {
        VmValue::Prompt(t) => assert_eq!(t, "Custom"),
        v => panic!("expected Prompt, got {:?}", v),
    }
}

// A prompt field holds text to send to a model, so a non-string value in one is
// rejected — whether it comes from the field's default or from an override at the
// literal. Both are caught in `type_infer`, before any bytecode runs, as
// `PromptFieldNotStr`.
//
// These were `#[ignore]`d as "the VM does not yet validate this, the treewalk
// did". It does now; the checks landed in the type checker, which covers both
// engines rather than just this one, and the attributes outlived the gap.

#[test]
fn test_vm_struct_prompt_field_non_string_error() {
    let Err(err) = try_run_src("struct Bad {\n  prompt sys = 42\n}\nlet b = Bad {}") else {
        panic!("a non-string prompt field default must be rejected");
    };
    assert!(matches!(err, JadeError::PromptFieldNotStr { .. }), "got {err:?}");
}

#[test]
fn test_vm_struct_prompt_field_override_non_string_error() {
    let Err(err) =
        try_run_src("struct Agent {\n  prompt system = \"ok\"\n}\nlet a = Agent { system: 99 }")
    else {
        panic!("a non-string prompt field override must be rejected");
    };
    assert!(matches!(err, JadeError::PromptFieldNotStr { .. }), "got {err:?}");
}

#[test]
fn test_vm_struct_extra_field_still_errors_with_defaults() {
    let err = try_run_src(
        "struct Agent {\n  let name = \"Jade\"\n}\nlet a = Agent { name: \"x\", extra: 1 }",
    )
    .err()
    .expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_struct_duplicate_field_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, x: 3 }")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::DuplicateField { field, .. } if field == "x"));
}

#[test]
fn test_vm_struct_default_references_variable() {
    let s = run_src("let base = 10\nstruct S {\n  let x = base\n}\nlet sv = S {}\nlet v = sv.x")
        .unwrap();
    assert_eq!(get_int(&s, "v"), 10);
}

#[test]
fn test_vm_struct_required_after_let_field() {
    let err = try_run_src("struct S {\n  let x = 0,\n  y\n}\nlet s = S { x: 1 }")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::MissingField { field, .. } if field == "y"));
}

// ── Empty struct tests ────────────────────────────────────────────────────

#[test]
fn test_vm_empty_struct_define_and_instantiate() {
    let s = run_src("struct Unit {}\nlet u = Unit {}").unwrap();
    match s.globals.get("u").unwrap() {
        VmValue::Struct(rc) => assert_eq!(rc.lock().type_name(), "Unit"),
        v => panic!("expected Struct, got {:?}", v),
    }
}

#[test]
fn test_vm_empty_struct_method() {
    let src = "struct Unit {}\nextend Unit {\n  fn tag(self) { return 42 }\n}\nlet u = Unit {}\nlet v = u.tag()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 42);
}

#[test]
fn test_vm_empty_struct_in_array() {
    let src = "struct Unit {}\nlet arr = [Unit {}, Unit {}, Unit {}]\nlet n = len(arr)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "n"), 3);
}

#[test]
fn test_vm_empty_struct_as_function_arg() {
    let src = "struct Unit {}\nfn consume(x) { return 1 }\nlet u = Unit {}\nlet v = consume(u)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 1);
}

#[test]
fn test_vm_empty_struct_extra_field_is_error() {
    let err = try_run_src("struct Unit {}\nlet u = Unit { x: 1 }").err().expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_empty_struct_raised_and_caught() {
    let src = "struct MyErr {}\ntry {\n  raise MyErr {}\n} catch MyErr e {\n  print(\"caught\")\n}";
    run_src(src).unwrap();
}

// ── Recursion limit (TOOLCHAIN-BUGS #10) ────────────────────────────────────
//
// `jade run` used to interpret directly on the calling thread's default ~8
// MiB stack, so a call chain past roughly 700 frames overflowed the *process*
// — an uncatchable abort with no Jade file or line, well short of what the
// compiled engine allowed (10000+). `call::MAX_CALL_DEPTH` is the counter
// that now stops a runaway recursion with an ordinary, catchable
// `JadeError::RecursionLimitExceeded` before the native stack becomes a
// factor at all — see `vm::run`'s `VM_STACK_SIZE` for the other half (giving
// the interpreter enough native stack to actually reach that counter).

fn depth_src(n: u32) -> String {
    format!(
        "fn depth(n) {{\n  if n <= 0 {{ return 0 }}\n  return 1 + depth(n - 1)\n}}\nlet d = depth({n})"
    )
}

/// The limit these tests use, which is deliberately not the real one.
///
/// Reaching 10,000 by actually recursing costs over a gigabyte of stack in a
/// debug build — the unoptimized async state machines run ~137 KB a frame —
/// and `cargo test` runs these in parallel. What needs covering is the
/// limit's behaviour, and that is identical at any depth.
const TEST_DEPTH: u32 = 24;

/// Run on a thread with a stack sized for the frames these tests actually
/// make, and with a runtime of its own.
///
/// Two reasons it is not the plain `run_src`. A nested `call_fn` costs ~137 KB
/// of native stack in a debug build, so even a couple of dozen frames outruns
/// the ~2 MiB the test harness gives a thread. And the runtime is built
/// *inside* the thread rather than borrowed from outside: a thread that takes
/// the caller's `Handle` and calls `block_on` deadlocks against a
/// current-thread runtime, because the caller is parked in `join()` and
/// nothing is left driving the reactor.
fn run_at_depth(src: &str, max_call_depth: u32) -> Result<VmState> {
    let tokens = lexer::tokenize(src)?;
    let program = parser::parse(tokens)?;
    let tprogram = type_infer::infer(program)?;
    let compiled = emit::emit(tprogram)?;
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let opts = VmOpts { max_call_depth: Some(max_call_depth), ..VmOpts::default() };
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(run(compiled, opts))
        })
        .expect("spawn the deep-stack test thread")
        .join()
        .expect("the deep-stack test thread panicked")
}

#[test]
fn test_vm_recursion_limit_matches_the_compiled_engine() {
    // The number itself is the fix: a program that fails must fail at the same
    // depth under both engines, or `jade run` and the binary disagree about
    // whether it is valid. The AOT side is `JRT_RECUR_MAX_DEPTH` in
    // `src/runtime_aot/common.c`, and nothing but this test ties them together.
    let c = include_str!("../runtime_aot/common.c");
    let want = format!("#define JRT_RECUR_MAX_DEPTH {MAX_CALL_DEPTH}");
    assert!(c.contains(&want), "the two engines must share one limit; expected `{want}`");
}

#[test]
fn test_vm_recursion_exactly_at_limit_succeeds() {
    // The limit counts *live* nested calls, so a chain of exactly the limit
    // is still allowed — `depth(0)` is itself the first call.
    let s = run_at_depth(&depth_src(TEST_DEPTH - 1), TEST_DEPTH).unwrap();
    assert_eq!(get_int(&s, "d"), (TEST_DEPTH - 1) as i64);
}

#[test]
fn test_vm_recursion_one_past_limit_raises_recursion_limit_exceeded() {
    let err = run_at_depth(&depth_src(TEST_DEPTH), TEST_DEPTH).err().expect("expected error");
    assert!(
        matches!(err, JadeError::RecursionLimitExceeded { .. }),
        "expected RecursionLimitExceeded, got {err:?}"
    );
    assert!(err.to_string().contains("recursion limit exceeded"), "should name it: {err}");
}

#[test]
fn test_vm_recursion_limit_is_catchable_and_execution_continues() {
    // Uncatchable before the fix: the process aborted before any Jade handler
    // ran. Raising rather than overflowing is the whole point.
    let src = format!(
        "fn depth(n) {{\n  if n <= 0 {{ return 0 }}\n  return 1 + depth(n - 1)\n}}\n\
         let msg = \"\"\nlet after = false\n\
         try {{\n  depth({TEST_DEPTH})\n}} catch e {{\n  msg = e.message\n}}\nafter = true"
    );
    let s = run_at_depth(&src, TEST_DEPTH).unwrap();
    assert!(get_str(&s, "msg").contains("recursion limit exceeded"));
    assert!(get_bool(&s, "after"), "execution should continue past the catch");
}

#[test]
fn test_vm_recursion_limit_is_a_runtime_error_struct() {
    // The shape every other built-in error raises through
    // `make_vm_runtime_error` — see exceptions/error_values/ in `examples/`.
    let src = format!(
        "fn depth(n) {{\n  if n <= 0 {{ return 0 }}\n  return 1 + depth(n - 1)\n}}\n\
         let kind = \"\"\n\
         try {{\n  depth({TEST_DEPTH})\n}} catch RuntimeError e {{\n  kind = \"RuntimeError\"\n}} \
         catch e {{\n  kind = \"untyped\"\n}}"
    );
    let s = run_at_depth(&src, TEST_DEPTH).unwrap();
    assert_eq!(get_str(&s, "kind"), "RuntimeError");
}

#[test]
fn test_vm_mutual_recursion_counts_toward_the_same_limit() {
    // The limit bounds call *depth*, not self-recursion by name — two functions
    // calling each other burn the same counter a direct recursive call would.
    let pingpong = "fn ping(n) {\n  if n <= 0 { return 0 }\n  return pong(n - 1)\n}\n\
                    fn pong(n) {\n  if n <= 0 { return 0 }\n  return ping(n - 1)\n}\n";
    let s =
        run_at_depth(&format!("{pingpong}let ok = ping({})", TEST_DEPTH - 1), TEST_DEPTH).unwrap();
    assert_eq!(get_int(&s, "ok"), 0);

    let err = run_at_depth(&format!("{pingpong}let ok = ping({TEST_DEPTH})"), TEST_DEPTH)
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::RecursionLimitExceeded { .. }));
}

// ── std/fs tests ──────────────────────────────────────────────────────────

#[test]
fn test_fs_write_and_read() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_write_read.txt");
    let path_str = path.to_str().unwrap();
    let src = format!(
        "use std::fs\nfs.write(\"{path_str}\", \"hello jade\")\nlet v = fs.read(\"{path_str}\")"
    );
    let s = run_src(&src).unwrap();
    assert_eq!(get_str(&s, "v"), "hello jade");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fs_exists_true() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_exists_true.txt");
    std::fs::write(&path, "x").unwrap();
    let path_str = path.to_str().unwrap();
    let src = format!("use std::fs\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(get_bool(&s, "v"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fs_exists_false() {
    let src = "use std::fs\nlet v = fs.exists(\"/tmp/jade_test_no_such_file_xyz.txt\")";
    let s = run_src(src).unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_fs_delete() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_delete.txt");
    std::fs::write(&path, "bye").unwrap();
    let path_str = path.to_str().unwrap();
    let src = format!("use std::fs\nfs.delete(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_fs_append() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_append.txt");
    let path_str = path.to_str().unwrap();
    let src = format!(
        "use std::fs\nfs.write(\"{path_str}\", \"hello\")\nfs.append(\"{path_str}\", \" world\")\nlet v = fs.read(\"{path_str}\")"
    );
    let s = run_src(&src).unwrap();
    assert_eq!(get_str(&s, "v"), "hello world");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fs_list_dir() {
    let dir = std::env::temp_dir();
    let subdir = dir.join("jade_test_fs_list_dir");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join("a.txt"), "").unwrap();
    std::fs::write(subdir.join("b.txt"), "").unwrap();
    let path_str = subdir.to_str().unwrap();
    let src = format!("use std::fs\nlet v = fs.list_dir(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("v").unwrap() {
        VmValue::Array(a) => {
            let names: Vec<String> = a
                .lock()
                .iter()
                .map(|v| match v {
                    VmValue::Str(s) => s.to_string(),
                    _ => panic!("non-str entry"),
                })
                .collect();
            assert!(names.contains(&"a.txt".to_string()));
            assert!(names.contains(&"b.txt".to_string()));
        }
        _ => panic!("expected array"),
    }
    let _ = std::fs::remove_dir_all(&subdir);
}

#[test]
fn test_fs_mkdir() {
    let dir = std::env::temp_dir();
    let newdir = dir.join("jade_test_fs_mkdir_new/nested");
    let path_str = newdir.to_str().unwrap();
    let _ = std::fs::remove_dir_all(dir.join("jade_test_fs_mkdir_new"));
    let src = format!("use std::fs\nfs.mkdir(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(get_bool(&s, "v"));
    let _ = std::fs::remove_dir_all(dir.join("jade_test_fs_mkdir_new"));
}

#[test]
fn test_fs_read_nonexistent_errors() {
    let err = try_run_src("use std::fs\nlet v = fs.read(\"/tmp/jade_no_such_file_xyz.txt\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn test_fs_write_arity_error() {
    let err = try_run_src("use std::fs\nfs.write(\"path\")").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { expected: 2, .. }));
}

// ── nil equality with unknown-typed values ────────────────────────────────

#[test]
fn test_nil_eq_struct_via_unknown_param() {
    // When a function param has type Unknown, `param != nil` must work at
    // runtime even when the argument is a struct (CmpNe dynamic path).
    let s = run_src(
        "struct Foo {}\nfn check(x) {\n if x != nil { return 1 }\n return 0\n}\nlet a = check(Foo {})\nlet b = check(nil)"
    ).unwrap();
    assert_eq!(get_int(&s, "a"), 1);
    assert_eq!(get_int(&s, "b"), 0);
}

#[test]
fn test_nil_eq_eq_struct_via_unknown_param() {
    let s = run_src(
        "struct Bar {}\nfn is_nil(x) {\n return x == nil\n}\nlet a = is_nil(nil)\nlet b = is_nil(Bar {})"
    ).unwrap();
    assert!(get_bool(&s, "a"));
    assert!(!get_bool(&s, "b"));
}

// ── module stdlib promotion ───────────────────────────────────────────────

/// Module-level `let` bindings must be visible inside module functions when
/// those functions are called from the parent. Functions are exported as
/// closures capturing the module scope.
#[test]
fn test_import_module_let_binding_visible_in_fn() {
    let dir = std::env::temp_dir().join("jade_test_mod_let_d");
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("m.jde");
    std::fs::write(
        &mod_path,
        "let _PREFIX = \"hi \"\nfn greet(name) {\n return _PREFIX + name\n}\n",
    )
    .unwrap();
    let src = "use m\nlet v = m.greet(\"jade\")".to_string();

    let tokens = crate::frontend::lexer::tokenize(&src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let tprogram = crate::compiler::type_infer::infer(program).expect("type infer");
    let compiled = crate::compiler::emit::emit(tprogram).expect("emit");
    let opts = VmOpts { source_dir: dir.clone(), ..VmOpts::default() };
    let s = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(compiled, opts))
        .expect("vm run");

    assert_eq!(get_str(&s, "v"), "hi jade");
    let _ = std::fs::remove_file(&mod_path);
}

/// Mutable module-level state (let bindings updated by module functions) must
/// persist across calls — SetGlobal inside a module function writes to the
/// module scope, not the parent globals.
#[test]
fn test_import_module_mutable_state() {
    let dir = std::env::temp_dir().join("jade_test_mod_mut_d");
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("c.jde");
    std::fs::write(
        &mod_path,
        "let count = 0\nfn inc() { count = count + 1 }\nfn get() { return count }\n",
    )
    .unwrap();
    let src = "use c\nc.inc()\nc.inc()\nc.inc()\nlet v = c.get()".to_string();

    let tokens = crate::frontend::lexer::tokenize(&src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let tprogram = crate::compiler::type_infer::infer(program).expect("type infer");
    let compiled = crate::compiler::emit::emit(tprogram).expect("emit");
    let opts = VmOpts { source_dir: dir.clone(), ..VmOpts::default() };
    let s = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(compiled, opts))
        .expect("vm run");

    assert_eq!(get_int(&s, "v"), 3);
    let _ = std::fs::remove_file(&mod_path);
}

/// A module that imports `use std::fs` should expose functions that use `fs`
/// to callers in the parent scope — the stdlib import must be promoted to the
/// parent globals rather than buried inside the module dict.
#[test]
fn test_import_module_stdlib_promotion() {
    let dir = std::env::temp_dir().join("jade_test_stdlib_promo_d");
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("io.jde");
    let txt_path = dir.join("out.txt");
    std::fs::write(&mod_path, "use std::fs\nfn write_file(p, s) {\n fs.write(p, s)\n}\n").unwrap();

    let txt_str = txt_path.to_str().unwrap();
    let src = format!("use io\nio.write_file(\"{txt_str}\", \"ok\")\nlet v = true");

    let tokens = crate::frontend::lexer::tokenize(&src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let tprogram = crate::compiler::type_infer::infer(program).expect("type infer");
    let compiled = crate::compiler::emit::emit(tprogram).expect("emit");
    let opts = VmOpts { source_dir: dir.clone(), ..VmOpts::default() };
    let s = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(compiled, opts))
        .expect("vm run");

    assert!(get_bool(&s, "v"));
    let content = std::fs::read_to_string(&txt_path).expect("output not written");
    assert_eq!(content, "ok");

    let _ = std::fs::remove_file(&mod_path);
    let _ = std::fs::remove_file(&txt_path);
    let _ = std::fs::remove_dir(&dir);
}

// ── imported struct metadata ──────────────────────────────────────────────

/// Compile and run `main_body` with `mod_src` written as the module `m` (a
/// sibling `m.jde`), importable via `use m`. Each test gets its own directory
/// (keyed by `stem`) so the fixed module name can't collide with sibling tests
/// in the shared temp dir. Returns the final VM state.
fn run_with_module(stem: &str, mod_src: &str, main_body: &str) -> VmState {
    let dir = std::env::temp_dir().join(format!("{stem}_d"));
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("m.jde");
    std::fs::write(&mod_path, mod_src).unwrap();
    let src = format!("use m\n{main_body}");

    let tokens = crate::frontend::lexer::tokenize(&src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let tprogram = crate::compiler::type_infer::infer(program).expect("type infer");
    let compiled = crate::compiler::emit::emit(tprogram).expect("emit");
    let opts = VmOpts { source_dir: dir.clone(), ..VmOpts::default() };
    let s = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(compiled, opts))
        .expect("vm run");

    let _ = std::fs::remove_file(&mod_path);
    let _ = std::fs::remove_dir(&dir);
    s
}

/// Field defaults on an imported struct must be applied. Instances carry the
/// bare type name, so registering the imported def only under the
/// namespaced key left `MakeStruct` unable to find the defaults.
#[test]
fn test_imported_struct_applies_field_defaults() {
    let s = run_with_module(
        "jade_test_imported_defaults",
        "struct Cfg {\n a,\n let b = 99\n}\n",
        "let c = m.Cfg { a: 1 }\nlet v = c.b",
    );
    assert_eq!(get_int(&s, "v"), 99);
}

/// Methods from an imported `extend` block must resolve on instances of the
/// imported struct — the same bare-vs-namespaced key mismatch made every
/// imported method look like a missing field.
#[test]
fn test_imported_extend_method_resolves() {
    let s = run_with_module(
        "jade_test_imported_extend",
        "struct P {\n x\n}\nextend P {\n fn double(self) {\n  return self.x * 2\n }\n}\n",
        "let p = m.P { x: 21 }\nlet v = p.double()",
    );
    assert_eq!(get_int(&s, "v"), 42);
}

/// The same, through an interface — this is the case the AOT backend already
/// handled while the VM did not.
#[test]
fn test_imported_interface_method_resolves() {
    let s = run_with_module(
        "jade_test_imported_iface",
        "interface Show {\n fn show(self)\n}\nstruct Q {\n n\n}\nextend Q: Show {\n fn show(self) {\n  return \"n=\" + str(self.n)\n }\n}\n",
        "let q = m.Q { n: 7 }\nlet v = q.show()",
    );
    assert_eq!(get_str(&s, "v"), "n=7");
}

/// A locally-defined type must win over an imported one of the same name:
/// the importing file's own defs are merged before its imports execute, and
/// bare keys are registered with `or_insert` so they never overwrite.
#[test]
fn test_local_struct_shadows_imported_same_name() {
    let s = run_with_module(
        "jade_test_imported_shadow",
        "struct Dup {\n a,\n let tag = \"imported\"\n}\n",
        "struct Dup {\n a,\n let tag = \"local\"\n}\nlet d = Dup { a: 1 }\nlet v = d.tag",
    );
    assert_eq!(get_str(&s, "v"), "local");
}

// ── type constructors ─────────────────────────────────────────────────────

#[test]
fn test_type_int_from_str() {
    let s = run_src("let v = int(\"42\")").unwrap();
    assert_eq!(get_int(&s, "v"), 42);
}

#[test]
fn test_type_int_from_str_whitespace() {
    let s = run_src("let v = int(\"  7  \")").unwrap();
    assert_eq!(get_int(&s, "v"), 7);
}

#[test]
fn test_type_int_from_float_truncates() {
    let s = run_src("let v = int(3.9)").unwrap();
    assert_eq!(get_int(&s, "v"), 3);
}

#[test]
fn test_type_int_from_bool_true() {
    let s = run_src("let v = int(true)").unwrap();
    assert_eq!(get_int(&s, "v"), 1);
}

#[test]
fn test_type_int_from_bool_false() {
    let s = run_src("let v = int(false)").unwrap();
    assert_eq!(get_int(&s, "v"), 0);
}

#[test]
fn test_type_int_from_int_identity() {
    let s = run_src("let v = int(99)").unwrap();
    assert_eq!(get_int(&s, "v"), 99);
}

#[test]
fn test_type_int_invalid_str_errors() {
    assert!(try_run_src("let v = int(\"abc\")").is_err());
}

#[test]
fn test_type_float_from_str() {
    let s = run_src("let v = float(\"3.14\")").unwrap();
    assert!((get_float(&s, "v") - 3.14).abs() < 1e-10);
}

#[test]
fn test_type_float_from_int() {
    let s = run_src("let v = float(5)").unwrap();
    assert!((get_float(&s, "v") - 5.0).abs() < 1e-10);
}

#[test]
fn test_type_float_from_bool() {
    let s = run_src("let v = float(true)").unwrap();
    assert!((get_float(&s, "v") - 1.0).abs() < 1e-10);
}

#[test]
fn test_type_float_identity() {
    let s = run_src("let v = float(2.5)").unwrap();
    assert!((get_float(&s, "v") - 2.5).abs() < 1e-10);
}

#[test]
fn test_type_bool_from_zero_is_false() {
    let s = run_src("let v = bool(0)").unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_nonzero_is_true() {
    let s = run_src("let v = bool(42)").unwrap();
    assert!(get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_nil_is_false() {
    let s = run_src("let v = bool(nil)").unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_str_true() {
    let s = run_src("let v = bool(\"true\")").unwrap();
    assert!(get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_str_false() {
    let s = run_src("let v = bool(\"false\")").unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_empty_str_is_false() {
    let s = run_src("let v = bool(\"\")").unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_type_bool_from_nonempty_str_is_true() {
    let s = run_src("let v = bool(\"anything\")").unwrap();
    assert!(get_bool(&s, "v"));
}

#[test]
fn test_type_str_from_int() {
    let s = run_src("let v = str(42)").unwrap();
    assert_eq!(get_str(&s, "v"), "42");
}

#[test]
fn test_type_str_from_bool() {
    let s = run_src("let v = str(true)").unwrap();
    assert_eq!(get_str(&s, "v"), "true");
}

#[test]
fn test_type_str_from_float() {
    let s = run_src("let v = str(3.14)").unwrap();
    assert_eq!(get_str(&s, "v"), "3.14");
}

#[test]
fn test_type_int_wrong_arity_errors() {
    assert!(try_run_src("let v = int(1, 2)").is_err());
}

// A struct type is not callable. `City { .. }` is the one construction,
// and the only one that checks required fields and applies declared
// defaults. Calling the type used to build a struct from a dict, filling
// every absent field with nil — so a *required* field could end up missing
// and `let population = 0` was ignored. Nobody declared that path; it fell
// out of type names being callable values.
#[test]
fn test_calling_a_struct_type_is_an_error() {
    let src = "struct City {\n  name,\n}\nlet d = {\"name\": \"Paris\"}\nlet c = City(d)";
    let err = match try_run_src(src) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("calling a struct type should be an error"),
    };
    assert!(
        err.contains("not a function"),
        "the error should say a type is not callable, got: {err}"
    );
}

// Not even for a value that is already the right type — there is no
// identity-conversion special case to fall back on.
#[test]
fn test_calling_a_struct_type_on_its_own_type_is_an_error() {
    let src = "struct Point {\n  x,\n}\nlet p = Point { x: 3 }\nlet q = Point(p)";
    assert!(try_run_src(src).is_err());
}

// The literal keeps applying declared defaults — that is the behaviour the
// type call lacked, and the reason it went away rather than being fixed.
#[test]
fn test_struct_literal_still_applies_declared_defaults() {
    let src = "struct City {\n  name,\n  let population = 7,\n}\nlet c = City { name: \"Paris\" }\nlet v = c.population";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 7);
}

#[test]
fn test_type_struct_from_incompatible_errors() {
    assert!(try_run_src("struct S {\n  x,\n}\nlet v = S(42)").is_err());
}

// ── func() ───────────────────────────────────────────────────────────────

#[test]
fn test_func_lookup_and_call() {
    let src = "fn double(x) {\n  return x * 2\n}\nlet f = func(\"double\")\nlet v = f(5)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 10);
}

#[test]
fn test_func_lookup_nonexistent_errors() {
    assert!(try_run_src("let f = func(\"no_such_fn\")").is_err());
}

#[test]
fn test_func_passthrough_existing_fn_value() {
    let src = "fn greet() {\n  return 1\n}\nlet f = func(greet)\nlet v = f()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 1);
}

#[test]
fn test_func_non_string_non_fn_errors() {
    assert!(try_run_src("let f = func(42)").is_err());
}

// ── default parameter values ──────────────────────────────────────────────

#[test]
fn test_default_param_used_when_omitted() {
    let src = "fn add(a, b = 10) {\n  return a + b\n}\nlet v = add(5)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 15);
}

#[test]
fn test_default_param_overridden_by_caller() {
    let src = "fn add(a, b = 10) {\n  return a + b\n}\nlet v = add(5, 3)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 8);
}

#[test]
fn test_default_param_multiple_defaults() {
    let src = "fn f(a = 1, b = 2, c = 3) {\n  return a + b + c\n}\nlet v = f()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 6);
}

#[test]
fn test_default_param_partial_override() {
    let src = "fn f(a = 1, b = 2, c = 3) {\n  return a + b + c\n}\nlet v = f(10, 20)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 33);
}

#[test]
fn test_default_param_nil() {
    let src = "fn f(x, on = nil) {\n  return on\n}\nlet v = f(1)";
    let s = run_src(src).unwrap();
    assert!(matches!(s.globals.get("v").unwrap(), VmValue::Nil));
}

#[test]
fn test_null_alias_of_nil() {
    // `null` evaluates to Nil and compares equal to both `nil` and `None`
    let src = "let a = null\nlet b = (null == nil)\nlet c = (null == None)";
    let s = run_src(src).unwrap();
    assert!(matches!(s.globals.get("a").unwrap(), VmValue::Nil));
    assert!(get_bool(&s, "b"));
    assert!(get_bool(&s, "c"));
}

#[test]
fn test_default_param_null() {
    let src = "fn f(x, on = null) {\n  return on\n}\nlet v = f(1)";
    let s = run_src(src).unwrap();
    assert!(matches!(s.globals.get("v").unwrap(), VmValue::Nil));
}

#[test]
fn test_default_param_str() {
    let src = "fn f(x, label = \"default\") {\n  return label\n}\nlet v = f(0)";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "v"), "default");
}

#[test]
fn test_default_param_bool() {
    let src = "fn f(x, flag = false) {\n  return flag\n}\nlet v = f(1)";
    let s = run_src(src).unwrap();
    assert!(!get_bool(&s, "v"));
}

// ── decorator with args ───────────────────────────────────────────────────

#[test]
fn test_decorator_no_args_passthrough() {
    let src =
        "fn identity(f) {\n  return f\n}\n@identity\nfn greet() {\n  return 42\n}\nlet v = greet()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 42);
}

#[test]
fn test_decorator_positional_arg() {
    // Decorator receives (fn, label) and returns the fn unchanged.
    let src = "fn tag(f, label) {\n  return f\n}\n@tag(\"hello\")\nfn greet() {\n  return 99\n}\nlet v = greet()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 99);
}

#[test]
fn test_decorator_wraps_function() {
    // Decorator stores a tag on the function's return value by recording a global.
    // (Closure capture of fn-local params is not yet supported in the emitter.)
    let src = "let calls = 0\nfn count(f) {\n  calls = calls + 1\n  return f\n}\n@count\nfn work() {\n  return 7\n}\nlet v = work()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 7);
    assert_eq!(get_int(&s, "calls"), 1);
}

#[test]
fn test_decorator_with_kwarg() {
    // @tag(label = "x") — keyword arg is passed positionally to tag(f, label).
    let src = "fn tag(f, label) {\n  return f\n}\n@tag(label = \"x\")\nfn greet() {\n  return 7\n}\nlet v = greet()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 7);
}

#[test]
fn test_multiple_decorators_applied_in_order() {
    // Two passthrough decorators — fn survives both.
    let src = "fn p(f) {\n  return f\n}\n@p\n@p\nfn val() {\n  return 3\n}\nlet v = val()";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 3);
}

// ── route() builtin ───────────────────────────────────────────────────────

#[test]
fn test_route_explicit_method_name_extend_method() {
    let src = "struct Animal {\n  sound,\n}\nextend Animal {\n  fn bark(self) {\n    return \"woof\"\n  }\n  fn meow(self) {\n    return \"purr\"\n  }\n}\nlet a = Animal { sound: \"bark\" }\nlet v = route(a, \"bark\")";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "v"), "woof");
}

#[test]
fn test_route_explicit_method_falls_back_to_global() {
    let src = "struct Thing {}\nfn handle(t) {\n  return 100\n}\nlet t = Thing {}\nlet v = route(t, \"handle\")";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 100);
}

#[test]
fn test_route_unknown_method_errors() {
    let src = "struct S {}\nlet s = S {}\nlet v = route(s, \"nope\")";
    assert!(try_run_src(src).is_err());
}

#[test]
fn test_route_nil_on_returns_obj() {
    // route(obj, nil) returns obj unchanged.
    let src = "struct S {\n  x,\n}\nlet s = S { x: 5 }\nlet r = route(s, nil)\nlet v = r.x";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 5);
}

#[test]
fn test_route_non_string_on_errors() {
    let src = "struct S {}\nlet s = S {}\nlet v = route(s, 42)";
    assert!(try_run_src(src).is_err());
}

// ── @route decorator on extend ────────────────────────────────────────────

#[test]
fn test_route_decorator_positional_field() {
    let src = "struct Cmd {\n  action,\n}\n@route(\"action\")\nextend Cmd {\n  fn run(self) {\n    return \"running\"\n  }\n  fn stop(self) {\n    return \"stopped\"\n  }\n}\nlet c = Cmd { action: \"run\" }\nlet v = route(c)";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "v"), "running");
}

#[test]
fn test_route_decorator_kwarg_on() {
    // @route(on = "action") is equivalent to @route("action").
    let src = "struct Cmd {\n  action,\n}\n@route(on = \"action\")\nextend Cmd {\n  fn run(self) {\n    return \"running\"\n  }\n}\nlet c = Cmd { action: \"run\" }\nlet v = route(c)";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "v"), "running");
}

#[test]
fn test_route_decorator_dispatches_different_methods() {
    let src = "struct Op {\n  kind,\n}\n@route(\"kind\")\nextend Op {\n  fn add(self) {\n    return 1\n  }\n  fn sub(self) {\n    return 2\n  }\n}\nlet a = Op { kind: \"add\" }\nlet b = Op { kind: \"sub\" }\nlet va = route(a)\nlet vb = route(b)";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "va"), 1);
    assert_eq!(get_int(&s, "vb"), 2);
}

#[test]
fn test_route_no_config_and_no_on_returns_obj() {
    // route(obj) with no registered config returns obj unchanged (on = nil path).
    let src = "struct S {\n  x,\n}\nlet s = S { x: 7 }\nlet r = route(s)\nlet v = r.x";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 7);
}

// ── Implicit self shorthand (.field) ──────────────────────────────────────

#[test]
fn test_implicit_self_read() {
    let src = "
struct Counter { count }
extend Counter {
fn value(self) {
    return .count
}
}
let c = Counter { count: 42 }
let v = c.value()
";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 42);
}

#[test]
fn test_implicit_self_write() {
    let src = "
struct Counter { count }
extend Counter {
fn inc(self) {
    .count = .count + 1
}
}
let c = Counter { count: 0 }
c.inc()
c.inc()
c.inc()
let v = c.count
";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 3);
}

#[test]
fn test_implicit_self_mixed_with_explicit() {
    // Can mix .field and self.field in the same method.
    let src = "
struct Point { x, y }
extend Point {
fn sum(self) {
    return .x + self.y
}
}
let p = Point { x: 3, y: 4 }
let v = p.sum()
";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 7);
}

// ── std/http ──────────────────────────────────────────────────────────────

fn start_http_test_server(status: u16, body: &'static str) -> u16 {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let response = format!(
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

#[test]
fn test_http_get_status_and_body() {
    let port = start_http_test_server(200, "hello jade");
    let src = format!("use std::http\nlet r = http.get(\"http://127.0.0.1:{port}/\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => {
            match map.get("status").unwrap() {
                VmValue::Int(n) => assert_eq!(*n, 200),
                v => panic!("expected Int, got {:?}", v),
            }
            match map.get("body").unwrap() {
                VmValue::Str(b) => assert_eq!(b, "hello jade"),
                v => panic!("expected Str, got {:?}", v),
            }
        }
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_http_post_returns_response() {
    let port = start_http_test_server(201, "created");
    let src =
        format!("use std::http\nlet r = http.post(\"http://127.0.0.1:{port}/\", \"payload\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => {
            match map.get("status").unwrap() {
                VmValue::Int(n) => assert_eq!(*n, 201),
                v => panic!("expected Int, got {:?}", v),
            }
            match map.get("body").unwrap() {
                VmValue::Str(b) => assert_eq!(b, "created"),
                v => panic!("expected Str, got {:?}", v),
            }
        }
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_http_get_with_headers() {
    let port = start_http_test_server(200, "ok");
    let src = format!(
        "use std::http\nlet r = http.get(\"http://127.0.0.1:{port}/\", {{\"X-Test\": \"jade\"}})"
    );
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => match map.get("status").unwrap() {
            VmValue::Int(n) => assert_eq!(*n, 200),
            v => panic!("expected Int, got {:?}", v),
        },
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_http_get_arity_error() {
    let err = try_run_src("use std::http\nhttp.get()").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_http_get_type_error() {
    let err = try_run_src("use std::http\nhttp.get(42)").err().expect("expected error");
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn test_http_post_arity_error() {
    let err = try_run_src("use std::http\nhttp.post(\"http://example.com\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_http_get_connection_refused_errors() {
    let err = try_run_src("use std::http\nhttp.get(\"http://127.0.0.1:1/\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

// ── std/uhttp ─────────────────────────────────────────────────────────────

// Binds a UnixListener on a unique temp path and serves one canned HTTP/1.1
// response. Returns the socket path (unlinked on the listener thread's drop is
// not guaranteed, so we bind under a per-test unique name to avoid AddrInUse).
fn start_uhttp_test_server(response: String) -> String {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(0);
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("jade_uhttp_test_{pid}_{n}.sock"));
    // Clear any stale socket file from a prior aborted run.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let path_str = path.to_string_lossy().into_owned();
    let cleanup = path.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
        }
        let _ = std::fs::remove_file(&cleanup);
    });
    path_str
}

fn canned_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

#[test]
fn test_uhttp_get_status_and_body() {
    let sock = start_uhttp_test_server(canned_response(200, "hello jade"));
    let src = format!("use std::uhttp\nlet r = uhttp.get(\"unix://{sock}:/\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => {
            match map.get("status").unwrap() {
                VmValue::Int(n) => assert_eq!(*n, 200),
                v => panic!("expected Int, got {:?}", v),
            }
            match map.get("body").unwrap() {
                VmValue::Str(b) => assert_eq!(b, "hello jade"),
                v => panic!("expected Str, got {:?}", v),
            }
        }
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_uhttp_post_returns_response() {
    let sock = start_uhttp_test_server(canned_response(201, "created"));
    let src = format!("use std::uhttp\nlet r = uhttp.post(\"unix://{sock}:/submit\", \"payload\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => {
            match map.get("status").unwrap() {
                VmValue::Int(n) => assert_eq!(*n, 201),
                v => panic!("expected Int, got {:?}", v),
            }
            match map.get("body").unwrap() {
                VmValue::Str(b) => assert_eq!(b, "created"),
                v => panic!("expected Str, got {:?}", v),
            }
        }
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_uhttp_get_with_headers() {
    let sock = start_uhttp_test_server(canned_response(200, "ok"));
    let src =
        format!("use std::uhttp\nlet r = uhttp.get(\"unix://{sock}:/\", {{\"X-Test\": \"jade\"}})");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => match map.get("status").unwrap() {
            VmValue::Int(n) => assert_eq!(*n, 200),
            v => panic!("expected Int, got {:?}", v),
        },
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_uhttp_get_chunked_body() {
    // "Wiki" + "pedia" split across two chunks (RFC 7230 example shape).
    let response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                    4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"
        .to_string();
    let sock = start_uhttp_test_server(response);
    let src = format!("use std::uhttp\nlet r = uhttp.get(\"unix://{sock}:/\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => match map.get("body").unwrap() {
            VmValue::Str(b) => assert_eq!(b, "Wikipedia"),
            v => panic!("expected Str, got {:?}", v),
        },
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_uhttp_head_empty_body() {
    let sock = start_uhttp_test_server(canned_response(200, "should-be-ignored"));
    let src = format!("use std::uhttp\nlet r = uhttp.head(\"unix://{sock}:/\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("r").unwrap() {
        VmValue::Dict(map) => match map.get("body").unwrap() {
            VmValue::Str(b) => assert_eq!(b, ""),
            v => panic!("expected Str, got {:?}", v),
        },
        v => panic!("expected Dict, got {:?}", v),
    }
}

#[test]
fn test_uhttp_get_arity_error() {
    let err = try_run_src("use std::uhttp\nuhttp.get()").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_uhttp_get_type_error() {
    let err = try_run_src("use std::uhttp\nuhttp.get(42)").err().expect("expected error");
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn test_uhttp_post_arity_error() {
    let err = try_run_src("use std::uhttp\nuhttp.post(\"unix:///tmp/x.sock:/\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_uhttp_connect_error() {
    let err = try_run_src("use std::uhttp\nuhttp.get(\"unix:///nonexistent/jade-uhttp.sock:/\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn test_uhttp_bad_scheme_error() {
    let err = try_run_src("use std::uhttp\nuhttp.get(\"http://127.0.0.1/\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

// Streams a chunked HTTP/1.1 response where each line is its own chunk (the
// shape Docker's /events and /logs endpoints use — newline-delimited JSON).
fn start_uhttp_stream_server(status: u16, lines: Vec<&'static str>) -> String {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NONCE: AtomicU64 = AtomicU64::new(1_000);
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("jade_uhttp_stream_{pid}_{n}.sock"));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let path_str = path.to_string_lossy().into_owned();
    let cleanup = path.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf);
            let mut resp = format!(
                "HTTP/1.1 {} OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                status
            );
            for line in &lines {
                let payload = format!("{line}\n"); // one event per chunk
                resp.push_str(&format!("{:x}\r\n{}\r\n", payload.len(), payload));
            }
            resp.push_str("0\r\n\r\n"); // final chunk
            let _ = stream.write_all(resp.as_bytes());
        }
        let _ = std::fs::remove_file(&cleanup);
    });
    path_str
}

#[test]
fn test_uhttp_stream_collects_lines() {
    let sock = start_uhttp_stream_server(200, vec!["event-a", "event-b", "event-c"]);
    let src = format!(
        "use std::uhttp\n\
         let seen = []\n\
         fn collect(line) {{ seen.push(line) }}\n\
         let status = uhttp.stream(\"unix://{sock}:/events\", collect)"
    );
    let s = run_src(&src).unwrap();
    match s.globals.get("status").unwrap() {
        VmValue::Int(n) => assert_eq!(*n, 200),
        v => panic!("expected Int, got {:?}", v),
    }
    match s.globals.get("seen").unwrap() {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let got: Vec<String> = guard
                .iter()
                .map(|v| match v {
                    VmValue::Str(s) => s.to_string(),
                    other => panic!("expected Str, got {:?}", other),
                })
                .collect();
            assert_eq!(got, vec!["event-a", "event-b", "event-c"]);
        }
        v => panic!("expected Array, got {:?}", v),
    }
}

#[test]
fn test_uhttp_stream_early_stop() {
    // Handler returns false after the first line → stream stops early.
    let sock = start_uhttp_stream_server(200, vec!["first", "second", "third"]);
    let src = format!(
        "use std::uhttp\n\
         let seen = []\n\
         fn once(line) {{\n\
             seen.push(line)\n\
             return false\n\
         }}\n\
         uhttp.stream(\"unix://{sock}:/events\", once)"
    );
    let s = run_src(&src).unwrap();
    match s.globals.get("seen").unwrap() {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            assert_eq!(guard.len(), 1, "handler returning false should stop after one line");
        }
        v => panic!("expected Array, got {:?}", v),
    }
}

#[test]
fn test_uhttp_stream_arity_error() {
    let err = try_run_src("use std::uhttp\nuhttp.stream(\"unix:///tmp/x.sock:/\")")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_uhttp_stream_handler_type_error() {
    let err = try_run_src("use std::uhttp\nuhttp.stream(\"unix:///tmp/x.sock:/\", 42)")
        .err()
        .expect("expected error");
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn test_uhttp_stream_connect_error() {
    let src = "use std::uhttp\n\
               fn noop(line) {}\n\
               uhttp.stream(\"unix:///nonexistent/jade-stream.sock:/\", noop)";
    let err = try_run_src(src).err().expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

// ── Stream muting unit tests ──────────────────────────────────────────────
//
// Tests for `drain_tokens_with_mute`. We construct a channel, push tokens,
// then run the drainer with a Vec<u8> buffer instead of stdout so we can
// assert on exactly what was printed vs what was silenced.

// start_muted=false, region_start=start, region_stop=stop
fn run_mute_region(tokens: Vec<&str>, start: Vec<&str>, stop: Vec<&str>) -> (String, String) {
    run_mute_full(tokens, false, start, stop)
}

// start_muted=true, region_start=[], region_stop=stop
fn run_mute_from_start(tokens: Vec<&str>, stop: Vec<&str>) -> (String, String) {
    run_mute_full(tokens, true, vec![], stop)
}

fn run_mute_full(
    tokens: Vec<&str>,
    start_muted: bool,
    start: Vec<&str>,
    stop: Vec<&str>,
) -> (String, String) {
    let s: Vec<String> = start.into_iter().map(|s| s.to_string()).collect();
    let e: Vec<String> = stop.into_iter().map(|s| s.to_string()).collect();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            for t in tokens {
                tx.send(t.to_string()).await.unwrap();
            }
            drop(tx);
            let mut out = Vec::<u8>::new();
            let full = drain_tokens_with_mute(&mut rx, start_muted, &s, &e, &mut out, false).await;
            let printed = String::from_utf8(out).unwrap();
            (full, printed)
        },
    )
}

#[test]
fn test_mute_no_region_prints_everything() {
    // No muting configured — every token reaches stdout.
    let (full, printed) = run_mute_region(vec!["hello ", "world"], vec![], vec![]);
    assert_eq!(full, "hello world");
    assert_eq!(printed, "hello world");
}

#[test]
fn test_mute_region_anchor_enters_permanent_mute() {
    // anchor fires → region entered, everything after suppressed (no stop_anchor).
    let (full, printed) =
        run_mute_region(vec!["<tool>", r#"{"tool_name": "x"}"#], vec!["<tool>"], vec![]);
    assert_eq!(full, r#"<tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "");
}

// The shape every other split-anchor test misses. Above, the anchor arrives
// as pure fragments ("<", "tool", ">") — no token mixes visible text with
// the start of an anchor. A real tokenizer emits word-ish pieces, so
// "Hello<" is completely ordinary, and that is the case that was broken:
// the scan buffered whole tokens and released whole tokens, so flushing
// "Hello<" as visible threw away the "<" the anchor needed. Muting then
// silently did nothing at all.
#[test]
fn test_mute_anchor_shares_a_token_with_visible_text() {
    let (full, printed) =
        run_mute_region(vec!["Hello<", "tool>", "secret"], vec!["<tool>"], vec![]);
    assert_eq!(full, "Hello<tool>secret");
    assert_eq!(printed, "Hello", "text before the anchor prints, the anchor and after do not");
}

// The same failure on the way out of a muted region: the stop anchor shares
// a token with muted text, and the visible tail after it must survive.
#[test]
fn test_mute_stop_shares_a_token_with_muted_text() {
    let (full, printed) =
        run_mute_region(vec!["a<t>", "hidden</", "t>tail"], vec!["<t>"], vec!["</t>"]);
    assert_eq!(full, "a<t>hidden</t>tail");
    assert_eq!(printed, "atail", "the muted region is dropped, the tail resumes");
}

// Both anchors straddling boundaries at once, which is the common case with
// small tokens — and the exact sequence a fake daemon chunking at 8 bytes
// produces for "ABCDEFGH<t>HIDD</t>TAIL".
#[test]
fn test_mute_both_anchors_split_across_token_boundaries() {
    let (full, printed) =
        run_mute_region(vec!["ABCDEFGH", "<t>HIDD<", "/t>TAIL"], vec!["<t>"], vec!["</t>"]);
    assert_eq!(full, "ABCDEFGH<t>HIDD</t>TAIL");
    assert_eq!(printed, "ABCDEFGHTAIL");
}

// One character per token — the worst case for a buffered scan.
#[test]
fn test_mute_one_character_per_token() {
    let (full, printed) = run_mute_region(
        "vis<t>hid</t>end"
            .chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .iter()
            .map(|s| s.as_str())
            .collect(),
        vec!["<t>"],
        vec!["</t>"],
    );
    assert_eq!(full, "vis<t>hid</t>end");
    assert_eq!(printed, "visend");
}

#[test]
fn test_mute_anchor_split_across_three_tokens() {
    // "<", "tool", ">" reassembled by buffering → anchor matched, region entered.
    let (full, printed) =
        run_mute_region(vec!["<", "tool", ">", r#"{"tool_name": "x"}"#], vec!["<tool>"], vec![]);
    assert_eq!(full, r#"<tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "");
}

#[test]
fn test_mute_bpe_merge_anchor_plus_brace() {
    // Tokenizer merged "<tool>" with "{" — anchor found at sub-token boundary,
    // region entered, remainder ("{…") suppressed.
    let (full, printed) =
        run_mute_region(vec![r#"<tool>{"#, r#""tool_name": "x"}"#], vec!["<tool>"], vec![]);
    assert_eq!(full, r#"<tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "");
}

#[test]
fn test_mute_bpe_merge_entire_tool_call_with_stop() {
    // Entire tool call as one token; anchor enters region, stop exits it, nothing after.
    let (full, printed) = run_mute_region(
        vec![r#"<tool>{"tool_name": "x"}</tool>"#],
        vec!["<tool>"],
        vec!["</tool>"],
    );
    assert_eq!(full, r#"<tool>{"tool_name": "x"}</tool>"#);
    assert_eq!(printed, "");
}

#[test]
fn test_mute_preamble_then_anchor_multi_token() {
    // Preamble prints; split anchor is reassembled, region entered, payload suppressed.
    let (full, printed) = run_mute_region(
        vec!["Sure!", "\n", "<", "tool", ">", r#"{"tool_name": "x"}"#],
        vec!["<tool>"],
        vec![],
    );
    assert_eq!(full, r#"Sure!\n<tool>{"tool_name": "x"}"#.replace("\\n", "\n"));
    assert_eq!(printed, "Sure!\n");
}

#[test]
fn test_mute_preamble_and_anchor_in_single_token() {
    // Preamble and anchor in one BPE token — preamble flushed, region entered.
    let (full, printed) = run_mute_region(
        vec![r#"Sure!\n<tool>{"tool_name": "x"}"#.replace("\\n", "\n").as_str()],
        vec!["<tool>"],
        vec![],
    );
    assert_eq!(full, "Sure!\n<tool>{\"tool_name\": \"x\"}");
    assert_eq!(printed, "Sure!\n");
}

#[test]
fn test_mute_no_anchor_in_response_prints_all() {
    // Anchor configured but never appears → prints everything.
    let (full, printed) = run_mute_region(vec!["Hello, ", "world!"], vec!["<tool>"], vec![]);
    assert_eq!(full, "Hello, world!");
    assert_eq!(printed, "Hello, world!");
}

#[test]
fn test_mute_partial_prefix_at_end_of_stream_flushes() {
    // "<too" + "k " accumulates to "<took " which is NOT a prefix of "<tool>",
    // so it flushes without entering muted mode.
    let (full, printed) = run_mute_region(vec!["<too", "k "], vec!["<tool>"], vec![]);
    assert_eq!(full, "<took ");
    assert_eq!(printed, "<took ");
}

#[test]
fn test_mute_incomplete_stop_at_end_of_stream_suppressed() {
    // Daemon stopped mid-"</tool>" sending only "</". We're inside a muted
    // region (after "<tool>"), so the partial stop is just discarded.
    let (full, printed) =
        run_mute_region(vec!["<tool>", "payload", "</"], vec!["<tool>"], vec!["</tool>"]);
    assert_eq!(full, "<tool>payload</");
    assert_eq!(printed, "");
}

#[test]
fn test_mute_preamble_partial_prefix_then_no_anchor() {
    // "< " is a prefix of "<tool>" briefly, but "price < today" never completes it.
    let (full, printed) = run_mute_region(vec!["price < today"], vec!["<tool>"], vec![]);
    assert_eq!(full, "price < today");
    assert_eq!(printed, "price < today");
}

// ── Gap: empty stream ────────────────────────────────────────────────────

#[test]
fn test_mute_empty_stream() {
    // Zero tokens — function must return empty strings without hanging.
    let (full, printed) = run_mute_region(vec![], vec!["<tool>"], vec![]);
    assert_eq!(full, "");
    assert_eq!(printed, "");
}

#[test]
fn test_mute_empty_stream_no_region() {
    let (full, printed) = run_mute_region(vec![], vec![], vec![]);
    assert_eq!(full, "");
    assert_eq!(printed, "");
}

// ── Gap: newline=true path ────────────────────────────────────────────────

#[test]
fn test_mute_newline_appended_to_printed() {
    // newline=true must append '\n' to the printed output.
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tx.send("hello".to_string()).await.unwrap();
        drop(tx);
        let mut out = Vec::<u8>::new();
        let full = drain_tokens_with_mute(&mut rx, false, &[], &[], &mut out, true).await;
        let printed = String::from_utf8(out).unwrap();
        assert_eq!(full, "hello");
        assert_eq!(printed, "hello\n");
    });
}

#[test]
fn test_mute_newline_not_appended_when_false() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tx.send("hello".to_string()).await.unwrap();
        drop(tx);
        let mut out = Vec::<u8>::new();
        drain_tokens_with_mute(&mut rx, false, &[], &[], &mut out, false).await;
        let printed = String::from_utf8(out).unwrap();
        assert_eq!(printed, "hello");
    });
}

// ── Gap: additional anchor split boundaries ───────────────────────────────

#[test]
fn test_mute_anchor_split_two_tokens_midpoint() {
    // "<too" + "l>" reassembled → anchor matched, region entered, payload suppressed.
    let (full, printed) =
        run_mute_region(vec!["<too", "l>", r#"{"tool_name": "x"}"#], vec!["<tool>"], vec![]);
    assert_eq!(full, r#"<tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "");
}

#[test]
fn test_mute_anchor_split_as_name_then_close() {
    // "<tool" + ">" — different 2-token split; same result.
    let (full, printed) =
        run_mute_region(vec!["<tool", ">", r#"{"tool_name": "x"}"#], vec!["<tool>"], vec![]);
    assert_eq!(full, r#"<tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "");
}

// ── Multiple region_start triggers ───────────────────────────────────────

#[test]
fn test_mute_multiple_literals_first_fires() {
    // Two possible anchors; the first one matched enters the muted region.
    let (full, printed) = run_mute_region(
        vec!["think: ", "<tool>", r#"{"tool_name": "x"}"#],
        vec!["<tool>", "<call>"],
        vec![],
    );
    assert_eq!(full, r#"think: <tool>{"tool_name": "x"}"#);
    assert_eq!(printed, "think: ");
}

#[test]
fn test_mute_multiple_literals_second_fires() {
    // Two possible anchors; the second one fires in this response.
    let (full, printed) = run_mute_region(
        vec!["think: ", "<call>", r#"{"tool_name": "x"}"#],
        vec!["<tool>", "<call>"],
        vec![],
    );
    assert_eq!(full, r#"think: <call>{"tool_name": "x"}"#);
    assert_eq!(printed, "think: ");
}

// ── Region with stop_anchor: text after stop prints ───────────────────────

#[test]
fn test_mute_region_stop_exits_and_trailing_prints() {
    // Region between <tool> and </tool> suppressed; text after </tool> prints.
    let (full, printed) = run_mute_region(
        vec!["<tool>", r#"{"tool_name": "x"}"#, "</tool>", "more"],
        vec!["<tool>"],
        vec!["</tool>"],
    );
    assert_eq!(full, r#"<tool>{"tool_name": "x"}</tool>more"#);
    assert_eq!(printed, "more");
}

#[test]
fn test_mute_from_start_stop_anchor_exits_then_prints() {
    // start_muted=true: suppressed from token 1 until stop_anchor; rest prints.
    let (full, printed) =
        run_mute_from_start(vec!["reasoning", "</think>", "answer"], vec!["</think>"]);
    assert_eq!(full, "reasoning</think>answer");
    assert_eq!(printed, "answer");
}

#[test]
fn test_mute_from_start_no_stop_suppresses_all() {
    // start_muted=true, no stop → entire response suppressed.
    let (full, printed) = run_mute_from_start(vec!["all", "suppressed"], vec![]);
    assert_eq!(full, "allsuppressed");
    assert_eq!(printed, "");
}

// ── Gap: Grammar.new with no anchor suppresses from start ──────────────────

#[test]
fn test_mute_grammar_no_anchor_suppresses_from_start() {
    // No anchor → start_muted=true. Return value still has full text.
    let s = run_src_with_mock(
        r#"prompt p = "test"
let g = Grammar.new("root ::= [a-z]+")
let reply = ?p |> g"#,
        vec!["hello world"],
    )
    .unwrap();
    assert_eq!(get_str(&s, "reply"), "hello world");
}

// ── Return-value correctness (no stdout needed) ───────────────────────────

#[test]
fn test_mute_stream_returns_full_text_via_vm() {
    // stream() must return the complete text (including muted spans) so callers
    // can parse structured data from it.
    let s = run_src_with_mock(
        r#"prompt p = "test"
let g = Grammar.new("root ::= \"{\" [a-z]+ \"}\"", "<tool>")
let reply = ?p |> g"#,
        vec![r#"<tool>{"tool_name": "x"}"#],
    )
    .unwrap();
    assert_eq!(get_str(&s, "reply"), r#"<tool>{"tool_name": "x"}"#);
}

#[test]
fn test_mute_stream_returns_full_text_with_preamble_via_vm() {
    let s = run_src_with_mock(
        r#"prompt p = "test"
let g = Grammar.new("root ::= \"{\" [a-z]+ \"}\"", "<tool>")
let reply = ?p |> g"#,
        vec![r#"Sure thing!<tool>{"tool_name": "x"}"#],
    )
    .unwrap();
    assert_eq!(get_str(&s, "reply"), r#"Sure thing!<tool>{"tool_name": "x"}"#);
}

// ── Stdout capture: mute_on= region and point suppression ─────────────────
//
// Mute source rule:
//   Grammar has anchor  → anchor enters region mute; stop_anchor exits it;
//                         everything from anchor to stop_anchor is suppressed.
//                         If no stop_anchor, muting is permanent to EOS.
//   Grammar has no anchor → quoted literals from the pattern are point filters
//                         (non-permanent; each occurrence suppressed, text
//                         between occurrences prints normally).
//
// MockBackend sends the whole response as one token (worst-case BPE scenario).

#[test]
fn test_mute_grammar_anchor_suppresses_entire_region() {
    // anchor = "<tool>", no stop_anchor → everything from "<tool>" to EOS is suppressed.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let g = Grammar.new("root ::= \"{\" [a-z]+ \"}\"", "<tool>")
print(?p |> g)"#,
        vec![r#"<tool>{"tool_name": "x"}"#],
    )
    .unwrap();
    assert_eq!(printed, "\n");
}

#[test]
fn test_mute_grammar_preamble_printed_anchor_suppressed() {
    // Preamble before anchor prints; anchor enters region mute → payload suppressed.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let g = Grammar.new("root ::= \"{\" [a-z]+ \"}\"", "<tool>")
print(?p |> g)"#,
        vec![r#"Sure thing!<tool>{"tool_name": "x"}"#],
    )
    .unwrap();
    assert_eq!(printed, "Sure thing!\n");
}

#[test]
fn test_mute_no_mute_on_kwarg_prints_everything() {
    // An unconstrained dereference has no anchors, so nothing is suppressed.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
print(?p)"#,
        vec!["hello world"],
    )
    .unwrap();
    assert_eq!(printed, "hello world\n");
}

#[test]
fn test_mute_grammar_full_tool_gbnf_anchor_and_stop_suppressed() {
    // Complex GBNF + anchor + stop_anchor: anchor enters region mute, stop_anchor
    // exits it — entire region (anchor, payload, stop_anchor) is suppressed.
    let gbnf = r#"root   ::= "{" ws toolkv (ws "," ws pair)* ws "}"
toolkv ::= [\x22] "tool_name" [\x22] ws ":" ws str
pair   ::= str ws ":" ws val
val    ::= str | num | "true" | "false" | "null"
str    ::= [\x22] [^\x22]* [\x22]
num    ::= "-"? [0-9]+ ("." [0-9]+)?
ws     ::= [ ]*"#;
    let src = format!(
        r#"let g = Grammar.new("{gbnf}", "<tool>", "</tool>")
prompt p = "test"
print(?p |> g)"#,
        gbnf = gbnf.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let (_s, printed) = run_src_with_stdout_capture(
        &src,
        vec![r#"<tool>{"tool_name": "get_weather", "city": "Paris"}</tool>"#],
    )
    .unwrap();
    // "<tool>" enters region mute, "</tool>" exits it; entire region suppressed, nothing prints.
    assert_eq!(printed, "\n");
}

// ── No-anchor grammar: suppress from start of generation ─────────────────

/// A stream is a buffer, so reading one twice gives the same text twice.
///
/// Before v1.2.4 the receiver was taken on first drain and a second read raised
/// `DoubleStreamDrain`. The array is what keeps the value a stream: binding it
/// with `let` drains it to a string at the store, but a container holds the
/// stream itself. The mock supplies one reply, which is the point — the second
/// read must not start a second inference.
#[test]
fn a_prompt_stream_can_be_read_twice() {
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let a = [?p]
print(a[0])
print(a[0])"#,
        vec!["once"],
    )
    .unwrap();
    assert_eq!(printed, "once\nonce\n");
}

/// The same stream printed once and then used as a value. Both drain paths have
/// to agree about the buffer, or the second read comes back empty.
#[test]
fn a_printed_prompt_stream_is_still_readable_as_a_value() {
    let s = run_src_with_mock(
        r#"prompt p = "test"
let a = [?p]
print(a[0])
let t = a[0]
let n = len(t)"#,
        vec!["abcd"],
    )
    .unwrap();
    assert_eq!(get_int(&s, "n"), 4);
}

#[test]
fn test_mute_no_anchor_suppresses_entire_response() {
    // No anchor, no stop_anchor → start_muted=true, permanent → nothing prints.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let g = Grammar.new("\"<think>\" | \"</think>\"")
print(?p |> g)"#,
        vec!["<think>some reasoning</think>final answer"],
    )
    .unwrap();
    assert_eq!(printed, "\n");
}

#[test]
fn test_mute_no_anchor_with_stop_anchor_prints_after() {
    // No anchor → start muted; stop_anchor "</think>" exits muted mode; "answer" prints.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let g = Grammar.new("root ::= [a-z]+", nil, "</think>")
print(?p |> g)"#,
        vec!["reasoning</think>answer"],
    )
    .unwrap();
    assert_eq!(printed, "answer\n");
}

#[test]
fn test_mute_grammar_no_anchor_regex_suppresses_everything() {
    // Regex-only pattern, no anchor → start_muted=true → all tokens suppressed.
    let (_s, printed) = run_src_with_stdout_capture(
        r#"prompt p = "test"
let g = Grammar.new("root ::= [a-z]+")
print(?p |> g)"#,
        vec!["hello world"],
    )
    .unwrap();
    assert_eq!(printed, "\n");
}

// ── Constrained lazy inference: stop_anchor reaches the backend ──────────────
//
// These tests verify that when `?p |> g` is evaluated with a
// Grammar that has a stop_anchor, the inference request sent to the backend
// carries that stop_anchor — i.e. the lazy stream starts with constraints.
//
// A helper that shares the Arc<MockBackend> with the test so we can inspect
// what InferenceRequest was actually sent after the run.

fn run_src_with_shared_backend(
    src: &str,
    backend: std::sync::Arc<crate::llm::MockBackend>,
) -> Result<VmState> {
    let tokens = lexer::tokenize(src).expect("lex failed");
    let program = parser::parse(tokens).expect("parse failed");
    let tprogram = type_infer::infer(program).expect("type inference failed");
    let compiled = emit::emit(tprogram).expect("emit failed");
    let opts = VmOpts {
        backend: Some(
            std::sync::Arc::clone(&backend) as std::sync::Arc<dyn crate::llm::InferenceBackend>
        ),
        #[cfg(test)]
        test_stdout: None,
        ..VmOpts::default()
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run(compiled, opts))
}

#[test]
fn test_stream_with_grammar_passes_stop_anchor_to_backend() {
    // Verify that `?p |> g` sends stop_anchor="</tool>" to the
    // inference backend rather than None — this is what prevents the model loop.
    let backend = std::sync::Arc::new(crate::llm::MockBackend::new(vec![
        r#"<tool>{"tool_name": "x"}</tool>"#,
    ]));
    let src = r#"prompt p = "test"
let g = Grammar.new("root ::= \"{\" [a-z]+ \"}\"", "<tool>", "</tool>")
let reply = ?p |> g"#;
    run_src_with_shared_backend(src, std::sync::Arc::clone(&backend)).unwrap();
    let captured = backend.captured.lock().unwrap();
    assert_eq!(captured.len(), 1, "exactly one inference call expected");
    assert_eq!(captured[0].stop_anchor.as_deref(), Some("</tool>"));
    assert_eq!(captured[0].anchor.as_deref(), Some("<tool>"));
}

#[test]
fn test_stream_with_grammar_no_stop_anchor_passes_none() {
    // Grammar.new with only anchor (no stop_anchor) → stop_anchor is None in request.
    let backend = std::sync::Arc::new(crate::llm::MockBackend::new(vec!["hello"]));
    let src = r#"prompt p = "test"
let g = Grammar.new("root ::= [a-z]+", "<tool>")
let reply = ?p |> g"#;
    run_src_with_shared_backend(src, std::sync::Arc::clone(&backend)).unwrap();
    let captured = backend.captured.lock().unwrap();
    assert_eq!(captured[0].stop_anchor, None);
    assert_eq!(captured[0].anchor.as_deref(), Some("<tool>"));
}

#[test]
fn test_stream_no_mute_on_passes_no_constraints() {
    // A bare `?p` → the backend receives no grammar/anchor/stop.
    let backend = std::sync::Arc::new(crate::llm::MockBackend::new(vec!["hello"]));
    let src = "prompt p = \"test\"\nlet reply = ?p";
    run_src_with_shared_backend(src, std::sync::Arc::clone(&backend)).unwrap();
    let captured = backend.captured.lock().unwrap();
    assert_eq!(captured[0].grammar, None);
    assert_eq!(captured[0].anchor, None);
    assert_eq!(captured[0].stop_anchor, None);
}

#[test]
fn test_prompt_deref_outside_stream_passes_no_constraints() {
    // `let x = ?p` (not inside stream) → lazy start with no constraints.
    let backend = std::sync::Arc::new(crate::llm::MockBackend::new(vec!["hello"]));
    let src = "prompt p = \"test\"\nlet x = ?p";
    run_src_with_shared_backend(src, std::sync::Arc::clone(&backend)).unwrap();
    let captured = backend.captured.lock().unwrap();
    assert_eq!(captured[0].grammar, None);
    assert_eq!(captured[0].stop_anchor, None);
}

// Two tests here asserted that every request carried `keep_anchors: false` and an
// empty `model` — the language had stopped setting either, but the wire type still
// had the fields, so nothing stopped a caller from filling them back in. Both
// fields are gone from `InferenceRequest` now, along with the socket that needed
// them, so the type says what the tests used to check.

// ── break and continue ────────────────────────────────────────────────────

#[test]
fn break_leaves_a_for_loop() {
    let s = run_src(
        "let mut_t = 0\nfor i in [1, 2, 3, 4] {\n if i == 3 { break }\n mut_t = mut_t + i\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_t"), 3);
}

#[test]
fn continue_skips_the_rest_of_a_for_body() {
    let s = run_src(
        "let mut_t = 0\nfor i in [1, 2, 3, 4] {\n if i == 2 { continue }\n mut_t = mut_t + i\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_t"), 8);
}

#[test]
fn continue_in_a_for_loop_still_advances_the_index() {
    // Landing at the top of the loop instead of at the increment would never
    // advance `idx`, and the loop would hang rather than fail.
    let s =
        run_src("let mut_n = 0\nfor i in [1, 2, 3] {\n mut_n = mut_n + 1\n continue\n}\n").unwrap();
    assert_eq!(get_int(&s, "mut_n"), 3);
}

#[test]
fn break_leaves_a_while_loop() {
    let s =
        run_src("let mut_n = 0\nwhile true {\n mut_n = mut_n + 1\n if mut_n == 5 { break }\n}\n")
            .unwrap();
    assert_eq!(get_int(&s, "mut_n"), 5);
}

#[test]
fn continue_in_a_while_loop_reruns_the_condition() {
    let s = run_src("let mut_n = 0\nlet mut_t = 0\nwhile mut_n < 5 {\n mut_n = mut_n + 1\n if mut_n == 3 { continue }\n mut_t = mut_t + mut_n\n}\n").unwrap();
    assert_eq!(get_int(&s, "mut_t"), 12);
}

#[test]
fn break_leaves_only_the_innermost_loop() {
    let s = run_src(
        "let mut_t = 0\nfor a in [1, 2, 3] {\n for b in [1, 2, 3] {\n  if b == 2 { break }\n  mut_t = mut_t + 1\n }\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_t"), 3);
}

#[test]
fn break_works_inside_a_function_body() {
    let s = run_src("fn f(xs) {\n let mut_t = 0\n for x in xs {\n  if x > 2 { break }\n  mut_t = mut_t + x\n }\n return mut_t\n}\nlet v = f([1, 2, 3, 4])\n").unwrap();
    assert_eq!(get_int(&s, "v"), 3);
}

#[test]
fn break_from_a_catch_arm_leaves_the_loop() {
    // The loop-until-it-raises shape. A C binding whose `fails_when` turns an
    // end-of-input code into an exception makes this the natural way to read
    // until the library says stop.
    let s = run_src(
        "fn step(n) {\n if n > 3 { raise \"EOF\" }\n return n\n}\n\
         let mut_seen = 0\nlet mut_i = 0\n\
         while true {\n mut_i = mut_i + 1\n try {\n  mut_seen = mut_seen + step(mut_i)\n } catch e {\n  break\n }\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_seen"), 6);
}

#[test]
fn breaking_out_of_a_try_body_pops_its_handler() {
    // Leaving by a jump skips the `PopHandler` the normal exit runs. If the
    // frame stayed installed it would point into code the loop has already
    // left, and the *next* raise anywhere in the function would land there.
    let s = run_src(
        "let mut_v = 0\nfor i in [1, 2, 3] {\n try {\n  if i == 2 { break }\n } catch e {\n  mut_v = 99\n }\n}\n\
         try {\n raise \"after\"\n} catch e {\n mut_v = 7\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_v"), 7);
}

#[test]
fn continue_out_of_a_try_body_pops_its_handler_too() {
    let s = run_src(
        "let mut_t = 0\nfor i in [1, 2, 3, 4] {\n try {\n  if i == 2 { continue }\n  mut_t = mut_t + i\n } catch e {\n  mut_t = 99\n }\n}\n\
         try {\n raise \"after\"\n} catch e {\n mut_t = mut_t + 100\n}\n",
    )
    .unwrap();
    assert_eq!(get_int(&s, "mut_t"), 108);
}

#[test]
fn break_outside_a_loop_is_refused() {
    let err = parser::parse(lexer::tokenize("break\n").unwrap()).expect_err("should refuse");
    assert!(matches!(err, crate::frontend::error::JadeError::BreakOutsideLoop { .. }), "{err:?}");
}

#[test]
fn continue_outside_a_loop_is_refused() {
    let err = parser::parse(lexer::tokenize("continue\n").unwrap()).expect_err("should refuse");
    assert!(
        matches!(err, crate::frontend::error::JadeError::ContinueOutsideLoop { .. }),
        "{err:?}"
    );
}

#[test]
fn a_loop_outside_the_function_is_not_a_loop_the_body_can_break_out_of() {
    // The jump would have to cross a call frame, which is a `return`, not a
    // `break`.
    let src = "for i in [1] {\n fn f() { break }\n}\n";
    let err = parser::parse(lexer::tokenize(src).unwrap()).expect_err("should refuse");
    assert!(matches!(err, crate::frontend::error::JadeError::BreakOutsideLoop { .. }), "{err:?}");
}

#[test]
fn a_closure_cannot_break_out_of_the_loop_that_encloses_it() {
    let src = "for i in [1] {\n let g = || { break }\n}\n";
    let err = parser::parse(lexer::tokenize(src).unwrap()).expect_err("should refuse");
    assert!(matches!(err, crate::frontend::error::JadeError::BreakOutsideLoop { .. }), "{err:?}");
}

#[test]
fn int_of_a_char_is_its_scalar() {
    // What makes a fixed-size C field readable: `char mnemonic[32]` arrives as
    // thirty-two characters, NUL padding included, and this is how a program
    // finds where the text stops. Nothing trims it, because trimming guesses.
    let st = run_src(
        "let a = int(char(\"j\"))\nlet b = int(char(\"\u{4e2d}\"))\nlet c = int(char(0))\n",
    )
    .expect("should run");
    assert_eq!(get_int(&st, "a"), 106);
    assert_eq!(get_int(&st, "b"), 20013);
    assert_eq!(get_int(&st, "c"), 0);
}

#[test]
fn char_of_an_int_is_refused_when_it_is_not_a_character() {
    // The surrogate range and anything past U+10FFFF are not characters.
    // Replacing them silently would corrupt what the conversion claims to do.
    let st = run_src("let c = char(106)\n").expect("should run");
    assert_eq!(get_char(&st, "c"), 'j');

    for bad in ["1114112", "55296"] {
        let err = match run_src(&format!("let c = char({bad})\n")) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("char({bad}) should have been refused"),
        };
        assert!(err.contains("not a Unicode scalar"), "char({bad}) should be refused: {err}");
    }
}
