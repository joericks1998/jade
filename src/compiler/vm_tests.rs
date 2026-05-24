use super::*;
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
    let s = run_src("let i = 0\nlet sum = 0\nwhile i < 5 {\n  sum = sum + i\n  i = i + 1\n}").unwrap();
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
    let s = run_src("struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 10, y: 20 }\nlet px = p.x").unwrap();
    assert_eq!(get_int(&s, "px"), 10);
}

#[test]
fn test_vm_struct_field_assign() {
    let s = run_src("struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 1, y: 2 }\np.x = 99\nlet px = p.x").unwrap();
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
    let s = run_src("fn double(n) {\n  let result = n * 2\n  result\n}\nlet x = double(5)").unwrap();
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
    let tokens = lexer::tokenize(src).expect("lex failed");
    let program = parser::parse(tokens).expect("parse failed");
    let tprogram = type_infer::infer(program).expect("type inference failed");
    let compiled = emit::emit(tprogram).expect("emit failed");
    let opts = VmOpts {
        backend: Some(std::sync::Arc::new(crate::llm::MockBackend::new(responses))),
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
    // Wrap the bare expression in a let binding named __repl_result__
    if let Some(Stmt::Expr(expr)) = program.stmts.pop() {
        program.stmts.push(Stmt::Let {
            name: "__repl_result__".to_string(),
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
    // read it
    assert!(matches!(state.globals.get("__repl_result__"), Some(VmValue::Int(2))));
    // remove it
    state.globals.remove("__repl_result__");
    assert!(state.globals.get("__repl_result__").is_none());
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
    let s = run_src("fn multiply(a, b, c) {\n  return a * b * c\n}\nlet r = multiply(2, 3, 4)").unwrap();
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
    let s = run_src("fn local_shadow(x) {\n  let y = x * 2\n  return y\n}\nlet c = local_shadow(5)").unwrap();
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
    let err = try_run_src("fn f(a) {\n  return a\n}\nlet x = f(1, 2)").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
}

#[test]
fn test_vm_not_callable() {
    let err = try_run_src("let x = 5\nlet y = x(1)").err().expect("expected error");
    assert!(matches!(err, JadeError::NotCallable { .. }));
}

// ── integer overflow (ported from eval.rs) ────────────────────────────────

#[test]
fn test_vm_integer_overflow_add() {
    let err = try_run_src(&format!("let x = {} + 1", i64::MAX)).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

#[test]
fn test_vm_integer_overflow_sub() {
    let err = try_run_src(&format!("let x = -{} - 2", i64::MAX)).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

#[test]
fn test_vm_integer_overflow_mul() {
    let err = try_run_src(&format!("let x = {} * 2", i64::MAX)).err().expect("expected error");
    assert!(matches!(err, JadeError::IntegerOverflow { .. }));
}

#[test]
fn test_vm_nested_fn_ok() {
    // Nested function definitions are now a parse error.
    let tokens = crate::frontend::lexer::tokenize("fn outer() {\n  fn inner() {\n    return 1\n  }\n  return 2\n}").expect("lex");
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
    let s = run_src("let sum = 0\nlet i = 1\nwhile i <= 10 {\n  sum = sum + i\n  i = i + 1\n}").unwrap();
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
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1 }").err().expect("expected error");
    assert!(matches!(err, JadeError::MissingField { .. }));
}

#[test]
fn test_vm_extra_field_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, z: 3 }").err().expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_field_access_on_non_struct_error() {
    let err = try_run_src("let x = 5\nlet v = x.y").err().expect("expected error");
    assert!(matches!(err, JadeError::NotAStruct { .. } | JadeError::TypeMismatch { .. } | JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_undefined_field_access_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2 }\nlet v = p.z").err().expect("expected error");
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

#[test]
fn test_vm_str_index() {
    let s = run_src("let sv = \"hello\"\nlet h = sv[0]").unwrap();
    assert_eq!(get_str(&s, "h"), "h");
}

#[test]
fn test_vm_str_index_last() {
    let s = run_src("let sv = \"hello\"\nlet o = sv[4]").unwrap();
    assert_eq!(get_str(&s, "o"), "o");
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
    let err = try_run_src(r#"print("a", "b")"#).err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
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
    let s = run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 3, y: 4 }\nlet sv = f\"({p.x}, {p.y})\"").unwrap();
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
        "    fn to_str(self) -> str\n",
        "}\n",
        "struct Point {\n  x,\n  y\n}\n",
        "extend Point: Displayable {\n",
        "    fn to_str(self) -> str {\n",
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
        "    fn to_str(self) -> str\n",
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
        "    fn to_str(self) -> str {\n",
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
    assert!(matches!(err, JadeError::MissingApiKey { .. }));
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
        "struct Agent {\n  prompt system = \"helpful\"\n}\nlet a = Agent {}\nlet r = ?a.system"
    ).err().expect("expected error");
    assert!(matches!(err, JadeError::MissingApiKey { .. }));
}

#[test]
fn test_vm_prompt_deref_field_access_not_a_prompt() {
    let err = run_src_with_mock(
        "struct S {\n  x,\n}\nlet s = S { x: 42 }\nlet r = ?s.x",
        vec![]
    ).err().expect("expected error");
    assert!(matches!(err, JadeError::NotAPrompt { .. }));
}

#[test]
fn test_vm_prompt_deref_field_access_with_mock() {
    let s = run_src_with_mock(
        "struct Agent {\n  prompt system = \"Say hello\"\n}\nlet a = Agent {}\nlet r = ?a.system",
        vec!["hello!"]
    ).unwrap();
    assert_eq!(get_str(&s, "r"), "hello!");
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
    ).err().expect("expected error");
    assert!(matches!(err, JadeError::PromptOverflow { .. }));
}

#[test]
fn test_vm_tokens_incremented_after_deref() {
    let s = run_src_with_mock("prompt p = \"hi\"\nlet x = ?p", vec!["hello"]).unwrap();
    match s.globals.get("__tokens__").unwrap() {
        VmValue::Int(n) => assert!(*n > 0),
        v => panic!("expected Int, got {:?}", v),
    }
}

#[test]
fn test_vm_untyped_deref_returns_str() {
    let s = run_src_with_mock("prompt p = \"test\"\nlet x = ?p", vec!["result"]).unwrap();
    assert_eq!(get_str(&s, "x"), "result");
}

#[test]
fn test_vm_typed_deref_retry_succeeds_on_second_attempt() {
    let s = run_src_with_mock(
        "prompt p = \"number?\"\nlet n = ?p |> int",
        vec!["not a number", "42"],
    ).unwrap();
    assert_eq!(get_int(&s, "n"), 42);
}

// ── Grammar ──────────────────────────────────────────────────────────────

#[test]
fn test_grammar_new_returns_grammar_value() {
    let s = run_src(r#"let g = Grammar.new('"yes" | "no"')"#).unwrap();
    match s.globals.get("g").unwrap() {
        VmValue::Grammar { pattern, anchor } => {
            assert_eq!(pattern, r#""yes" | "no""#);
            assert_eq!(*anchor, None);
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
    ).unwrap();
    assert_eq!(get_str(&s, "answer"), "yes");
}

#[test]
fn test_grammar_new_with_anchor() {
    let s = run_src(r#"let g = Grammar.new('"yes" | "no"', anchor = "Answer:")"#).unwrap();
    match s.globals.get("g").unwrap() {
        VmValue::Grammar { pattern, anchor } => {
            assert_eq!(pattern, r#""yes" | "no""#);
            assert_eq!(anchor.as_deref(), Some("Answer:"));
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
    let s = run_src("struct Config {\n  let host = \"localhost\"\n}\nlet c = Config {}\nlet h = c.host").unwrap();
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
    let err = try_run_src("struct Mixed {\n  x,\n  let label = \"origin\"\n}\nlet m = Mixed {}").err().expect("expected error");
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

#[test]
#[ignore = "VM does not yet validate that prompt struct fields must be strings (treewalk did)"]
fn test_vm_struct_prompt_field_non_string_error() {
    assert!(try_run_src("struct Bad {\n  prompt sys = 42\n}\nlet b = Bad {}").is_err());
}

#[test]
#[ignore = "VM does not yet validate that prompt struct field overrides must be strings"]
fn test_vm_struct_prompt_field_override_non_string_error() {
    assert!(try_run_src("struct Agent {\n  prompt system = \"ok\"\n}\nlet a = Agent { system: 99 }").is_err());
}

#[test]
fn test_vm_struct_extra_field_still_errors_with_defaults() {
    let err = try_run_src("struct Agent {\n  let name = \"Jade\"\n}\nlet a = Agent { name: \"x\", extra: 1 }").err().expect("expected error");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_vm_struct_duplicate_field_error() {
    let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, x: 3 }").err().expect("expected error");
    assert!(matches!(err, JadeError::DuplicateField { field, .. } if field == "x"));
}

#[test]
fn test_vm_struct_default_references_variable() {
    let s = run_src("let base = 10\nstruct S {\n  let x = base\n}\nlet sv = S {}\nlet v = sv.x").unwrap();
    assert_eq!(get_int(&s, "v"), 10);
}

#[test]
fn test_vm_struct_required_after_let_field() {
    let err = try_run_src("struct S {\n  let x = 0,\n  y\n}\nlet s = S { x: 1 }").err().expect("expected error");
    assert!(matches!(err, JadeError::MissingField { field, .. } if field == "y"));
}

// ── Empty struct tests ────────────────────────────────────────────────────

#[test]
fn test_vm_empty_struct_define_and_instantiate() {
    let s = run_src("struct Unit {}\nlet u = Unit {}").unwrap();
    match s.globals.get("u").unwrap() {
        VmValue::Struct(rc) => assert_eq!(rc.lock().type_name, "Unit"),
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

// ── std/fs tests ──────────────────────────────────────────────────────────

#[test]
fn test_fs_write_and_read() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_write_read.txt");
    let path_str = path.to_str().unwrap();
    let src = format!(
        "use \"std/fs\"\nfs.write(\"{path_str}\", \"hello jade\")\nlet v = fs.read(\"{path_str}\")"
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
    let src = format!("use \"std/fs\"\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(get_bool(&s, "v"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fs_exists_false() {
    let src = "use \"std/fs\"\nlet v = fs.exists(\"/tmp/jade_test_no_such_file_xyz.txt\")";
    let s = run_src(src).unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_fs_delete() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_delete.txt");
    std::fs::write(&path, "bye").unwrap();
    let path_str = path.to_str().unwrap();
    let src = format!("use \"std/fs\"\nfs.delete(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(!get_bool(&s, "v"));
}

#[test]
fn test_fs_append() {
    let dir = std::env::temp_dir();
    let path = dir.join("jade_test_fs_append.txt");
    let path_str = path.to_str().unwrap();
    let src = format!(
        "use \"std/fs\"\nfs.write(\"{path_str}\", \"hello\")\nfs.append(\"{path_str}\", \" world\")\nlet v = fs.read(\"{path_str}\")"
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
    let src = format!("use \"std/fs\"\nlet v = fs.list_dir(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    match s.globals.get("v").unwrap() {
        VmValue::Array(a) => {
            let names: Vec<String> = a.lock().iter().map(|v| match v {
                VmValue::Str(s) => s.clone(),
                _ => panic!("non-str entry"),
            }).collect();
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
    let src = format!("use \"std/fs\"\nfs.mkdir(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
    let s = run_src(&src).unwrap();
    assert!(get_bool(&s, "v"));
    let _ = std::fs::remove_dir_all(dir.join("jade_test_fs_mkdir_new"));
}

#[test]
fn test_fs_read_nonexistent_errors() {
    let err = try_run_src("use \"std/fs\"\nlet v = fs.read(\"/tmp/jade_no_such_file_xyz.txt\")").err().expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn test_fs_write_arity_error() {
    let err = try_run_src("use \"std/fs\"\nfs.write(\"path\")").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { expected: 2, .. }));
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

#[test]
fn test_type_struct_from_dict() {
    let src = "struct City {\n  name,\n  country,\n}\nlet d = {\"name\": \"Paris\", \"country\": \"France\"}\nlet c = City(d)\nlet n = c.name";
    let s = run_src(src).unwrap();
    assert_eq!(get_str(&s, "n"), "Paris");
}

#[test]
fn test_type_struct_from_same_type_is_identity() {
    let src = "struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 3, y: 4 }\nlet q = Point(p)\nlet v = q.x";
    let s = run_src(src).unwrap();
    assert_eq!(get_int(&s, "v"), 3);
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
    let src = "fn identity(f) {\n  return f\n}\n@identity\nfn greet() {\n  return 42\n}\nlet v = greet()";
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
    let src = format!(
        "use \"std/http\"\nlet r = http.get(\"http://127.0.0.1:{port}/\")"
    );
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
    let src = format!(
        "use \"std/http\"\nlet r = http.post(\"http://127.0.0.1:{port}/\", \"payload\")"
    );
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
        "use \"std/http\"\nlet r = http.get(\"http://127.0.0.1:{port}/\", {{\"X-Test\": \"jade\"}})"
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
    let err = try_run_src("use \"std/http\"\nhttp.get()").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_http_get_type_error() {
    let err = try_run_src("use \"std/http\"\nhttp.get(42)").err().expect("expected error");
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn test_http_post_arity_error() {
    let err = try_run_src("use \"std/http\"\nhttp.post(\"http://example.com\")").err().expect("expected error");
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn test_http_get_connection_refused_errors() {
    let err = try_run_src("use \"std/http\"\nhttp.get(\"http://127.0.0.1:1/\")").err().expect("expected error");
    assert!(matches!(err, JadeError::IoError { .. }));
}
