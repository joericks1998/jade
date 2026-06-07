use super::*;
use crate::frontend::{lexer, parser};

fn infer_src(src: &str) -> Result<TProgram> {
    let tokens = lexer::tokenize(src).expect("lex");
    let program = parser::parse(tokens).expect("parse");
    infer(program)
}

fn infer_ok(src: &str) -> TProgram {
    infer_src(src).expect("type check failed")
}

fn infer_err(src: &str) -> JadeError {
    infer_src(src).expect_err("expected type error")
}

// ── Literals ──────────────────────────────────────────────────────────────

#[test]
fn test_infer_int_literal() {
    let tp = infer_ok("let x = 42");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

#[test]
fn test_infer_float_literal() {
    let tp = infer_ok("let x = 3.14");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Float);
}

#[test]
fn test_infer_bool_literal() {
    let tp = infer_ok("let x = true");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Bool);
}

#[test]
fn test_infer_str_literal() {
    let tp = infer_ok(r#"let x = "hello""#);
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Str);
}

#[test]
fn test_infer_nil_literal() {
    let tp = infer_ok("let x = nil");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Nil);
}

#[test]
fn test_infer_null_literal() {
    // `null` is a third spelling of nil (alongside `None`); all infer as JadeType::Nil
    for src in ["let x = null", "let x = None", "let x = nil"] {
        let tp = infer_ok(src);
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Nil, "failed for: {src}");
    }
}

#[test]
fn test_infer_bool_nil() {
    // bool(nil) must pass type inference with nil recognized as JadeType::Nil.
    let tp = infer_ok("let x = bool(nil)");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    // The conversion constructors int/float/bool/str now infer their concrete
    // result type (so AOT codegen can format e.g. bool → "true"/"false" instead
    // of the Unknown→1/0 path); `bool(_)` is therefore Bool, not Unknown.
    assert_eq!(value.ty, JadeType::Bool);
}

// ── Arithmetic ────────────────────────────────────────────────────────────

#[test]
fn test_infer_int_add() {
    let tp = infer_ok("let x = 1 + 2");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

#[test]
fn test_infer_float_add() {
    let tp = infer_ok("let x = 1.0 + 2.0");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Float);
}

#[test]
fn test_infer_mixed_add_promotes_to_float() {
    let tp = infer_ok("let x = 1 + 2.0");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Float);
}

#[test]
fn test_infer_str_concat() {
    let tp = infer_ok(r#"let x = "a" + "b""#);
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Str);
}

#[test]
fn test_infer_int_plus_str_is_error() {
    let err = infer_err(r#"let x = 1 + "a""#);
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

// ── Logical and comparison ─────────────────────────────────────────────────

#[test]
fn test_infer_logical_and() {
    let tp = infer_ok("let x = true && false");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Bool);
}

#[test]
fn test_infer_and_non_bool_is_error() {
    let err = infer_err("let x = 1 && 0");
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

#[test]
fn test_infer_comparison_int() {
    let tp = infer_ok("let x = 1 < 2");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Bool);
}

#[test]
fn test_infer_strict_equality_cross_type_is_error() {
    let err = infer_err("let x = 1 == 1.0");
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

// ── Unary operators ───────────────────────────────────────────────────────

#[test]
fn test_infer_unary_neg_int() {
    let tp = infer_ok("let x = -5");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

#[test]
fn test_infer_unary_not_bool() {
    let tp = infer_ok("let x = !true");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Bool);
}

#[test]
fn test_infer_unary_not_int_is_error() {
    let err = infer_err("let x = !1");
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

#[test]
fn test_infer_logical_with_unknown_operand_is_bool() {
    // `!x`, `x && y`, `x || y` on untyped (Unknown) params must infer Bool,
    // not int/Unknown — native codegen emits i1 for these, so a function
    // returning one needs a Bool signature or LLVM verification fails.
    // Regression for the v1.1.10 fix (and guards against re-widening `!`
    // to accept non-bool concrete operands).
    for (src, label) in [
        ("fn f(x) {\n return !x\n}",       "not"),
        ("fn f(x, y) {\n return x && y\n}", "and"),
        ("fn f(x, y) {\n return x || y\n}", "or"),
    ] {
        let tp = infer_ok(src);
        let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(*ret_ty, JadeType::Bool, "failed for: {label}");
    }
}

// ── Arrays ────────────────────────────────────────────────────────────────

#[test]
fn test_infer_array_int() {
    let tp = infer_ok("let a = [1, 2, 3]");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Int)));
}

#[test]
fn test_infer_array_empty() {
    let tp = infer_ok("let a = []");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Unknown)));
}

#[test]
fn test_infer_heterogeneous_array_widens_to_unknown() {
    // Heterogeneous arrays are now a type error.
    let err = infer_err(r#"let a = [1, "hello"]"#);
    assert!(matches!(err, crate::frontend::error::JadeError::HeterogeneousArray { .. }));
}

// ── Control flow ──────────────────────────────────────────────────────────

#[test]
fn test_infer_if_bool_condition() {
    infer_ok("if true {\n let x = 1\n}");
}

#[test]
fn test_infer_if_int_condition_is_error() {
    let err = infer_err("if 1 {\n let x = 2\n}");
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

#[test]
fn test_infer_while_bool_condition() {
    infer_ok("while false {\n let x = 1\n}");
}

// ── Functions ─────────────────────────────────────────────────────────────

#[test]
fn test_infer_fn_return_type_int() {
    let tp = infer_ok("fn add(x, y) {\n return 42\n}");
    let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(*ret_ty, JadeType::Int);
}

#[test]
fn test_infer_fn_no_return_is_nil() {
    let tp = infer_ok("fn noop(x) {\n let y = 1\n}");
    let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(*ret_ty, JadeType::Nil);
}

#[test]
fn test_infer_call_return_type() {
    let tp = infer_ok("fn id(x) {\n return 1\n}\nlet r = id(99)");
    let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

#[test]
fn test_infer_call_builtin_len() {
    let tp = infer_ok("let n = len([1, 2])");
    let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

// ── Structs ───────────────────────────────────────────────────────────────

#[test]
fn test_infer_struct_literal_type() {
    let tp = infer_ok("struct Point {\n x,\n y\n}\nlet p = Point { x: 1, y: 2 }");
    let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
    assert_eq!(value.ty, JadeType::Struct("Point".to_string()));
}

#[test]
fn test_infer_struct_undefined_type_is_error() {
    let err = infer_err("let p = Foo { x: 1 }");
    assert!(matches!(err, JadeError::UndefinedType { .. }));
}

#[test]
fn test_infer_struct_missing_field_is_error() {
    let err = infer_err("struct P {\n x,\n y\n}\nlet p = P { x: 1 }");
    assert!(matches!(err, JadeError::MissingField { .. }));
}

#[test]
fn test_infer_struct_extra_field_is_error() {
    let err = infer_err("struct P {\n x\n}\nlet p = P { x: 1, z: 2 }");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

#[test]
fn test_infer_empty_struct_literal_type() {
    let tp = infer_ok("struct Unit {}\nlet u = Unit {}");
    let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
    assert_eq!(value.ty, JadeType::Struct("Unit".to_string()));
}

#[test]
fn test_infer_empty_struct_extra_field_is_error() {
    let err = infer_err("struct Unit {}\nlet u = Unit { x: 1 }");
    assert!(matches!(err, JadeError::UndefinedField { .. }));
}

// ── Imports ───────────────────────────────────────────────────────────────

#[test]
fn test_infer_file_import_without_alias_is_error() {
    let err = infer_err(r#"use "my_lib.jde""#);
    assert!(matches!(err, JadeError::MissingImportAlias { .. }));
}

#[test]
fn test_infer_stdlib_import_without_alias_is_ok() {
    infer_ok("use std::math");
}

#[test]
fn test_infer_stdlib_string_import_is_error() {
    let err = infer_err(r#"use "std/math""#);
    assert!(matches!(err, JadeError::StdlibStringImport { .. }));
}

#[test]
fn test_infer_from_use_string_stdlib_is_error() {
    let err = infer_err(r#"from "std/math" use floor"#);
    assert!(matches!(err, JadeError::StdlibStringImport { .. }));
}

#[test]
fn test_infer_from_use_dot_stdlib_is_ok() {
    infer_ok("from std::math use floor");
}

// ── Prompts ───────────────────────────────────────────────────────────────

#[test]
fn test_infer_prompt_decl_type() {
    let tp = infer_ok(r#"prompt p = "hello""#);
    let TStmt::PromptDecl { .. } = &tp.stmts[0] else { panic!() };
}

#[test]
fn test_infer_prompt_decl_non_str_is_error() {
    let err = infer_err("prompt p = 42");
    assert!(matches!(err, JadeError::TypeMismatch { .. }));
}

#[test]
fn test_infer_prompt_deref_untyped_is_str() {
    let tp = infer_ok("prompt p = \"hi\"\nlet r = ?p");
    let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
    assert_eq!(value.ty, JadeType::Str);
}

#[test]
fn test_infer_prompt_deref_typed_int() {
    let tp = infer_ok("prompt p = \"hi\"\nlet r = ?p |> int");
    let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
    assert_eq!(value.ty, JadeType::Int);
}

// ── Undefined variable ────────────────────────────────────────────────────

#[test]
fn test_infer_undefined_variable_is_error() {
    let err = infer_err("let x = y + 1");
    assert!(matches!(err, JadeError::UndefinedVariable { .. }));
}

// ── Unknown propagation ───────────────────────────────────────────────────

#[test]
fn test_infer_unknown_param_propagates() {
    // Function body with unannotated params: operator with Unknown propagates to Unknown.
    let tp = infer_ok("fn add(x, y) {\n return x + y\n}");
    let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
    assert_eq!(*ret_ty, JadeType::Unknown);
}
