//! Unit tests for the type checker: the per-file pass, and the types the
//! whole-program checker gives the programs the per-file pass used to type.
//!
//! Until 2.0.6 the per-file pass gave every node a type of its own, and most of
//! these tests read that type. The whole-program checker in `infer/` works out
//! every type once, so those tests now ask it instead (`mod types`), and the
//! ones that read a rewritten shape, a flattened pipe or a typed dereference,
//! ask the per-file pass (`mod resolve`). The VM and bytecode tests used to
//! live here too; they left with their modules.
#![allow(clippy::all)]

mod resolve {
    use crate::frontend::ast::{Expr, ExprKind, Program, Stmt};
    use crate::frontend::error::{JadeError, Result};
    use crate::frontend::{lexer, parser};
    use crate::typechecker::resolve::resolve;

    fn resolve_src(src: &str) -> Result<Program> {
        let tokens = lexer::tokenize(src).expect("lex");
        let program = parser::parse(tokens).expect("parse");
        resolve(program)
    }

    fn resolve_ok(src: &str) -> Program {
        resolve_src(src).expect("the per-file pass failed")
    }

    fn resolve_err(src: &str) -> JadeError {
        resolve_src(src).expect_err("expected an error from the per-file pass")
    }

    /// The value of the `let` at index `i`.
    fn let_value(p: &Program, i: usize) -> &Expr {
        let Stmt::Let { value, .. } = &p.stmts[i] else { panic!("expected Let at {i}") };
        value
    }

    // ── Classes ───────────────────────────────────────────────────────────────

    #[test]
    fn a_literal_of_an_undefined_class_is_an_error() {
        let err = resolve_err("let p = Foo { x: 1 }");
        assert!(matches!(err, JadeError::UndefinedType { .. }));
    }

    #[test]
    fn a_literal_missing_a_required_field_is_an_error() {
        let err = resolve_err("class P {\n @public x,\n @public y\n}\nlet p = P { x: 1 }");
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn a_literal_naming_a_field_the_class_lacks_is_an_error() {
        let err = resolve_err("class P {\n @public x\n}\nlet p = P { x: 1, z: 2 }");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
        let err = resolve_err("class Unit {}\nlet u = Unit { x: 1 }");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    #[test]
    fn a_literal_naming_a_field_twice_is_an_error() {
        let err = resolve_err("class P {\n @public x\n}\nlet p = P { x: 1, x: 2 }");
        assert!(matches!(err, JadeError::DuplicateField { .. }));
    }

    /// A literal of a class this file defines comes out complete: every field
    /// in definition order, the defaults filled in, a prompt field marked, and
    /// what the source wrote kept in `given`.
    #[test]
    fn a_literal_is_completed_in_definition_order() {
        let p = resolve_ok(
            "class A {\n @public b,\n @public let a = 1,\n @public prompt q = \"hi\"\n}\nlet x = A { b: 2 }",
        );
        let ExprKind::StructLiteral { fields, given, base, .. } = &let_value(&p, 1).kind else {
            panic!("expected a class literal")
        };
        assert!(base.is_none());
        let names: Vec<(&str, bool)> = fields.iter().map(|(n, _, pr)| (n.as_str(), *pr)).collect();
        assert_eq!(names, vec![("b", false), ("a", false), ("q", true)]);
        assert_eq!(given, &vec!["b".to_string()]);
    }

    /// With a base, a field the literal leaves out is not filled in: whether
    /// the base or the default supplies it depends on the base's class, which
    /// the whole-program checker decides.
    #[test]
    fn a_literal_with_a_base_holds_only_what_it_names() {
        let p = resolve_ok(
            "class A {\n @public b,\n @public let a = 1\n}\nlet x = A { b: 2 }\nlet y = A { ...x, b: 3 }",
        );
        let ExprKind::StructLiteral { fields, base, .. } = &let_value(&p, 2).kind else {
            panic!("expected a class literal")
        };
        assert!(base.is_some());
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "b");
    }

    /// A class's own definition carries its fields flattened, inherited ones
    /// first.
    #[test]
    fn a_child_class_carries_its_parents_fields() {
        let p = resolve_ok("class A {\n @public a\n}\nclass B(A) {\n @public b\n}");
        let Stmt::StructDef { fields, .. } = &p.stmts[1] else { panic!("expected StructDef") };
        let names: Vec<&str> = fields.iter().map(|f| f.name()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    /// A default that builds its own class would be filled in forever, so it
    /// is refused with the fix.
    #[test]
    fn a_default_that_builds_its_own_class_is_refused() {
        let err = resolve_err("class Node {\n @public let next = Node {}\n}\nlet n = Node {}");
        let m = err.to_string();
        assert!(m.contains("builds another Node"), "got {m}");
        assert!(m.contains("such as nil"), "the message names the fix: {m}");
    }

    // ── Imports ───────────────────────────────────────────────────────────────

    #[test]
    fn a_quoted_file_import_is_an_error() {
        // Quoted file imports were removed — use a bare/`::` module name instead.
        let err = resolve_err(r#"use "my_lib.jde""#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
    }

    #[test]
    fn a_bare_module_import_is_ok() {
        // A sibling `.jde` file is imported by bare module name; resolution is
        // deferred to run/build, so the per-file pass accepts it.
        resolve_ok("use my_lib");
        resolve_ok("use sub::helper");
    }

    #[test]
    fn an_import_alias_is_an_error() {
        let err = resolve_err("use my_lib as m");
        assert!(matches!(err, JadeError::ImportAlias { .. }));
    }

    #[test]
    fn a_std_import_is_ok() {
        resolve_ok("use std::math");
    }

    #[test]
    fn a_quoted_std_import_is_an_error() {
        let err = resolve_err(r#"use "std/math""#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
    }

    #[test]
    fn a_quoted_from_use_is_an_error() {
        let err = resolve_err(r#"from "std/math" use floor"#);
        assert!(matches!(err, JadeError::QuotedImport { .. }));
    }

    #[test]
    fn a_from_use_of_a_std_package_is_ok() {
        resolve_ok("from std::math use floor");
    }

    // ── Names ─────────────────────────────────────────────────────────────────

    #[test]
    fn a_prompt_declaration_is_kept() {
        let p = resolve_ok(r#"prompt p = "hello""#);
        assert!(matches!(&p.stmts[0], Stmt::PromptDecl { .. }));
    }

    #[test]
    fn an_undefined_variable_is_an_error() {
        let err = resolve_err("let x = y + 1");
        assert!(matches!(err, JadeError::UndefinedVariable { .. }));
    }

    /// A std import must not turn the check off.
    ///
    /// It used to: any `use` at all made every unknown name unknown, so
    /// `exit(main())` passed, emitted a read of a nil global, and built a
    /// binary that did nothing. A std package contributes its own name, so
    /// after one there is still nothing this pass cannot account for.
    #[test]
    fn a_std_import_does_not_excuse_an_undefined_variable() {
        let err = resolve_err("use std::env\nfn main() { return 0 }\nexit(main())");
        assert!(
            matches!(&err, JadeError::UndefinedVariable { name, .. } if name == "exit"),
            "expected undefined 'exit', got {err}"
        );
        assert!(
            err.to_string().contains("raise"),
            "the message should name what to write instead: {err}"
        );
    }

    /// The other half: leniency is kept where it is actually needed. A user
    /// module's top-level names only exist once the importer merges the
    /// modules, well after this pass, so a bare name has to be allowed through
    /// here. The checker catches an unbound one after the merge.
    #[test]
    fn a_user_module_import_stays_lenient() {
        resolve_ok("use some_helper\nlet x = whatever_it_exports()");
    }

    /// `from … use a, b` binds bare names this pass never sees, std or not.
    #[test]
    fn a_from_use_stays_lenient() {
        resolve_ok("from std::env use args\nlet x = args()");
    }

    // ── Std package contents ──────────────────────────────────────────────────

    /// A package's functions are known at compile time, so a name it does not
    /// have is settled here. It used to survive to run time and surface as
    /// `struct 'dict' has no field 'round'` — a package was a dict at run time,
    /// so the report named neither the module nor a type the reader would know.
    ///
    /// The name is deliberately one nothing will ever provide, rather than one
    /// that is merely missing today.
    #[test]
    fn an_unknown_package_function_names_the_module() {
        let err = resolve_err("use std::math\nlet v = 2.5\nlet r = math.frobnicate(v)");
        let JadeError::UnknownPackageFn { package, name, available, .. } = &err else {
            panic!("expected UnknownPackageFn, got {err}");
        };
        assert_eq!(package, "std::math");
        assert_eq!(name, "frobnicate");
        assert!(available.contains(&"floor".to_string()), "lists what it does have: {available:?}");
    }

    /// A local that shadows the package name is not checked against it.
    #[test]
    fn a_shadowed_package_is_not_checked() {
        resolve_ok("use std::math\nlet math = {\"round\": 1}\nlet r = math.round");
    }

    /// Only an imported package. A bare `math.round` with no `use` is an
    /// undefined variable, which is a different mistake and already reported.
    #[test]
    fn the_package_check_needs_the_import() {
        let err = resolve_err("let r = math.round(2.5)");
        assert!(
            matches!(&err, JadeError::UndefinedVariable { name, .. } if name == "math"),
            "expected undefined 'math', got {err}"
        );
    }

    // ── Pipes and typed dereferences ──────────────────────────────────────────
    //
    // A pipe calls its stages once per piece of its source, so a whole chain
    // becomes one `ExprKind::Pipe`. A typed dereference is a call around the
    // whole reply instead, `toInt(?p)`. See `design/v2-pipes.md`.

    /// The pipe a `let` holds, with its parts.
    fn pipe_of(value: &Expr) -> (&Expr, &Vec<(String, Expr)>, &str, &Expr) {
        let ExprKind::Pipe { source, hoisted, piece, body } = &value.kind else {
            panic!("expected Pipe, got {:?}", value.kind)
        };
        (source, hoisted, piece, body)
    }

    /// A converter around a whole dereference asks the model for that type, so
    /// it folds into the dereference as an `output_type`.
    #[test]
    fn a_converter_around_a_deref_becomes_an_output_type() {
        for (conv, ty) in [("toInt", "int"), ("toChar", "char"), ("toBool", "bool")] {
            let p = resolve_ok(&format!("prompt p = \"x\"\nlet n = {conv}(?p)"));
            let ExprKind::PromptDeref { output_type, grammar_expr, .. } = &let_value(&p, 1).kind
            else {
                panic!("expected PromptDeref, got {:?}", let_value(&p, 1).kind)
            };
            assert_eq!(output_type.as_deref(), Some(ty));
            assert!(grammar_expr.is_none());
        }
    }

    /// A type function only checks, so it does not fold: `int(?p)` is a call,
    /// which the static checker refuses since the reply is a str.
    #[test]
    fn a_type_function_around_a_deref_is_a_call() {
        let p = resolve_ok("prompt p = \"x\"\nlet n = int(?p)");
        assert!(matches!(let_value(&p, 1).kind, ExprKind::Call { .. }));
    }

    /// A class is a name in scope, so by name alone it looks like something to
    /// call; around a dereference it is a type, not a class view.
    #[test]
    fn a_class_around_a_deref_is_a_type() {
        let p = resolve_ok("class City {\n    @public name\n}\nprompt p = \"x\"\nlet c = City(?p)");
        let ExprKind::PromptDeref { output_type, .. } = &let_value(&p, 2).kind else {
            panic!("expected PromptDeref, got {:?}", let_value(&p, 2).kind)
        };
        assert_eq!(output_type.as_deref(), Some("City"));
    }

    #[test]
    fn a_grammar_around_a_deref_becomes_a_grammar_expr() {
        let p =
            resolve_ok("let g = Grammar.new('\"yes\" | \"no\"')\nprompt p = \"x\"\nlet r = g(?p)");
        let ExprKind::PromptDeref { output_type, grammar_expr, .. } = &let_value(&p, 2).kind else {
            panic!("expected PromptDeref, got {:?}", let_value(&p, 2).kind)
        };
        assert!(output_type.is_none());
        assert!(grammar_expr.is_some());
    }

    /// The old spellings of a typed dereference would call the stage on each
    /// piece of the reply, which is almost never meant, so each is refused with
    /// the call that applies it to the whole reply.
    #[test]
    fn a_whole_reply_stage_piped_after_a_deref_names_the_call() {
        for (src, fix) in [
            ("prompt p = \"x\"\nlet n = ?p |> toInt", "toInt(?p)"),
            ("prompt p = \"x\"\nlet n = ?p |> int", "toInt(?p)"),
            ("prompt p = \"x\"\nlet c = ?p |> toChar", "toChar(?p)"),
            ("prompt p = \"x\"\nlet n = ?p |> byte", "toByte(?p)"),
            ("class City {\n    @public name\n}\nprompt p = \"x\"\nlet c = ?p |> City", "City(?p)"),
            ("let g = Grammar.new('\"a\"')\nprompt p = \"x\"\nlet r = ?p |> g", "g(?p)"),
        ] {
            let err = resolve_err(src);
            let JadeError::WholeReplyStage { fix: got, .. } = &err else {
                panic!("expected WholeReplyStage for {src}, got {err}")
            };
            assert_eq!(got, fix, "for {src}");
        }
    }

    /// A function after a dereference runs on each piece of the reply: the
    /// piece is the stage's first argument.
    #[test]
    fn a_function_piped_after_a_deref_runs_per_piece() {
        let p = resolve_ok("fn shout(s) { return s }\nprompt p = \"x\"\nlet r = ?p |> shout");
        let (source, _, piece, body) = pipe_of(let_value(&p, 2));
        assert!(matches!(source.kind, ExprKind::PromptDeref { output_type: None, .. }));
        let ExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert!(matches!(&args[0].kind, ExprKind::Identifier(n) if n == piece));
    }

    /// A typed dereference is one piece, so it is the pipe's source whole.
    #[test]
    fn a_function_piped_after_a_typed_deref_receives_the_converted_value() {
        let p = resolve_ok(
            "fn double(x) { return x * 2 }\nprompt p = \"x\"\nlet n = toInt(?p) |> double",
        );
        let (source, _, _, body) = pipe_of(let_value(&p, 2));
        assert!(matches!(
            &source.kind,
            ExprKind::PromptDeref { output_type: Some(t), .. } if t == "int"
        ));
        let ExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert_eq!(args.len(), 1, "the piece is the sole argument");
    }

    /// A plain value is one piece, and the pipe gives back the stage's result.
    #[test]
    fn a_function_stage_on_an_ordinary_value_is_one_call() {
        let p = resolve_ok("fn double(x) { return x * 2 }\nlet n = 5 |> double");
        let (source, hoisted, _, body) = pipe_of(let_value(&p, 1));
        assert!(matches!(source.kind, ExprKind::Integer(5)));
        assert!(hoisted.is_empty());
        let ExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert_eq!(args.len(), 1);
    }

    /// The piece goes in front of the stage's own arguments, and a literal
    /// argument stays where it is, so it still takes the type it meets.
    #[test]
    fn a_call_stage_takes_the_piece_as_its_first_argument() {
        let p = resolve_ok("fn add(a, b) { return a + b }\nlet n = 5 |> add(3)");
        let (_, hoisted, piece, body) = pipe_of(let_value(&p, 1));
        assert!(hoisted.is_empty());
        let ExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert_eq!(args.len(), 2);
        assert!(matches!(&args[0].kind, ExprKind::Identifier(n) if n == piece));
        assert!(matches!(args[1].kind, ExprKind::Integer(3)));
    }

    /// A stage argument that is neither a literal nor a name is worked out
    /// once, before the first piece, not once per piece.
    #[test]
    fn a_stage_argument_is_worked_out_once() {
        let p = resolve_ok(
            "fn add(a, b) { return a + b }\nfn ten() { return 10 }\nlet n = 5 |> add(ten())",
        );
        let (_, hoisted, _, body) = pipe_of(let_value(&p, 2));
        assert_eq!(hoisted.len(), 1);
        assert!(matches!(hoisted[0].1.kind, ExprKind::Call { .. }));
        let ExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert!(matches!(&args[1].kind, ExprKind::Identifier(n) if *n == hoisted[0].0));
    }

    /// A chain is one pipe, so each piece goes through every stage before the
    /// next piece is made.
    #[test]
    fn a_chain_is_one_pipe() {
        let p = resolve_ok(
            "fn double(x) { return x * 2 }\nfn inc(x) { return x + 1 }\nlet n = 5 |> double |> inc",
        );
        let (source, _, piece, body) = pipe_of(let_value(&p, 2));
        assert!(matches!(source.kind, ExprKind::Integer(5)));
        let ExprKind::Call { callee, args, .. } = &body.kind else { panic!("expected Call") };
        assert!(matches!(&callee.kind, ExprKind::Identifier(n) if n == "inc"));
        let ExprKind::Call { callee, args: inner, .. } = &args[0].kind else {
            panic!("expected the inner stage to be a Call")
        };
        assert!(matches!(&callee.kind, ExprKind::Identifier(n) if n == "double"));
        assert!(matches!(&inner[0].kind, ExprKind::Identifier(n) if n == piece));
    }

    /// A stage this file can see is not a function is refused here: a literal,
    /// or a name bound to a prompt or a grammar. A name bound to any other
    /// value may hold a function, so the checker decides (`mod types`).
    #[test]
    fn a_stage_that_is_plainly_not_a_function_is_an_error() {
        for src in [
            "let x = 5 |> 3",
            "let x = 5 |> \"nope\"",
            "prompt p = \"x\"\nlet x = 5 |> p",
            "let g = Grammar.new('\"a\"')\nlet x = 5 |> g",
        ] {
            assert!(
                matches!(resolve_err(src), JadeError::InvalidPipeStage { .. }),
                "expected InvalidPipeStage for {src}",
            );
        }
    }

    /// No parser-only node survives the pass, wherever a pipe was written:
    /// nested, as an argument, inside a function, or in a class default.
    #[test]
    fn the_per_file_pass_leaves_no_raw_pipe() {
        fn walk(e: &Expr) {
            assert!(!matches!(e.kind, ExprKind::RawPipe { .. }), "a raw pipe survived: {e:?}");
            match &e.kind {
                ExprKind::Call { callee, args, kwargs } => {
                    walk(callee);
                    args.iter().for_each(walk);
                    kwargs.iter().for_each(|(_, v)| walk(v));
                }
                ExprKind::Pipe { source, hoisted, body, .. } => {
                    walk(source);
                    hoisted.iter().for_each(|(_, v)| walk(v));
                    walk(body);
                }
                ExprKind::BinOp { left, right, .. } => {
                    walk(left);
                    walk(right);
                }
                ExprKind::StructLiteral { fields, base, .. } => {
                    fields.iter().for_each(|(_, v, _)| walk(v));
                    base.iter().for_each(|b| walk(b));
                }
                ExprKind::Array { elements } => elements.iter().for_each(walk),
                _ => {}
            }
        }
        fn walk_stmts(stmts: &[Stmt]) {
            for s in stmts {
                match s {
                    Stmt::Let { value, .. } | Stmt::Expr(value) => walk(value),
                    Stmt::Return { value: Some(v), .. } => walk(v),
                    Stmt::FnDef { body, .. } => walk_stmts(body),
                    Stmt::StructDef { fields, .. } => {
                        for f in fields {
                            if let crate::frontend::ast::StructFieldDef::Let { default, .. } = f {
                                walk(default);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let p = resolve_ok(
            "fn double(x) { return x * 2 }\n\
             fn f(n) { return n |> double |> double }\n\
             class A {\n @public let d = 2 |> double\n}\n\
             let a = A {}\n\
             let xs = [1 |> double, f(3 |> double)]\n\
             print((4 |> double) + 1)",
        );
        walk_stmts(&p.stmts);
    }

    // ── yield / streams ───────────────────────────────────────────────────────

    /// A generator produces a stream, so returning a value as well asks it to be
    /// two things. A *bare* return is fine — it stops the generator early.
    #[test]
    fn a_generator_cannot_also_return_a_value() {
        let err = resolve_err("fn g() {\n    yield 1\n    return 2\n}");
        assert!(matches!(err, JadeError::YieldAndReturn { .. }), "got {err:?}");
        resolve_ok("fn g(n) {\n    yield 1\n    if n > 0 {\n        return\n    }\n    yield 2\n}");
    }
}

mod types {
    use crate::frontend::error::JadeError;
    use crate::typechecker::infer::{Ty, Typed, check_members, check_program};

    /// Purpose: This function runs a program through the frontend, the
    /// per-file pass and the whole-program checker, the way a build does,
    /// prompt dereferences rewritten into the inference prelude included.
    fn check(src: &str) -> Result<Typed, JadeError> {
        let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
        let program = crate::frontend::parser::parse(tokens).expect("parses");
        let mut p = crate::typechecker::resolve::resolve(program)?;
        check_members(&p)?;
        crate::typechecker::resolve::fill_struct_literal_defaults(&mut p)?;
        p.stmts = crate::infer::rewrite::inject(p.stmts).expect("the prelude compiles");
        // The checker's answer is keyed by node address, so the program must
        // outlive it; a leak keeps the test short.
        let p: &'static _ = Box::leak(Box::new(p));
        check_program(p)
    }

    fn ok(src: &str) -> Typed {
        match check(src) {
            Ok(t) => t,
            Err(e) => panic!("expected the program to check, got: {e}\n{src}"),
        }
    }

    fn rejects(src: &str, want: &str) {
        match check(src) {
            Ok(_) => panic!("expected an error containing '{want}', but it checked:\n{src}"),
            Err(e) => {
                let m = e.to_string();
                assert!(m.contains(want), "expected '{want}' in the error, got: {m}");
            }
        }
    }

    fn global(t: &Typed, name: &str) -> Ty {
        t.globals.get(name).cloned().unwrap_or_else(|| panic!("no global {name}"))
    }

    fn stream(t: Ty) -> Ty {
        Ty::Stream(Box::new(t))
    }

    // ── Literals ──────────────────────────────────────────────────────────────

    #[test]
    fn literals_have_their_types() {
        assert_eq!(global(&ok("let x = 42"), "x"), Ty::Int);
        assert_eq!(global(&ok("let x = 3.14"), "x"), Ty::Float);
        assert_eq!(global(&ok("let x = true"), "x"), Ty::Bool);
        assert_eq!(global(&ok(r#"let x = "hello""#), "x"), Ty::Str);
    }

    /// `null` and `None` are spellings of nil alongside `nil`.
    #[test]
    fn every_spelling_of_nil_is_nil() {
        for src in ["let x = null", "let x = None", "let x = nil"] {
            assert_eq!(global(&ok(src), "x"), Ty::Nil, "failed for: {src}");
        }
    }

    /// `bool` checks a value is a bool and never converts, so `bool(nil)` is
    /// refused with the converter that would.
    #[test]
    fn a_type_function_on_nil_names_the_converter() {
        rejects("let x = bool(nil)\nprint(x)", "toBool()");
    }

    // ── Arithmetic ────────────────────────────────────────────────────────────

    #[test]
    fn arithmetic_has_the_operands_type() {
        assert_eq!(global(&ok("let x = 1 + 2"), "x"), Ty::Int);
        assert_eq!(global(&ok("let x = 1.0 + 2.0"), "x"), Ty::Float);
        assert_eq!(global(&ok(r#"let x = "a" + "b""#), "x"), Ty::Str);
    }

    /// A literal takes the type it meets, so `1 + 2.0` is a float sum.
    #[test]
    fn an_int_literal_meeting_a_float_is_a_float() {
        assert_eq!(global(&ok("let x = 1 + 2.0"), "x"), Ty::Float);
    }

    #[test]
    fn an_int_plus_a_str_is_an_error() {
        rejects(r#"let x = 1 + "a""#, "'+' cannot combine int and str");
    }

    // ── Logical and comparison ────────────────────────────────────────────────

    #[test]
    fn logic_and_comparison_give_bools() {
        assert_eq!(global(&ok("let x = true && false"), "x"), Ty::Bool);
        assert_eq!(global(&ok("let x = 1 < 2"), "x"), Ty::Bool);
    }

    #[test]
    fn and_on_ints_is_an_error() {
        rejects("let x = 1 && 0", "&&");
    }

    /// Strict equality: both sides must be one type.
    #[test]
    fn equality_across_types_is_an_error() {
        rejects("let x = 1 == 1.0", "both sides must be the same type");
    }

    // ── Unary operators ───────────────────────────────────────────────────────

    #[test]
    fn unary_operators_keep_their_types() {
        assert_eq!(global(&ok("let x = -5"), "x"), Ty::Int);
        assert_eq!(global(&ok("let x = !true"), "x"), Ty::Bool);
    }

    #[test]
    fn not_on_an_int_is_an_error() {
        rejects("let x = !1", "'!'");
    }

    /// `!x`, `x && y` and `x || y` give a bool whatever the function is
    /// called with, since a parameter's type comes from the call.
    #[test]
    fn logic_on_parameters_gives_a_bool() {
        for src in [
            "fn f(x) {\n return !x\n}\nlet r = f(true)",
            "fn f(x, y) {\n return x && y\n}\nlet r = f(true, false)",
            "fn f(x, y) {\n return x || y\n}\nlet r = f(true, false)",
        ] {
            assert_eq!(global(&ok(src), "r"), Ty::Bool, "failed for: {src}");
        }
    }

    // ── Arrays ────────────────────────────────────────────────────────────────

    #[test]
    fn an_array_has_its_elements_type() {
        assert_eq!(global(&ok("let a = [1, 2, 3]"), "a"), Ty::Array(Box::new(Ty::Int)));
        assert_eq!(global(&ok(r#"let a = ["x", "y"]"#), "a"), Ty::Array(Box::new(Ty::Str)));
    }

    /// An empty array nothing fills holds nil.
    #[test]
    fn an_empty_array_nothing_fills_holds_nil() {
        assert_eq!(global(&ok("let a = []"), "a"), Ty::Array(Box::new(Ty::Nil)));
    }

    /// An array holds one kind of value, so a mixed literal is refused, mixed
    /// numbers included: `[1, 2.0]` names the conversion.
    #[test]
    fn a_mixed_array_is_an_error() {
        rejects(r#"let a = [1, "hello"]"#, "the elements of an array");
        rejects("let a = [1, 2.0]", "convert one");
    }

    /// An element worked out by a call joins the others like any value.
    #[test]
    fn an_element_from_a_call_joins_the_rest() {
        let t = ok("fn f() { return 1 }\nlet a = [1, f(), 3]");
        assert_eq!(global(&t, "a"), Ty::Array(Box::new(Ty::Int)));
    }

    /// A store of another type into an array is refused rather than widening
    /// the element type; a store of the same type keeps it.
    #[test]
    fn storing_another_type_into_an_array_is_an_error() {
        rejects("let a = [1, 2]\na[0] = \"s\"\nlet b = a[0]", "an element of 'a'");
        assert_eq!(global(&ok("let a = [1, 2]\na[0] = 3\nlet b = a[0]"), "b"), Ty::Int);
    }

    // ── Control flow ──────────────────────────────────────────────────────────

    #[test]
    fn a_bool_condition_checks() {
        ok("if true {\n let x = 1\n}");
        ok("while false {\n let x = 1\n}");
    }

    #[test]
    fn an_int_condition_is_an_error() {
        rejects("if 1 {\n let x = 2\n}", "an if condition");
        rejects("while 1 {\n let x = 2\n}", "a while condition");
    }

    /// A `let` inside a block binds a name for that block, in the checker and
    /// in MIR alike, so the outer binding keeps its type after it.
    #[test]
    fn a_let_inside_a_block_shadows_for_the_block() {
        let t = ok("fn f() { let x = 1\n if true { let x = \"s\" }\n return x + 1 }\nlet r = f()");
        assert_eq!(global(&t, "r"), Ty::Int);
    }

    // ── Functions ─────────────────────────────────────────────────────────────

    #[test]
    fn a_call_has_the_functions_return_type() {
        assert_eq!(global(&ok("fn add(x, y) {\n return 42\n}\nlet r = add(1, 2)"), "r"), Ty::Int);
        assert_eq!(global(&ok("fn id(x) {\n return 1\n}\nlet r = id(99)"), "r"), Ty::Int);
    }

    #[test]
    fn a_function_with_no_return_gives_nil() {
        assert_eq!(global(&ok("fn noop(x) {\n let y = 1\n}\nlet r = noop(1)"), "r"), Ty::Nil);
    }

    #[test]
    fn len_gives_an_int() {
        assert_eq!(global(&ok("let n = len([1, 2])"), "n"), Ty::Int);
    }

    /// A function's parameter types come from each call, so its result does.
    #[test]
    fn a_functions_type_follows_its_arguments() {
        let t = ok("fn add(x, y) {\n return x + y\n}\nlet i = add(1, 2)\nlet f = add(1.5, 2.0)");
        assert_eq!(global(&t, "i"), Ty::Int);
        assert_eq!(global(&t, "f"), Ty::Float);
    }

    /// Two return types on two paths are refused, rather than typed as the
    /// first one found, which is what v1 once did.
    #[test]
    fn a_function_returning_two_types_is_an_error() {
        rejects(
            "fn f(c) { if c { return \"s\" }  return 1 }\nprint(f(true))",
            "this function's return values",
        );
    }

    /// A body that returns on only some paths answers nil on the others, so
    /// its result may be nil.
    #[test]
    fn a_conditional_return_joins_the_fall_through() {
        let t = ok("fn f(c) { if c { return 1 } }\nlet r = f(true)");
        assert_eq!(global(&t, "r"), Ty::opt(Ty::Int));
    }

    #[test]
    fn a_function_returning_one_type_everywhere_keeps_it() {
        let t = ok("fn f(c) { if c { return 1 }  return 2 }\nlet r = f(true)");
        assert_eq!(global(&t, "r"), Ty::Int);
    }

    /// A trailing bare expression is a return.
    #[test]
    fn an_implicit_return_is_typed() {
        let t = ok("fn double(x) { x * 2 }\nlet y = double(5) + 1");
        assert_eq!(global(&t, "y"), Ty::Int);
    }

    // ── Classes ───────────────────────────────────────────────────────────────

    #[test]
    fn a_literal_has_its_class() {
        let t = ok("class Point {\n @public x,\n @public y\n}\nlet p = Point { x: 1, y: 2 }");
        assert_eq!(global(&t, "p"), Ty::Struct("Point".to_string()));
        let t = ok("class Unit {}\nlet u = Unit {}");
        assert_eq!(global(&t, "u"), Ty::Struct("Unit".to_string()));
    }

    /// Reading or setting a field the class lacks is refused once the value's
    /// class is known.
    #[test]
    fn a_field_the_class_lacks_is_an_error() {
        let class = "class P {\n @public x\n}\nlet p = P { x: 1 }\n";
        rejects(&format!("{class}print(p.z)"), "class 'P' has no field 'z'");
        rejects(&format!("{class}p.z = 3"), "class 'P' has no field 'z'");
    }

    /// A prompt field holds text.
    #[test]
    fn a_prompt_field_must_be_a_str() {
        let class = "class A {\n @public prompt q = \"x\"\n}\n";
        rejects(&format!("{class}let a = A {{ q: 5 }}"), "a prompt field must be a str");
    }

    /// A copy-with literal reads each field it leaves out from the base when
    /// the base's class has it, and takes the field's default when it does
    /// not, wherever the base comes from.
    #[test]
    fn a_copy_takes_a_default_its_base_lacks() {
        let classes = "class Bare {\n @public name\n}\n\
                       class Full {\n @public name,\n @public let rank = 7\n}\n\
                       fn make() { return Bare { name: \"n\" } }\n";
        ok(&format!("{classes}let a = Full {{ ...Bare {{ name: \"n\" }} }}\nprint(a.rank)"));
        ok(&format!("{classes}let b = Full {{ ...make() }}\nprint(b.rank)"));
        rejects(
            "class Bare {\n @public name\n}\nclass Full {\n @public name,\n @public id\n}\n\
             let f = Full { ...Bare { name: \"n\" } }",
            "set it in the literal",
        );
    }

    // ── Prompts ───────────────────────────────────────────────────────────────

    #[test]
    fn a_prompts_text_must_be_a_str() {
        rejects("prompt p = 42", "a prompt's text must be a str");
    }

    #[test]
    fn a_dereference_gives_a_str_or_the_type_asked_for() {
        assert_eq!(global(&ok("prompt p = \"hi\"\nlet r = ?p"), "r"), Ty::Str);
        assert_eq!(global(&ok("prompt p = \"hi\"\nlet r = toInt(?p)"), "r"), Ty::Int);
        for (conv, want) in [("toChar", Ty::Char), ("toBool", Ty::Bool)] {
            let t = ok(&format!("prompt p = \"x\"\nlet n = {conv}(?p)"));
            assert_eq!(global(&t, "n"), want, "for {conv}");
        }
    }

    #[test]
    fn a_grammar_constrained_dereference_gives_a_str() {
        let t = ok("let g = Grammar.new('\"yes\" | \"no\"')\nprompt p = \"x\"\nlet r = g(?p)");
        assert_eq!(global(&t, "r"), Ty::Str);
    }

    // ── Dicts ─────────────────────────────────────────────────────────────────

    #[test]
    fn indexing_a_dict_gives_its_value_type() {
        assert_eq!(global(&ok("let d = {\"ok\": true}\nlet r = d[\"ok\"]"), "r"), Ty::Bool);
        assert_eq!(global(&ok("let d = {\"a\": 1, \"b\": 2}\nlet r = d[\"a\"]"), "r"), Ty::Int);
    }

    /// A dict holds one kind of value, however the mix arrives: a literal, a
    /// reassignment, or a store.
    #[test]
    fn a_dict_of_mixed_values_is_an_error() {
        rejects("let d = {\"name\": \"jade\", \"version\": 1}", "the values of a dict");
        rejects("let d = {\"ok\": true}\nd = {\"a\": 1, \"b\": \"x\"}", "the values of a dict");
        rejects("let d = {\"a\": true}\nd[\"b\"] = 5", "a value of 'd'");
    }

    // ── Pipes ─────────────────────────────────────────────────────────────────

    /// A function after a dereference runs on each piece of the reply, a str,
    /// and the pipe is a stream of what it gives.
    #[test]
    fn a_pipe_after_a_deref_is_a_stream() {
        let t = ok("fn shout(s) { return s }\nprompt p = \"x\"\nlet r = ?p |> shout");
        assert_eq!(global(&t, "r"), stream(Ty::Str));
    }

    /// A typed dereference is one piece, so the stage gets the int.
    #[test]
    fn a_pipe_after_a_typed_deref_gets_the_converted_value() {
        let t = ok("fn double(x) { return x * 2 }\nprompt p = \"x\"\nlet n = toInt(?p) |> double");
        assert_eq!(global(&t, "n"), Ty::Int);
    }

    /// A plain value is one piece, and the pipe gives back the stage's result.
    #[test]
    fn a_pipe_over_a_plain_value_is_not_a_stream() {
        assert_eq!(global(&ok("fn double(x) { return x * 2 }\nlet n = 5 |> double"), "n"), Ty::Int);
    }

    #[test]
    fn a_pipe_over_a_stream_gives_a_stream() {
        let t =
            ok("fn g() {\n    yield 1\n}\nfn double(x) { return x * 2 }\nlet s = g() |> double");
        assert_eq!(global(&t, "s"), stream(Ty::Int));
    }

    /// A stage bound to a value that is not a function is refused once its
    /// type is known, in the pipe's words.
    #[test]
    fn a_stage_holding_an_int_is_an_error() {
        rejects("let n = 1\nlet x = 5 |> n", "'|>' needs a function on its right");
    }

    // ── char ──────────────────────────────────────────────────────────────────

    #[test]
    fn indexing_a_string_gives_a_char() {
        assert_eq!(global(&ok("let s = \"hi\"\nlet c = s[0]"), "c"), Ty::Char);
    }

    #[test]
    fn iterating_a_string_binds_a_char() {
        let t = ok("let s = \"hi\"\nlet last = s[0]\nfor c in s {\n    last = c\n}");
        assert_eq!(global(&t, "last"), Ty::Char);
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
            assert_eq!(global(&ok(src), "b"), Ty::Bool, "for {src}");
        }
    }

    #[test]
    fn concatenating_a_char_with_a_str_gives_a_str() {
        for src in [
            "let s = \"hi\"\nlet r = s[0] + \"x\"",
            "let s = \"hi\"\nlet r = \"x\" + s[0]",
            "let s = \"hi\"\nlet r = s[0] + s[1]",
        ] {
            assert_eq!(global(&ok(src), "r"), Ty::Str, "for {src}");
        }
    }

    // ── yield / streams ───────────────────────────────────────────────────────

    #[test]
    fn a_function_that_yields_returns_a_stream() {
        assert_eq!(global(&ok("fn g() {\n    yield 1\n}\nlet s = g()"), "s"), stream(Ty::Int));
    }

    /// A `yield` anywhere in the body counts, not just at the top level.
    #[test]
    fn a_yield_inside_a_loop_still_makes_a_generator() {
        let t = ok("fn g(n) {\n    while n > 0 {\n        yield n\n    }\n}\nlet s = g(3)");
        assert_eq!(global(&t, "s"), stream(Ty::Int));
    }

    /// A stream holds one kind of value, as an array does.
    #[test]
    fn yielded_types_that_disagree_are_an_error() {
        rejects("fn g() {\n    yield 1\n    yield \"two\"\n}\nprint(g())", "do not mix");
    }

    #[test]
    fn iterating_a_stream_binds_its_element_type() {
        let t = ok("fn g() {\n    yield 1\n}\nlet last = 0\nfor x in g() {\n    last = x\n}");
        assert_eq!(global(&t, "last"), Ty::Int);
    }

    #[test]
    fn a_generator_may_return_bare_to_stop_early() {
        let t = ok(
            "fn g(n) {\n    yield 1\n    if n > 0 {\n        return\n    }\n    yield 2\n}\nlet s = g(0)",
        );
        assert_eq!(global(&t, "s"), stream(Ty::Int));
    }

    // ── Internal errors ───────────────────────────────────────────────────────

    /// A parser-only pipe that skipped the per-file pass is reported as an
    /// internal error at its span, not a panic.
    #[test]
    fn a_raw_pipe_reaching_the_checker_is_an_internal_error() {
        let tokens = crate::frontend::lexer::tokenize("fn d(x) { return x }\nlet n = 5 |> d")
            .expect("lexes");
        let program = crate::frontend::parser::parse(tokens).expect("parses");
        let err = check_program(&program).expect_err("the checker refuses a raw pipe");
        assert!(err.to_string().contains("internal compiler error"), "got {err}");
        assert!(err.to_string().starts_with("[2:"), "carries the span: {err}");
    }
}

mod tree {
    use crate::frontend::ast::{Expr, ExprKind, Program, Stmt};
    use crate::frontend::error::Span;

    fn sp() -> Span {
        Span { line: 1, col: 1 }
    }

    #[test]
    fn a_program_round_trips_through_serde() {
        let expr = Expr::new(ExprKind::Integer(42), sp());
        let prog = Program { stmts: vec![Stmt::Let { name: "x".into(), value: expr, span: sp() }] };
        let json = serde_json::to_string(&prog).expect("serialize program");
        let back: Program = serde_json::from_str(&json).expect("deserialize program");
        assert_eq!(back.stmts.len(), 1);
        match &back.stmts[0] {
            Stmt::Let { name, value, .. } => {
                assert_eq!(name, "x");
                match value.kind {
                    ExprKind::Integer(n) => assert_eq!(n, 42),
                    ref other => panic!("expected Integer, got {:?}", other),
                }
            }
            other => panic!("expected Let, got {:?}", other),
        }
    }

    #[test]
    fn an_expression_carries_its_span() {
        let e = Expr::new(ExprKind::Bool(true), Span { line: 4, col: 9 });
        assert!(matches!(e.kind, ExprKind::Bool(true)));
        assert_eq!(e.span.line, 4);
        assert_eq!(e.span.col, 9);
    }

    /// Every expression kind round-trips through bincode, which is what the
    /// cache writes, the per-file pass's shapes included.
    #[test]
    fn a_resolved_tree_round_trips_through_bincode() {
        let src = "fn double(x) { return x * 2 }\nprompt p = \"x\"\n\
                   let n = toInt(?p) |> double |> double\nlet s = f\"{n}!\"";
        let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
        let program = crate::frontend::parser::parse(tokens).expect("parses");
        let resolved = crate::typechecker::resolve::resolve(program).expect("resolves");
        let bytes = bincode::serialize(&resolved).expect("serialize");
        let back: Program = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(format!("{back:?}"), format!("{resolved:?}"));
    }
}
