//! Unit tests for the compiler proper: type inference, TIR, emission, GBNF.
//!
//! The VM and bytecode tests used to live here too. They moved out with their
//! modules — `vm/tests.rs` and `bytecode/tests.rs` — when the VM stopped being
//! filed as a compiler phase. One inline submodule per source module keeps each
//! test scope clean.
#![allow(clippy::all)]

mod type_infer {
    use crate::compiler::tir::{JadeType, TProgram, TStmt};
    use crate::compiler::type_infer::*;
    use crate::frontend::error::{JadeError, Result};
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
            ("fn f(x) {\n return !x\n}", "not"),
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
        // Nothing more precise than `Unknown` is true of every element, so that
        // is the element type — not an error. It was one until v1.1.32, which is
        // why this test's name outlived two opposite assertions.
        let tp = infer_ok(r#"let a = [1, "hello"]"#);
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Unknown)));
    }

    /// Uniform elements still give a concrete element type — widening is the
    /// fallback, not the rule. Losing this would pessimize every typed array.
    #[test]
    fn test_infer_homogeneous_array_keeps_its_element_type() {
        let tp = infer_ok(r#"let a = ["x", "y"]"#);
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Str)));
    }

    /// Mixed numerics widen rather than promoting to float. Typing `a[0]` as
    /// float while the slot holds a tagged int would send AOT codegen down a
    /// specialized path for a value that is not that type.
    #[test]
    fn test_infer_mixed_numeric_array_widens_rather_than_promoting() {
        let tp = infer_ok("let a = [1, 2.0]");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Unknown)));
    }

    /// An element the checker cannot type does not itself make the array mixed.
    #[test]
    fn test_infer_array_ignores_unknown_elements_when_widening() {
        let tp = infer_ok("fn f() { return 1 }\nlet a = [1, f(), 3]");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Int)));
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
    fn test_infer_quoted_file_import_is_error() {
        // Quoted file imports were removed — use a bare/`::` module name instead.
        let err = infer_err(r#"use "my_lib.jde""#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
    }

    #[test]
    fn test_infer_bare_module_import_is_ok() {
        // A sibling `.jde` file is imported by bare module name; resolution is
        // deferred to run/build, so type-check accepts it.
        infer_ok("use my_lib");
        infer_ok("use sub::helper");
    }

    #[test]
    fn test_infer_import_alias_is_error() {
        let err = infer_err("use my_lib as m");
        assert!(matches!(err, JadeError::ImportAlias { .. }));
    }

    #[test]
    fn test_infer_stdlib_import_is_ok() {
        infer_ok("use std::math");
    }

    #[test]
    fn test_infer_quoted_stdlib_import_is_error() {
        let err = infer_err(r#"use "std/math""#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
    }

    #[test]
    fn test_infer_from_use_quoted_is_error() {
        let err = infer_err(r#"from "std/math" use floor"#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
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

    // ── Dict value-type tracking ───────────────────────────────────────────────
    // A variable bound to a homogeneous dict literal lets `d[k]` infer the value's
    // concrete type (drives the AOT backend's static print/format codegen); mixed
    // or mutated dicts fall back to Unknown so the type is never wrong.

    #[test]
    fn test_index_homogeneous_bool_dict_is_bool() {
        let tp = infer_ok("let d = {\"ok\": true}\nlet r = d[\"ok\"]");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Bool);
    }

    #[test]
    fn test_index_homogeneous_int_dict_is_int() {
        let tp = infer_ok("let d = {\"a\": 1, \"b\": 2}\nlet r = d[\"a\"]");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    #[test]
    fn test_index_mixed_dict_is_unknown() {
        let tp = infer_ok("let d = {\"name\": \"jade\", \"version\": 1}\nlet r = d[\"name\"]");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Unknown);
    }

    #[test]
    fn test_reassigned_dict_clears_value_type() {
        let tp = infer_ok("let d = {\"ok\": true}\nd = {\"a\": 1, \"b\": \"x\"}\nlet r = d[\"a\"]");
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!() };
        assert_eq!(value.ty, JadeType::Unknown);
    }

    #[test]
    fn test_index_assign_heterogeneous_clears_value_type() {
        let tp = infer_ok("let d = {\"a\": true}\nd[\"b\"] = 5\nlet r = d[\"a\"]");
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!() };
        assert_eq!(value.ty, JadeType::Unknown);
    }

    // ── Pipe stages ───────────────────────────────────────────────────────────
    //
    // `|>` reaches inference as an `Expr::Pipe` carrying an unclassified stage.
    // These pin which of the three meanings each stage shape takes, because the
    // choice is the language rule and it is not visible in the syntax.

    use crate::compiler::tir::TExprKind;

    /// The stage folds back into the dereference as an `output_type`, which is
    /// what makes `grammar_for` constrain sampling. If this regressed to an
    /// ordinary call the program would still compile and still coerce — the
    /// model would simply generate unconstrained, which is exactly the kind of
    /// failure that shows up as a flaky reply rather than an error.
    #[test]
    fn a_type_name_stage_on_a_deref_becomes_an_output_type() {
        let tp = infer_ok("prompt p = \"x\"\nlet n = ?p |> int");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        let TExprKind::PromptDeref { output_type, grammar_expr, .. } = &value.kind else {
            panic!("expected PromptDeref, got {:?}", value.kind)
        };
        assert_eq!(output_type.as_deref(), Some("int"));
        assert!(grammar_expr.is_none());
        assert_eq!(value.ty, JadeType::Int);
    }

    /// A struct registers itself as a callable constructor under its own name,
    /// so by type alone it is indistinguishable from a function. Checking the
    /// function rule first turned `?p |> City` into `City(?p)`.
    #[test]
    fn a_struct_name_stage_on_a_deref_is_a_type_not_a_call() {
        let tp = infer_ok("struct City { name }\nprompt p = \"x\"\nlet c = ?p |> City");
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!("expected Let") };
        let TExprKind::PromptDeref { output_type, .. } = &value.kind else {
            panic!("expected PromptDeref, got {:?}", value.kind)
        };
        assert_eq!(output_type.as_deref(), Some("City"));
        // `parse_type_name` maps only the builtin keywords, so a struct-typed
        // dereference is statically `Unknown` and its fields are checked at run
        // time. That predates v1.2.0 and is deliberately unchanged here: this
        // patch moves *where* a stage is classified, not what a stage means.
        assert_eq!(value.ty, JadeType::Unknown);
    }

    #[test]
    fn a_grammar_stage_on_a_deref_becomes_a_grammar_expr() {
        let tp =
            infer_ok("let g = Grammar.new('\"yes\" | \"no\"')\nprompt p = \"x\"\nlet r = ?p |> g");
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!("expected Let") };
        let TExprKind::PromptDeref { output_type, grammar_expr, .. } = &value.kind else {
            panic!("expected PromptDeref, got {:?}", value.kind)
        };
        assert!(output_type.is_none());
        assert!(grammar_expr.is_some());
        assert_eq!(value.ty, JadeType::Str);
    }

    /// The chain that was unwritable before v1.2.0. The first stage folds in and
    /// constrains generation; the second is an ordinary call that receives the
    /// coerced int rather than the raw reply text.
    #[test]
    fn a_function_stage_after_a_typed_deref_receives_the_coerced_value() {
        let tp = infer_ok(
            "fn double(x) { return x * 2 }\nprompt p = \"x\"\nlet n = ?p |> int |> double",
        );
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!("expected Let") };
        let TExprKind::Call { args, .. } = &value.kind else {
            panic!("expected the outer stage to be a Call, got {:?}", value.kind)
        };
        assert_eq!(args.len(), 1, "the piped value is the sole argument");
        assert_eq!(args[0].ty, JadeType::Int, "double() receives an int, not the reply text");
    }

    /// A function stage on a plain value is the pipe the language always had.
    #[test]
    fn a_function_stage_on_an_ordinary_value_is_a_call() {
        let tp = infer_ok("fn double(x) { return x * 2 }\nlet n = 5 |> double");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        let TExprKind::Call { args, .. } = &value.kind else { panic!("expected Call") };
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].ty, JadeType::Int);
    }

    /// The piped value goes in front of the stage's own arguments.
    #[test]
    fn a_call_stage_takes_the_piped_value_as_its_first_argument() {
        let tp = infer_ok("fn add(a, b) { return a + b }\nlet n = 5 |> add(3)");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        let TExprKind::Call { args, .. } = &value.kind else { panic!("expected Call") };
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn a_non_callable_stage_is_an_error() {
        for src in ["let x = 5 |> 3", "let x = 5 |> \"nope\"", "let n = 1\nlet x = 5 |> n"] {
            assert!(
                matches!(infer_err(src), JadeError::InvalidPipeStage { .. }),
                "expected InvalidPipeStage for {src}",
            );
        }
    }

    /// A user function beats anything else it collides with, which is the rule
    /// for every name that is not a builtin keyword or a declared struct.
    // ── char ──────────────────────────────────────────────────────────────────

    #[test]
    fn indexing_a_string_infers_char() {
        let tp = infer_ok("let s = \"hi\"\nlet c = s[0]");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        assert_eq!(value.ty, JadeType::Char);
    }

    #[test]
    fn iterating_a_string_binds_a_char() {
        // The loop variable's type shows up on a use of it inside the body.
        let tp = infer_ok("for c in \"hi\" {\n    let x = c\n}");
        let TStmt::For { body, .. } = &tp.stmts[0] else { panic!("expected For") };
        let TStmt::Let { value, .. } = &body[0] else { panic!("expected Let in body") };
        assert_eq!(value.ty, JadeType::Char);
    }

    /// The documented exception to "`==` refuses to compare across types". It
    /// exists because `s[i]` began yielding a char: without it every
    /// `if s[0] == "a"` already written would have become a type error.
    #[test]
    fn a_char_compares_with_a_str_in_both_orders() {
        for src in [
            "let s = \"hi\"\nlet b = s[0] == \"h\"",
            "let s = \"hi\"\nlet b = \"h\" == s[0]",
            "let s = \"hi\"\nlet b = s[0] < s[1]",
        ] {
            let tp = infer_ok(src);
            let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
            assert_eq!(value.ty, JadeType::Bool, "for {src}");
        }
    }

    #[test]
    fn concatenating_a_char_with_a_str_yields_a_str() {
        for src in [
            "let s = \"hi\"\nlet r = s[0] + \"x\"",
            "let s = \"hi\"\nlet r = \"x\" + s[0]",
            "let s = \"hi\"\nlet r = s[0] + s[1]",
        ] {
            let tp = infer_ok(src);
            let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
            assert_eq!(value.ty, JadeType::Str, "for {src}");
        }
    }

    /// `char` is a builtin type keyword, so it constrains a dereference rather
    /// than applying the `char()` constructor to the raw reply. Without the
    /// grammar the model generates freely and the coercion then fails.
    #[test]
    fn a_char_stage_on_a_deref_constrains_rather_than_converts() {
        let tp = infer_ok("prompt p = \"x\"\nlet c = ?p |> char");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        let TExprKind::PromptDeref { output_type, .. } = &value.kind else {
            panic!("expected PromptDeref, got {:?}", value.kind)
        };
        assert_eq!(output_type.as_deref(), Some("char"));
        assert_eq!(value.ty, JadeType::Char);
    }

    // ── yield / streams ───────────────────────────────────────────────────────

    #[test]
    fn a_function_that_yields_returns_a_stream() {
        let tp = infer_ok("fn g() {\n    yield 1\n}\nlet s = g()");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        assert_eq!(value.ty, JadeType::Stream(Box::new(JadeType::Int)));
    }

    /// A `yield` anywhere in the body counts, not just at the top level.
    #[test]
    fn a_yield_inside_a_loop_still_makes_a_generator() {
        let tp = infer_ok("fn g(n) {\n    while n > 0 {\n        yield n\n    }\n}\nlet s = g(3)");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        assert!(matches!(value.ty, JadeType::Stream(_)), "got {:?}", value.ty);
    }

    /// The same widening rule a mixed array literal follows: disagreement is not
    /// an error, it just stops being specific.
    #[test]
    fn yielded_types_that_disagree_widen_to_unknown() {
        let tp = infer_ok("fn g() {\n    yield 1\n    yield \"two\"\n}\nlet s = g()");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        assert_eq!(value.ty, JadeType::Stream(Box::new(JadeType::Unknown)));
    }

    #[test]
    fn iterating_a_stream_binds_its_element_type() {
        let tp = infer_ok("fn g() {\n    yield 1\n}\nfor x in g() {\n    let y = x\n}");
        let TStmt::For { body, .. } = &tp.stmts[1] else { panic!("expected For") };
        let TStmt::Let { value, .. } = &body[0] else { panic!("expected Let in body") };
        assert_eq!(value.ty, JadeType::Int);
    }

    /// A generator produces a stream, so returning a value as well asks it to be
    /// two things. A *bare* return is fine — it stops the generator early.
    #[test]
    fn a_generator_cannot_also_return_a_value() {
        let err = infer_err("fn g() {\n    yield 1\n    return 2\n}");
        assert!(matches!(err, JadeError::YieldAndReturn { .. }), "got {err:?}");
    }

    #[test]
    fn a_generator_may_return_bare_to_stop_early() {
        let tp = infer_ok(
            "fn g(n) {\n    yield 1\n    if n > 0 {\n        return\n    }\n    yield 2\n}\nlet s = g(0)",
        );
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!("expected Let") };
        assert!(matches!(value.ty, JadeType::Stream(_)), "got {:?}", value.ty);
    }

    #[test]
    fn a_user_function_stage_beats_a_grammar_variable_of_the_same_name() {
        let tp = infer_ok("fn shout(s) { return s }\nprompt p = \"x\"\nlet r = ?p |> shout");
        let TStmt::Let { value, .. } = &tp.stmts[2] else { panic!("expected Let") };
        assert!(
            matches!(value.kind, TExprKind::Call { .. }),
            "a function stage applies, it does not constrain: {:?}",
            value.kind,
        );
    }

    // ── Handles cannot cross a task boundary ──────────────────────────────────
    //
    // Tested at the rule rather than through source, because no syntax produces
    // a `Handle` type yet: it arrives with a declared native binding. The check
    // is written now so the guarantee lands with the feature rather than after
    // someone hits the race.
    //
    // `taskcheck` cannot cover this. It watches SetIndex/SetField/mutating
    // methods, and a handle has none — all the mutation is inside the C library.

    use crate::compiler::tir::TExpr;
    use crate::frontend::error::Span;

    fn arg(ty: JadeType) -> TExpr {
        TExpr { kind: TExprKind::Identifier("db".to_string()), ty, span: Span { line: 3, col: 7 } }
    }

    #[test]
    fn a_handle_cannot_be_passed_into_a_task() {
        let err = reject_handle_across_a_task(&[arg(JadeType::Handle("sqlite3".to_string()))])
            .expect_err("a handle argument must be refused");
        match err {
            JadeError::HandleAcrossTask { type_name, span } => {
                assert_eq!(type_name, "sqlite3");
                assert_eq!(span.line, 3, "the error points at the argument, not the spawn");
            }
            other => panic!("expected HandleAcrossTask, got {other:?}"),
        }
    }

    #[test]
    fn the_message_names_the_fix_and_not_the_wrong_one() {
        let err = reject_handle_across_a_task(&[arg(JadeType::Handle("SNDFILE".to_string()))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("handle<SNDFILE>"), "must name the type: {err}");
        assert!(err.contains("open the handle inside the task"), "must name the fix: {err}");
        // The generic shared-mutation advice is the opposite of correct here —
        // passing it in as a parameter is exactly what was just refused.
        assert!(!err.contains("pass the value in as a parameter"), "wrong advice: {err}");
    }

    #[test]
    fn an_array_of_handles_is_refused_too() {
        // Wrapping one in a container does not make it safe to share.
        let ty = JadeType::Array(Box::new(JadeType::Handle("gzFile".to_string())));
        assert!(reject_handle_across_a_task(&[arg(ty)]).is_err());
    }

    #[test]
    fn ordinary_arguments_still_pass() {
        let ok = [
            arg(JadeType::Int),
            arg(JadeType::Str),
            arg(JadeType::Array(Box::new(JadeType::Int))),
            arg(JadeType::Struct("Point".to_string())),
            arg(JadeType::Unknown),
        ];
        assert!(reject_handle_across_a_task(&ok).is_ok());
    }
}

mod gbnf {
    use crate::compiler::gbnf::*;
    use crate::frontend::ast::StructFieldDef;
    use std::collections::HashMap;

    fn no_defs() -> HashMap<String, Vec<StructFieldDef>> {
        HashMap::new()
    }

    #[test]
    fn int_grammar() {
        let g = grammar_for("int", &no_defs()).unwrap();
        assert!(g.contains("root ::="));
        assert!(g.contains("[0-9]"));
    }

    #[test]
    fn bool_grammar() {
        let g = grammar_for("bool", &no_defs()).unwrap();
        assert!(g.contains("\"true\""), "should match true");
        assert!(g.contains("\"false\""), "should match false");
        assert!(g.contains(r"[ \t\n\r]*"), "should allow trailing whitespace");
    }

    #[test]
    fn str_is_none() {
        assert!(grammar_for("str", &no_defs()).is_none());
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(grammar_for("UnknownType", &no_defs()).is_none());
    }

    #[test]
    fn struct_grammar_is_prefix_with_free_rest() {
        let fields = vec![
            StructFieldDef::Required("name".to_string()),
            StructFieldDef::Required("age".to_string()),
        ];
        let mut defs = HashMap::new();
        defs.insert("Person".to_string(), fields);
        let g = grammar_for("Person", &defs).unwrap();
        assert!(g.contains("\"{\""), "grammar should anchor opening brace");
        // Must allow a continuation after the brace — an anchor-only grammar
        // (`root ::= "{"`) forces premature EOG and never coerces.
        assert!(g.contains("rest"), "grammar must permit a continuation after `{{`");
        assert_ne!(g.trim(), "root ::= \"{\"", "grammar must not be anchor-only");
    }

    #[test]
    fn array_grammar() {
        let g = grammar_for("array", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"[\""), "should anchor opening bracket");
        assert!(g.contains("rest"), "grammar must permit a continuation after `[`");
        assert_ne!(g.trim(), "root ::= \"[\"", "grammar must not be anchor-only");
    }

    #[test]
    fn dict_grammar() {
        let g = grammar_for("dict", &no_defs()).unwrap();
        assert!(g.starts_with("root"));
        assert!(g.contains("\"{\""), "should anchor opening brace");
        assert!(g.contains("rest"), "grammar must permit a continuation after `{{`");
        assert_ne!(g.trim(), "root ::= \"{\"", "grammar must not be anchor-only");
    }
}

// ─── NEW TESTS ──────────────────────────────────────────────────────────────

mod emit {
    use crate::bytecode::Instr;
    use crate::compiler::emit::{self, CompiledProgram};
    use crate::compiler::type_infer;
    use crate::frontend::error::Result;
    use crate::frontend::{lexer, parser};

    /// Run the frontend pipeline (lex → parse → infer) and emit bytecode.
    fn compile(src: &str) -> Result<CompiledProgram> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        let tprogram = type_infer::infer(program).expect("infer failed");
        emit::emit(tprogram)
    }

    fn compile_ok(src: &str) -> CompiledProgram {
        compile(src).expect("emit failed")
    }

    /// Count instructions in the top-level chunk matching a predicate.
    fn count_top<F: Fn(&Instr) -> bool>(cp: &CompiledProgram, f: F) -> usize {
        cp.top.code.iter().filter(|i| f(i)).count()
    }

    // ── decorators on a function ─────────────────────────────────────────────
    //
    // `fn` and `async fn` had separate copies of this emission, and they had
    // drifted: only the `fn` copy split a namespaced name. They share one path
    // now, so both of these assert the same shape.

    /// The field name of every `GetField` in the top-level chunk.
    fn top_fields(cp: &CompiledProgram) -> Vec<String> {
        cp.top
            .code
            .iter()
            .filter_map(|i| match i {
                Instr::GetField(_, _, f) => Some(f.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_namespaced_decorator_on_a_fn_loads_the_module_then_the_field() {
        let cp = compile_ok(
            "fn tag(f) {\n    return f\n}\n@tools::register\nfn go() {\n    return 1\n}",
        );
        assert!(top_fields(&cp).contains(&"register".to_string()));
    }

    #[test]
    fn a_namespaced_decorator_on_an_async_fn_resolves_the_same_way() {
        // This is the regression: the async copy emitted a bare
        // GetGlobal("tools.register"), looking for a global whose name contains
        // a dot — which nothing can define.
        let cp = compile_ok(
            "fn tag(f) {\n    return f\n}\n@tools::register\nasync fn go() {\n    return 1\n}",
        );
        assert!(top_fields(&cp).contains(&"register".to_string()));
    }

    #[test]
    fn emit_empty_program_has_no_user_declarations() {
        let cp = compile_ok("");
        // An empty program declares no structs, extend methods, or routes; the
        // emitter may still append an implicit trailing instruction.
        assert!(cp.struct_defs.is_empty());
        assert!(cp.extend_methods.is_empty());
        assert!(cp.route_configs.is_empty());
    }

    #[test]
    fn emit_int_let_loads_int_and_sets_global() {
        let cp = compile_ok("let x = 7");
        assert!(
            count_top(&cp, |i| matches!(i, Instr::LoadInt(_, 7))) >= 1,
            "expected a LoadInt(_, 7)"
        );
        assert!(
            count_top(&cp, |i| matches!(i, Instr::SetGlobal(n, _) if n == "x")) == 1,
            "expected exactly one SetGlobal(x)"
        );
    }

    #[test]
    fn emit_float_let_loads_float() {
        let cp = compile_ok("let y = 3.5");
        assert!(count_top(&cp, |i| matches!(i, Instr::LoadFloat(_, f) if *f == 3.5)) >= 1);
    }

    #[test]
    fn emit_bool_and_str_and_nil_literals() {
        let cp = compile_ok("let a = true\nlet b = \"hi\"\nlet c = nil");
        assert!(count_top(&cp, |i| matches!(i, Instr::LoadBool(_, true))) >= 1);
        assert!(count_top(&cp, |i| matches!(i, Instr::LoadStr(_, s) if s == "hi")) >= 1);
        assert!(count_top(&cp, |i| matches!(i, Instr::LoadNil(_))) >= 1);
    }

    #[test]
    fn emit_int_add_uses_typed_addint() {
        // Both operands are statically Int → the emitter must pick the typed op,
        // not the dynamic BinOp fallback.
        let cp = compile_ok("let x = 1 + 2");
        assert!(
            count_top(&cp, |i| matches!(i, Instr::AddInt(..))) >= 1,
            "int + int must lower to AddInt"
        );
        assert_eq!(
            count_top(&cp, |i| matches!(i, Instr::BinOp(..))),
            0,
            "typed add must not fall back to dynamic BinOp"
        );
    }

    #[test]
    fn emit_float_add_uses_typed_addfloat() {
        let cp = compile_ok("let x = 1.0 + 2.0");
        assert!(count_top(&cp, |i| matches!(i, Instr::AddFloat(..))) >= 1);
    }

    #[test]
    fn emit_int_float_mix_promotes_with_inttofloat() {
        // int + float → int operand widened before AddFloat.
        let cp = compile_ok("let x = 1 + 2.0");
        assert!(
            count_top(&cp, |i| matches!(i, Instr::IntToFloat(..))) >= 1,
            "mixed arithmetic must widen the int"
        );
        assert!(count_top(&cp, |i| matches!(i, Instr::AddFloat(..))) >= 1);
    }

    #[test]
    fn emit_str_concat_uses_concatstr() {
        let cp = compile_ok(r#"let s = "a" + "b""#);
        assert!(count_top(&cp, |i| matches!(i, Instr::ConcatStr(..))) >= 1);
    }

    #[test]
    fn emit_if_produces_conditional_jump() {
        let cp = compile_ok("let x = 0\nif true { x = 1 }");
        assert!(
            count_top(&cp, |i| matches!(i, Instr::JumpIfFalse(..))) >= 1,
            "an if must emit a JumpIfFalse guard"
        );
    }

    #[test]
    fn emit_while_produces_backward_jump() {
        let cp = compile_ok("let i = 0\nwhile i < 3 { i = i + 1 }");
        // The loop back-edge is an unconditional Jump with a negative offset.
        let has_back_jump = cp.top.code.iter().any(|i| matches!(i, Instr::Jump(o) if *o < 0));
        assert!(has_back_jump, "while loop must emit a backward Jump");
    }

    #[test]
    fn emit_fndef_interns_a_function() {
        let cp = compile_ok("fn f() { return 1 }");
        assert_eq!(cp.top.fn_defs.len(), 1, "one fn def should be interned");
        assert_eq!(cp.top.fn_defs[0].chunk.name, "f");
    }

    #[test]
    fn emit_structdef_records_field_defs() {
        let cp = compile_ok("struct Point { x, y }");
        let fields = cp.struct_defs.get("Point").expect("Point struct recorded");
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn emit_call_of_builtin_emits_call_instr() {
        let cp = compile_ok(r#"print("hi")"#);
        assert!(
            count_top(&cp, |i| matches!(i, Instr::Call(..))) >= 1,
            "a function call must lower to a Call instruction"
        );
    }

    #[test]
    fn emit_array_literal_emits_makearray() {
        let cp = compile_ok("let a = [1, 2, 3]");
        assert!(count_top(&cp, |i| matches!(i, Instr::MakeArray(..))) >= 1);
    }

    #[test]
    fn emit_top_n_slots_is_nonzero_when_registers_used() {
        let cp = compile_ok("let x = 1 + 2 + 3");
        assert!(cp.top_n_slots > 0, "arithmetic must allocate register slots");
    }

    #[test]
    fn emit_extend_block_registers_methods() {
        let src = "struct P { x }\nextend P {\n fn get(self) { return self.x }\n}";
        let cp = compile_ok(src);
        let methods = cp.extend_methods.get("P").expect("extend methods for P");
        assert!(methods.contains_key("get"), "method `get` should be registered");
    }
}

mod tir {
    use crate::compiler::tir::{JadeType, TExpr, TExprKind, TProgram, TStmt};
    use crate::frontend::error::Span;

    fn sp() -> Span {
        Span { line: 1, col: 1 }
    }

    #[test]
    fn jadetype_equality_and_clone() {
        assert_eq!(JadeType::Int, JadeType::Int);
        assert_ne!(JadeType::Int, JadeType::Float);
        let t = JadeType::Array(Box::new(JadeType::Str));
        assert_eq!(t.clone(), JadeType::Array(Box::new(JadeType::Str)));
        assert_ne!(t, JadeType::Array(Box::new(JadeType::Int)));
    }

    #[test]
    fn nested_array_type_equality() {
        let a = JadeType::Array(Box::new(JadeType::Array(Box::new(JadeType::Int))));
        let b = JadeType::Array(Box::new(JadeType::Array(Box::new(JadeType::Int))));
        assert_eq!(a, b);
    }

    #[test]
    fn struct_type_carries_name() {
        assert_eq!(JadeType::Struct("Point".into()), JadeType::Struct("Point".into()));
        assert_ne!(JadeType::Struct("Point".into()), JadeType::Struct("Line".into()));
    }

    #[test]
    fn fn_type_compares_params_and_ret() {
        let f1 = JadeType::Fn { params: vec![JadeType::Int], ret: Box::new(JadeType::Bool) };
        let f2 = JadeType::Fn { params: vec![JadeType::Int], ret: Box::new(JadeType::Bool) };
        let f3 = JadeType::Fn { params: vec![JadeType::Float], ret: Box::new(JadeType::Bool) };
        assert_eq!(f1, f2);
        assert_ne!(f1, f3);
    }

    #[test]
    fn future_wraps_inner_type() {
        let fut = JadeType::Future(Box::new(JadeType::Int));
        assert_eq!(fut, JadeType::Future(Box::new(JadeType::Int)));
        assert_ne!(fut, JadeType::Int);
    }

    #[test]
    fn jadetype_serde_roundtrip_preserves_variant() {
        for t in [
            JadeType::Int,
            JadeType::Float,
            JadeType::Bool,
            JadeType::Str,
            JadeType::Nil,
            JadeType::Prompt,
            JadeType::Grammar,
            JadeType::Dict,
            JadeType::Unknown,
            JadeType::Array(Box::new(JadeType::Int)),
            JadeType::Struct("Foo".into()),
            JadeType::Future(Box::new(JadeType::Str)),
        ] {
            let json = serde_json::to_string(&t).expect("serialize");
            let back: JadeType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(t, back, "roundtrip must preserve the type");
        }
    }

    #[test]
    fn texpr_and_program_serde_roundtrip() {
        let expr = TExpr { kind: TExprKind::Integer(42), ty: JadeType::Int, span: sp() };
        let prog =
            TProgram { stmts: vec![TStmt::Let { name: "x".into(), value: expr, span: sp() }] };
        let json = serde_json::to_string(&prog).expect("serialize program");
        let back: TProgram = serde_json::from_str(&json).expect("deserialize program");
        assert_eq!(back.stmts.len(), 1);
        match &back.stmts[0] {
            TStmt::Let { name, value, .. } => {
                assert_eq!(name, "x");
                assert_eq!(value.ty, JadeType::Int);
                match value.kind {
                    TExprKind::Integer(n) => assert_eq!(n, 42),
                    ref other => panic!("expected Integer, got {:?}", other),
                }
            }
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn texpr_carries_type_and_span() {
        let e = TExpr {
            kind: TExprKind::Bool(true),
            ty: JadeType::Bool,
            span: Span { line: 4, col: 9 },
        };
        assert_eq!(e.ty, JadeType::Bool);
        assert_eq!(e.span.line, 4);
        assert_eq!(e.span.col, 9);
    }
}
