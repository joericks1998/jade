use super::*;
use crate::frontend::{
    ast::{BinOpKind, Expr, Stmt, UnaryOpKind},
    error::JadeError,
    lexer,
};

fn parse_src(src: &str) -> Program {
    let tokens = lexer::tokenize(src).unwrap();
    parse(tokens).unwrap()
}

fn parse_src_err(src: &str) -> JadeError {
    let tokens = lexer::tokenize(src).unwrap();
    parse(tokens).unwrap_err()
}

// ── existing operations ──────────────────────────────────────────────────

#[test]
fn test_parse_let_integer() {
    let p = parse_src("let x = 5");
    let Stmt::Let { name, value, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "x");
    assert!(matches!(value, Expr::Integer { value: 5, .. }));
}

#[test]
fn test_parse_let_float() {
    let p = parse_src("let x = 1.5");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::Float { .. }));
}

#[test]
fn test_parse_addition() {
    let p = parse_src("let x = 1 + 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Add, .. }));
}

#[test]
fn test_parse_subtraction() {
    let p = parse_src("let x = 5 - 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Sub, .. }));
}

#[test]
fn test_parse_multiplication() {
    let p = parse_src("let x = 2 * 4");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Mul, .. }));
}

#[test]
fn test_parse_division() {
    let p = parse_src("let x = 8 / 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Div, .. }));
}

#[test]
fn test_parse_modulo() {
    let p = parse_src("let x = 7 % 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Mod, .. }));
}

#[test]
fn test_parse_bitwise_and() {
    let p = parse_src("let x = 6 & 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitAnd, .. }));
}

#[test]
fn test_parse_bitwise_or() {
    let p = parse_src("let x = 6 | 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitOr, .. }));
}

#[test]
fn test_parse_bitwise_xor() {
    let p = parse_src("let x = 6 ^ 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitXor, .. }));
}

#[test]
fn test_parse_shl() {
    let p = parse_src("let x = 1 << 3");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Shl, .. }));
}

#[test]
fn test_parse_shr() {
    let p = parse_src("let x = 16 >> 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Shr, .. }));
}

#[test]
fn test_parse_bitnot() {
    let p = parse_src("let x = ~5");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::BitNot, .. }));
}

#[test]
fn test_parse_neg() {
    let p = parse_src("let x = -5");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::Neg, .. }));
}

#[test]
fn test_parse_identifier_ref() {
    let p = parse_src("let a = 1\nlet b = a");
    assert_eq!(p.stmts.len(), 2);
    let Stmt::Let { value, .. } = &p.stmts[1] else { panic!() };
    assert!(matches!(value, Expr::Identifier { .. }));
}

#[test]
fn test_parse_precedence_mul_before_add() {
    let p = parse_src("let x = 2 + 3 * 4");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::BinOp { op: BinOpKind::Add, right, .. } = value else { panic!("expected Add") };
    assert!(matches!(right.as_ref(), Expr::BinOp { op: BinOpKind::Mul, .. }));
}

#[test]
fn test_parse_precedence_grouped_overrides() {
    let p = parse_src("let x = (2 + 3) * 4");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::BinOp { op: BinOpKind::Mul, left, .. } = value else { panic!("expected Mul") };
    assert!(matches!(left.as_ref(), Expr::BinOp { op: BinOpKind::Add, .. }));
}

#[test]
fn test_parse_error_unexpected_token() {
    let err = parse_src_err("let 123 = 5");
    assert!(matches!(err, JadeError::UnexpectedToken { .. }));
}

#[test]
fn test_parse_error_unexpected_eof() {
    let err = parse_src_err("let x =");
    assert!(matches!(err, JadeError::UnexpectedEof { .. }));
}

// ── boolean / logical / comparison ───────────────────────────────────────

#[test]
fn test_parse_bool_literal_true() {
    let p = parse_src("let x = true");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::Bool { value: true, .. }));
}

#[test]
fn test_parse_bool_literal_false() {
    let p = parse_src("let x = false");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::Bool { value: false, .. }));
}

#[test]
fn test_parse_logical_and() {
    let p = parse_src("let x = true && false");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::And, .. }));
}

#[test]
fn test_parse_logical_or() {
    let p = parse_src("let x = true || false");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Or, .. }));
}

#[test]
fn test_parse_logical_not() {
    let p = parse_src("let x = !true");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::Not, .. }));
}

#[test]
fn test_parse_eq() {
    let p = parse_src("let x = 1 == 1");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Eq, .. }));
}

#[test]
fn test_parse_ne() {
    let p = parse_src("let x = 1 != 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Ne, .. }));
}

#[test]
fn test_parse_lt() {
    let p = parse_src("let x = 1 < 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Lt, .. }));
}

#[test]
fn test_parse_gt() {
    let p = parse_src("let x = 2 > 1");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Gt, .. }));
}

#[test]
fn test_parse_le() {
    let p = parse_src("let x = 1 <= 2");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Le, .. }));
}

#[test]
fn test_parse_ge() {
    let p = parse_src("let x = 2 >= 1");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::BinOp { op: BinOpKind::Ge, .. }));
}

#[test]
fn test_parse_precedence_compare_before_and() {
    let p = parse_src("let x = 1 < 2 && 3 > 0");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::BinOp { op: BinOpKind::And, left, right, .. } = value else { panic!("expected And") };
    assert!(matches!(left.as_ref(),  Expr::BinOp { op: BinOpKind::Lt, .. }));
    assert!(matches!(right.as_ref(), Expr::BinOp { op: BinOpKind::Gt, .. }));
}

// ── function / control flow parsing ──────────────────────────────────────

#[test]
fn test_parse_fn_def_no_params() {
    let p = parse_src("fn greet() {\n    return 1\n}");
    let Stmt::FnDef { name, params, body, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "greet");
    assert!(params.is_empty());
    assert_eq!(body.len(), 1);
    assert!(matches!(body[0], Stmt::Return { .. }));
}

#[test]
fn test_parse_fn_def_with_params() {
    let p = parse_src("fn add(a, b) {\n    return a\n}");
    let Stmt::FnDef { name, params, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "add");
    let names: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, &["a", "b"]);
}

#[test]
fn test_parse_return_with_value() {
    let p = parse_src("fn f(x) {\n    return x\n}");
    let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
    let Stmt::Return { value, .. } = &body[0] else { panic!() };
    assert!(matches!(value, Some(Expr::Identifier { .. })));
}

#[test]
fn test_parse_if_no_else() {
    let p = parse_src("fn f(x) {\n    if x {\n        return 1\n    }\n    return 0\n}");
    let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
    let Stmt::If { else_body, .. } = &body[0] else { panic!() };
    assert!(else_body.is_none());
}

#[test]
fn test_parse_if_with_else() {
    let p = parse_src("fn f(x) {\n    if x {\n        return 1\n    } else {\n        return 0\n    }\n}");
    let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
    let Stmt::If { then_body, else_body, .. } = &body[0] else { panic!() };
    assert_eq!(then_body.len(), 1);
    assert!(else_body.is_some());
}

#[test]
fn test_parse_call_no_args() {
    let p = parse_src("let x = f()");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::Call { args, .. } = value else { panic!() };
    assert!(args.is_empty());
}

#[test]
fn test_parse_call_with_args() {
    let p = parse_src("let x = add(1, 2)");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::Call { args, .. } = value else { panic!() };
    assert_eq!(args.len(), 2);
}

#[test]
fn test_parse_nested_call_as_arg() {
    let p = parse_src("let x = f(g(1))");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::Call { args, .. } = value else { panic!() };
    assert!(matches!(args[0], Expr::Call { .. }));
}

#[test]
fn test_parse_fn_as_value_in_let() {
    let p = parse_src("fn double(x) {\n    return x\n}\nlet f = double");
    assert_eq!(p.stmts.len(), 2);
    let Stmt::Let { value, .. } = &p.stmts[1] else { panic!() };
    assert!(matches!(value, Expr::Identifier { .. }));
}

#[test]
fn test_parse_nested_fn_ok() {
    // Nested function definitions are now allowed.
    let prog = parse_src("fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}");
    assert_eq!(prog.stmts.len(), 1);
}

#[test]
fn test_parse_return_outside_fn_error() {
    let err = parse_src_err("return 1");
    assert!(matches!(err, JadeError::ReturnOutsideFunction { .. }));
}

// ── while ────────────────────────────────────────────────────────────────

#[test]
fn test_parse_while_basic() {
    let p = parse_src("let i = 0\nwhile i < 5 {\n    let i = i + 1\n}");
    assert_eq!(p.stmts.len(), 2);
    let Stmt::While { condition, body, .. } = &p.stmts[1] else { panic!() };
    assert!(matches!(condition, Expr::BinOp { op: BinOpKind::Lt, .. }));
    assert_eq!(body.len(), 1);
}

#[test]
fn test_parse_while_empty_body() {
    let p = parse_src("while false {\n}");
    assert_eq!(p.stmts.len(), 1);
    let Stmt::While { body, .. } = &p.stmts[0] else { panic!() };
    assert!(body.is_empty());
}

#[test]
fn test_parse_while_inside_fn() {
    let p = parse_src("fn f(n) {\n    while n > 0 {\n        let n = n - 1\n    }\n    return n\n}");
    let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(body[0], Stmt::While { .. }));
}

// ── struct / extend ───────────────────────────────────────────────────────

#[test]
fn test_parse_struct_def() {
    let p = parse_src("struct Point {\n    x,\n    y\n}");
    let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Point");
    assert_eq!(fields.len(), 2);
    assert!(matches!(&fields[0], StructFieldDef::Required(n) if n == "x"));
    assert!(matches!(&fields[1], StructFieldDef::Required(n) if n == "y"));
}

#[test]
fn test_parse_struct_def_empty() {
    let p = parse_src("struct Empty {\n}");
    let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Empty");
    assert!(fields.is_empty());
}

#[test]
fn test_parse_struct_def_let_default() {
    let p = parse_src("struct Agent {\n    let name = \"Assistant\"\n}");
    let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Agent");
    assert_eq!(fields.len(), 1);
    assert!(matches!(&fields[0], StructFieldDef::Let { name, .. } if name == "name"));
}

#[test]
fn test_parse_struct_def_prompt_field() {
    let p = parse_src("struct Agent {\n    prompt system = \"You are helpful\"\n}");
    let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Agent");
    assert_eq!(fields.len(), 1);
    assert!(matches!(&fields[0], StructFieldDef::Prompt { name, .. } if name == "system"));
}

#[test]
fn test_parse_struct_def_mixed() {
    let p = parse_src("struct Mixed {\n    x,\n    let label = \"origin\",\n    prompt sys = \"helpful\"\n}");
    let Stmt::StructDef { fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(fields.len(), 3);
    assert!(matches!(&fields[0], StructFieldDef::Required(n) if n == "x"));
    assert!(matches!(&fields[1], StructFieldDef::Let { name, .. } if name == "label"));
    assert!(matches!(&fields[2], StructFieldDef::Prompt { name, .. } if name == "sys"));
}

#[test]
fn test_parse_struct_def_struct_literal_default() {
    // A struct-literal expression as a default value parses without ambiguity.
    // The inner `}` belongs to the nested struct literal; the outer `}` closes the struct def.
    let p = parse_src("struct Wrapper {\n    let inner = Point { x: 0, y: 0 }\n}");
    let Stmt::StructDef { fields, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(fields.len(), 1);
    assert!(matches!(&fields[0], StructFieldDef::Let { name, default: Expr::StructLiteral { .. }, .. } if name == "inner"));
}

#[test]
fn test_parse_extend_block() {
    let p = parse_src("extend Foo {\n    fn get(self) {\n        return 1\n    }\n}");
    let Stmt::ExtendBlock { type_name, methods, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(type_name, "Foo");
    assert_eq!(methods.len(), 1);
    let Stmt::FnDef { name, params, .. } = &methods[0] else { panic!() };
    assert_eq!(name, "get");
    assert_eq!(params[0].0, "self");
}

#[test]
fn test_parse_struct_literal() {
    let p = parse_src("let p = Point { x: 1, y: 2 }");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::StructLiteral { type_name, fields, .. } = value else { panic!() };
    assert_eq!(type_name, "Point");
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].0, "x");
    assert_eq!(fields[1].0, "y");
}

#[test]
fn test_parse_field_access() {
    let p = parse_src("let v = p.x");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::FieldAccess { field, .. } = value else { panic!() };
    assert_eq!(field, "x");
}

#[test]
fn test_parse_field_assign() {
    let p = parse_src("let p = 0\np.x = 5");
    let Stmt::FieldAssign { object, field, .. } = &p.stmts[1] else { panic!() };
    assert_eq!(object, "p");
    assert_eq!(field, "x");
}

#[test]
fn test_parse_pipe_invalid_rhs_is_error() {
    // `|>` requires a function name or call on the right, not a raw expression
    let tokens = lexer::tokenize("5 |> (1 + 2)").unwrap();
    assert!(parse(tokens).is_err());
}

// ── arrays ───────────────────────────────────────────────────────────────

#[test]
fn test_parse_array_literal_empty() {
    let p = parse_src("let a = []");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::Array { elements, .. } if elements.is_empty()));
}

#[test]
fn test_parse_array_literal_ints() {
    let p = parse_src("let a = [1, 2, 3]");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::Array { elements, .. } = value else { panic!() };
    assert_eq!(elements.len(), 3);
    assert!(matches!(elements[0], Expr::Integer { value: 1, .. }));
}

#[test]
fn test_parse_array_trailing_comma() {
    let p = parse_src("let a = [1, 2,]");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    let Expr::Array { elements, .. } = value else { panic!() };
    assert_eq!(elements.len(), 2);
}

#[test]
fn test_parse_array_index() {
    let p = parse_src("let x = a[0]");
    let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(value, Expr::Index { .. }));
}

#[test]
fn test_parse_array_index_assign() {
    let p = parse_src("let a = []\na[1] = 99");
    let Stmt::IndexAssign { name, .. } = &p.stmts[1] else { panic!() };
    assert_eq!(name, "a");
}

// ── interface ─────────────────────────────────────────────────────────────

#[test]
fn test_parse_interface_def() {
    let p = parse_src("interface Displayable {\n    fn to_str(self) -> str\n}");
    let Stmt::InterfaceDef { name, methods, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Displayable");
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].name, "to_str");
    assert_eq!(methods[0].params, vec!["self"]);
    assert_eq!(methods[0].return_type.as_deref(), Some("str"));
}

#[test]
fn test_parse_interface_def_no_return_type() {
    let p = parse_src("interface Runnable {\n    fn run(self)\n}");
    let Stmt::InterfaceDef { name, methods, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "Runnable");
    assert_eq!(methods[0].name, "run");
    assert!(methods[0].return_type.is_none());
}

#[test]
fn test_parse_extend_with_interface() {
    let p = parse_src("extend Foo: Bar {\n    fn go(self) {\n        return 1\n    }\n}");
    let Stmt::ExtendBlock { type_name, interface_name, methods, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(type_name, "Foo");
    assert_eq!(interface_name.as_deref(), Some("Bar"));
    assert_eq!(methods.len(), 1);
}

#[test]
fn test_parse_extend_without_interface() {
    let p = parse_src("extend Foo {\n    fn go(self) {\n        return 1\n    }\n}");
    let Stmt::ExtendBlock { type_name, interface_name, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(type_name, "Foo");
    assert!(interface_name.is_none());
}

#[test]
fn test_parse_fn_with_return_type() {
    let p = parse_src("fn greet(name) -> str {\n    return \"hi\"\n}");
    let Stmt::FnDef { name, params, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(name, "greet");
    assert_eq!(params[0].0, "name");
}

// ── LLM / prompt ────────────────────────────────────────────────────────

#[test]
fn test_parse_prompt_decl() {
    let p = parse_src("prompt p = \"hello\"");
    let Stmt::PromptDecl { name, .. } = &p.stmts[0] else { panic!("expected PromptDecl") };
    assert_eq!(name, "p");
}

#[test]
fn test_parse_prompt_deref_untyped() {
    let p = parse_src("let x = ?p");
    let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with PromptDeref") };
    assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
    assert!(constraint.is_none());
}

#[test]
fn test_parse_prompt_deref_typed_int() {
    let p = parse_src("let x = ?p |> int");
    let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with PromptDeref") };
    assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
    assert!(matches!(constraint.as_deref(), Some(Expr::Identifier { name, .. }) if name == "int"));
}

#[test]
fn test_parse_prompt_deref_field_access() {
    let p = parse_src("let x = ?obj.system");
    let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with PromptDeref") };
    assert!(matches!(expr.as_ref(), Expr::FieldAccess { field, .. } if field == "system"));
    assert!(constraint.is_none());
}

#[test]
fn test_parse_prompt_deref_field_access_typed() {
    let p = parse_src("let x = ?obj.field |> int");
    let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with PromptDeref") };
    assert!(matches!(expr.as_ref(), Expr::FieldAccess { field, .. } if field == "field"));
    assert!(matches!(constraint.as_deref(), Some(Expr::Identifier { name, .. }) if name == "int"));
}

#[test]
fn test_parse_streaming_prohibition() {
    let tokens = super::super::lexer::tokenize("print(?p |> int)").expect("lex");
    let err = parse(tokens).unwrap_err();
    assert!(matches!(err, super::super::error::JadeError::StreamingWithType { .. }));
}

// ── dict literal tests ────────────────────────────────────────────────────

#[test]
fn test_parse_dict_empty() {
    let p = parse_src("let d = {}");
    let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with Dict") };
    assert!(entries.is_empty());
}

#[test]
fn test_parse_dict_single_entry() {
    let p = parse_src(r#"let d = {"key": 1}"#);
    let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with Dict") };
    assert_eq!(entries.len(), 1);
    assert!(matches!(&entries[0].0, Expr::Str { value, .. } if value == "key"));
    assert!(matches!(&entries[0].1, Expr::Integer { value: 1, .. }));
}

#[test]
fn test_parse_dict_multiple_entries() {
    let p = parse_src(r#"let d = {"a": 1, "b": 2}"#);
    let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with Dict") };
    assert_eq!(entries.len(), 2);
}

#[test]
fn test_parse_dict_trailing_comma() {
    let p = parse_src(r#"let d = {"a": 1,}"#);
    let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with Dict") };
    assert_eq!(entries.len(), 1);
}

#[test]
fn test_parse_dict_identifier_key() {
    // Variable key: the key expression is an identifier
    let p = parse_src("let d = {k: 1}");
    let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
        else { panic!("expected Let with Dict") };
    assert_eq!(entries.len(), 1);
    assert!(matches!(&entries[0].0, Expr::Identifier { name, .. } if name == "k"));
}

// ── default parameter values ──────────────────────────────────────────────

#[test]
fn test_parse_fn_default_param_int() {
    let p = parse_src("fn f(a, b = 42) {}");
    let Stmt::FnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(params.len(), 2);
    assert!(params[0].1.is_none());
    assert!(matches!(params[1].1, Some(Expr::Integer { value: 42, .. })));
}

#[test]
fn test_parse_fn_default_param_str() {
    let p = parse_src("fn f(x, label = \"hello\") {}");
    let Stmt::FnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(&params[1].1, Some(Expr::Str { value, .. }) if value == "hello"));
}

#[test]
fn test_parse_fn_default_param_nil() {
    let p = parse_src("fn f(x, on = nil) {}");
    let Stmt::FnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(&params[1].1, Some(Expr::Identifier { name, .. }) if name == "nil"));
}

#[test]
fn test_parse_fn_default_param_bool() {
    let p = parse_src("fn f(a, flag = false) {}");
    let Stmt::FnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert!(matches!(params[1].1, Some(Expr::Bool { value: false, .. })));
}

#[test]
fn test_parse_fn_all_defaults() {
    let p = parse_src("fn f(a = 1, b = 2, c = 3) {}");
    let Stmt::FnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(params.len(), 3);
    assert!(params.iter().all(|(_, d)| d.is_some()));
}

#[test]
fn test_parse_async_fn_default_param() {
    let p = parse_src("async fn f(x, y = 0) {}");
    let Stmt::AsyncFnDef { params, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(params.len(), 2);
    assert!(params[0].1.is_none());
    assert!(matches!(params[1].1, Some(Expr::Integer { value: 0, .. })));
}

// ── decorator argument parsing ────────────────────────────────────────────

#[test]
fn test_parse_decorator_no_args() {
    let p = parse_src("@tag\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(decorators.len(), 1);
    assert_eq!(decorators[0].0, "tag");
    assert!(decorators[0].1.is_empty());
}

#[test]
fn test_parse_decorator_positional_str_arg() {
    let p = parse_src("@tag(\"hello\")\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    let (kw, expr) = &decorators[0].1[0];
    assert!(kw.is_none());
    assert!(matches!(expr, Expr::Str { value, .. } if value == "hello"));
}

#[test]
fn test_parse_decorator_positional_int_arg() {
    let p = parse_src("@retry(3)\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    let (kw, expr) = &decorators[0].1[0];
    assert!(kw.is_none());
    assert!(matches!(expr, Expr::Integer { value: 3, .. }));
}

#[test]
fn test_parse_decorator_kwarg() {
    let p = parse_src("@route(on = \"action\")\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    let (kw, expr) = &decorators[0].1[0];
    assert_eq!(kw.as_deref(), Some("on"));
    assert!(matches!(expr, Expr::Str { value, .. } if value == "action"));
}

#[test]
fn test_parse_decorator_multiple_args_mixed() {
    let p = parse_src("@dec(\"pos\", key = \"val\")\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    let args = &decorators[0].1;
    assert_eq!(args.len(), 2);
    assert!(args[0].0.is_none());
    assert_eq!(args[1].0.as_deref(), Some("key"));
}

#[test]
fn test_parse_decorator_multiple_decorators() {
    let p = parse_src("@a\n@b(\"x\")\nfn f() {}");
    let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(decorators.len(), 2);
    assert_eq!(decorators[0].0, "a");
    assert_eq!(decorators[1].0, "b");
}

#[test]
fn test_parse_struct_decorator_with_arg() {
    let p = parse_src("@log(\"City\")\nstruct City {\n  name,\n}");
    let Stmt::StructDef { decorators, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(decorators.len(), 1);
    assert_eq!(decorators[0].0, "log");
    let (kw, expr) = &decorators[0].1[0];
    assert!(kw.is_none());
    assert!(matches!(expr, Expr::Str { value, .. } if value == "City"));
}

#[test]
fn test_parse_extend_decorator_route_positional() {
    let p = parse_src("@route(\"action\")\nextend Cmd {}");
    let Stmt::ExtendBlock { decorators, type_name, .. } = &p.stmts[0] else { panic!() };
    assert_eq!(type_name, "Cmd");
    assert_eq!(decorators[0].0, "route");
    let (kw, expr) = &decorators[0].1[0];
    assert!(kw.is_none());
    assert!(matches!(expr, Expr::Str { value, .. } if value == "action"));
}

#[test]
fn test_parse_extend_decorator_route_kwarg() {
    let p = parse_src("@route(on = \"field\")\nextend T {}");
    let Stmt::ExtendBlock { decorators, .. } = &p.stmts[0] else { panic!() };
    let (kw, expr) = &decorators[0].1[0];
    assert_eq!(kw.as_deref(), Some("on"));
    assert!(matches!(expr, Expr::Str { value, .. } if value == "field"));
}

#[test]
fn test_parse_implicit_self_field_access() {
    // `.name` should desugar to `self.name`
    let p = parse_src("extend T { fn greet(self) { return .name } }");
    let Stmt::ExtendBlock { methods, .. } = &p.stmts[0] else { panic!() };
    let Stmt::FnDef { body, .. } = &methods[0] else { panic!() };
    let Stmt::Return { value: Some(ret), .. } = &body[0] else { panic!() };
    assert!(matches!(ret,
        Expr::FieldAccess { object, field, .. }
        if field == "name" && matches!(object.as_ref(), Expr::Identifier { name, .. } if name == "self")
    ));
}

#[test]
fn test_parse_implicit_self_assignment() {
    // `.count = .count + 1` desugars to FieldAssign { object: "self", field: "count", ... }
    let p = parse_src("extend T { fn inc(self) { .count = .count + 1 } }");
    let Stmt::ExtendBlock { methods, .. } = &p.stmts[0] else { panic!() };
    let Stmt::FnDef { body, .. } = &methods[0] else { panic!() };
    let Stmt::FieldAssign { object, field, .. } = &body[0] else { panic!() };
    assert_eq!(object, "self");
    assert_eq!(field, "count");
}
