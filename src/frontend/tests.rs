//! Unit tests for the `frontend` module.
//!
//! One inline submodule per source file (`lexer`, `parser`, `ast`, `error`).
//! Because `tests.rs` is a sibling of those source submodules, each block reaches
//! its target through the public crate path (`crate::frontend::<sub>::*`).

mod lexer {
    use crate::frontend::error::JadeError;
    use crate::frontend::lexer::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    // ── existing operations ──────────────────────────────────────────────────

    #[test]
    fn test_integer_literal() {
        assert_eq!(kinds("42"), vec![TokenKind::Integer(42), TokenKind::Semicolon, TokenKind::Eof]);
    }

    // The literal is a float to tokenize, not an approximation of pi.
    #[allow(clippy::approx_constant)]
    #[test]
    fn test_float_literal() {
        assert_eq!(
            kinds("3.14"),
            vec![TokenKind::Float(3.14), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_keyword_let() {
        assert_eq!(kinds("let"), vec![TokenKind::Let, TokenKind::Eof]);
    }

    #[test]
    fn test_all_arithmetic_operators() {
        assert_eq!(
            kinds("+ - * / %"),
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_all_bitwise_operators() {
        assert_eq!(
            kinds("& | ^ ~ << >>"),
            vec![
                TokenKind::Ampersand,
                TokenKind::Pipe,
                TokenKind::Caret,
                TokenKind::Tilde,
                TokenKind::LtLt,
                TokenKind::GtGt,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_auto_semicolon_after_integer() {
        assert_eq!(
            kinds("1\n2"),
            vec![
                TokenKind::Integer(1),
                TokenKind::Semicolon,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_auto_semicolon_after_rparen() {
        assert_eq!(
            kinds("(x)\ny"),
            vec![
                TokenKind::LParen,
                TokenKind::Identifier("x".into()),
                TokenKind::RParen,
                TokenKind::Semicolon,
                TokenKind::Identifier("y".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_no_semicolon_after_operator() {
        assert_eq!(
            kinds("1 +\n2"),
            vec![
                TokenKind::Integer(1),
                TokenKind::Plus,
                TokenKind::Integer(2),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_at_token() {
        assert_eq!(kinds("@"), vec![TokenKind::At, TokenKind::Eof]);
    }

    // ── boolean / logical / comparison tokens ────────────────────────────────

    #[test]
    fn test_tokenize_true() {
        assert_eq!(kinds("true"), vec![TokenKind::True, TokenKind::Semicolon, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_false() {
        assert_eq!(kinds("false"), vec![TokenKind::False, TokenKind::Semicolon, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_and() {
        assert_eq!(kinds("&&"), vec![TokenKind::AmpAmp, TokenKind::Eof]);
    }

    #[test]
    fn test_keyword_and() {
        assert_eq!(kinds("and"), vec![TokenKind::AmpAmp, TokenKind::Eof]);
    }

    #[test]
    fn test_keyword_or() {
        assert_eq!(kinds("or"), vec![TokenKind::PipePipe, TokenKind::Eof]);
    }

    #[test]
    fn test_keyword_not() {
        assert_eq!(kinds("not"), vec![TokenKind::Bang, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_single_amp_unchanged() {
        assert_eq!(kinds("&"), vec![TokenKind::Ampersand, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_or() {
        assert_eq!(kinds("||"), vec![TokenKind::PipePipe, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_single_pipe_unchanged() {
        assert_eq!(kinds("|"), vec![TokenKind::Pipe, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_pipe_gt() {
        assert_eq!(kinds("|>"), vec![TokenKind::PipeGt, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_not() {
        assert_eq!(kinds("!"), vec![TokenKind::Bang, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_comparison_ops() {
        assert_eq!(
            kinds("== != < > <= >="),
            vec![
                TokenKind::EqEq,
                TokenKind::BangEq,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::LtEq,
                TokenKind::GtEq,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_auto_semicolon_after_true() {
        assert_eq!(
            kinds("true\nfalse"),
            vec![
                TokenKind::True,
                TokenKind::Semicolon,
                TokenKind::False,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_bare_lt_and_gt() {
        assert_eq!(
            kinds("1 < 2 > 0"),
            vec![
                TokenKind::Integer(1),
                TokenKind::Lt,
                TokenKind::Integer(2),
                TokenKind::Gt,
                TokenKind::Integer(0),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_eq_eq_vs_equals() {
        assert_eq!(kinds("=="), vec![TokenKind::EqEq, TokenKind::Eof]);
        assert_eq!(kinds("="), vec![TokenKind::Equals, TokenKind::Eof]);
    }

    #[test]
    fn test_bang_eq_vs_bang() {
        assert_eq!(kinds("!="), vec![TokenKind::BangEq, TokenKind::Eof]);
        assert_eq!(kinds("!"), vec![TokenKind::Bang, TokenKind::Eof]);
    }

    // ── function / control flow tokens ───────────────────────────────────────

    #[test]
    fn test_tokenize_fn() {
        assert_eq!(kinds("fn"), vec![TokenKind::Fn, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_return() {
        assert_eq!(kinds("return"), vec![TokenKind::Return, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_if_else() {
        assert_eq!(kinds("if else"), vec![TokenKind::If, TokenKind::Else, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_elif() {
        assert_eq!(
            kinds("if elif else"),
            vec![TokenKind::If, TokenKind::Elif, TokenKind::Else, TokenKind::Eof]
        );
    }

    #[test]
    fn test_tokenize_braces() {
        // RBrace is a line terminator, so `{}` on one line inserts a semicolon after `}`
        assert_eq!(
            kinds("{}"),
            vec![TokenKind::LBrace, TokenKind::RBrace, TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_tokenize_comma() {
        assert_eq!(
            kinds("a, b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Comma,
                TokenKind::Identifier("b".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_semicolon_after_rbrace() {
        // `}` at end of line now inserts a semicolon so struct literals and
        // block-ending statements terminate correctly. The parser consumes the
        // inserted semicolon before checking for `else`.
        assert_eq!(
            kinds("}\nelse"),
            vec![TokenKind::RBrace, TokenKind::Semicolon, TokenKind::Else, TokenKind::Eof,]
        );
    }

    #[test]
    fn test_float_requires_digit_after_dot() {
        // `1.` tokenizes as Integer(1) followed by a standalone Dot — not a float literal.
        // Float literals require at least one digit after the decimal point: `1.0`.
        assert_eq!(kinds("1."), vec![TokenKind::Integer(1), TokenKind::Dot, TokenKind::Eof]);
    }

    // ── struct / extend ───────────────────────────────────────────────────────

    #[test]
    fn test_tokenize_struct_keyword() {
        assert_eq!(kinds("struct"), vec![TokenKind::Struct, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_extend_keyword() {
        assert_eq!(kinds("extend"), vec![TokenKind::Extend, TokenKind::Eof]);
    }

    /// `interface` stays reserved even though nothing parses it, so a stale
    /// block gets an error naming the removal rather than a confusing
    /// statement-level one. Same reason `use "path"` still lexes.
    #[test]
    fn interface_is_still_reserved_so_the_error_can_name_the_removal() {
        assert_eq!(kinds("interface"), vec![TokenKind::Interface, TokenKind::Eof]);
    }

    #[test]
    fn test_arrow_is_not_a_token() {
        // Return type annotations were removed; `->` has no special meaning and
        // lexes as the two characters it is made of.
        assert_eq!(kinds("->"), vec![TokenKind::Minus, TokenKind::Gt, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_squiggly_arrow() {
        assert_eq!(kinds("~>"), vec![TokenKind::TildeGt, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_squiggly_arrow_vs_tilde() {
        // `~` alone stays Tilde (bitwise NOT); only `~>` becomes TildeGt
        assert_eq!(kinds("~ ~>"), vec![TokenKind::Tilde, TokenKind::TildeGt, TokenKind::Eof]);
    }

    #[test]
    fn test_tokenize_dot() {
        assert_eq!(
            kinds("a.b"),
            vec![
                TokenKind::Identifier("a".into()),
                TokenKind::Dot,
                TokenKind::Identifier("b".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_tokenize_colon() {
        assert_eq!(kinds(":"), vec![TokenKind::Colon, TokenKind::Eof]);
    }

    // ── triple-quoted and f-strings ──────────────────────────────────────────

    #[test]
    fn test_str_triple_quote_simple() {
        assert_eq!(
            kinds(r#""""hello""""#),
            vec![TokenKind::Str("hello".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_triple_quote_with_inner_quotes() {
        assert_eq!(
            kinds(r#""""he said "hi" to her""""#),
            vec![
                TokenKind::Str(r#"he said "hi" to her"#.into()),
                TokenKind::Semicolon,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn test_fstr_no_interpolation() {
        assert_eq!(
            kinds(r#"f"hello""#),
            vec![
                TokenKind::FStr(vec![RawFStrPart::Literal("hello".into())]),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_fstr_with_single_slot() {
        assert_eq!(
            kinds(r#"f"hi {name}""#),
            vec![
                TokenKind::FStr(vec![
                    RawFStrPart::Literal("hi ".into()),
                    RawFStrPart::Expr("name".into()),
                ]),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_fstr_triple_quote() {
        assert_eq!(
            kinds(r#"f"""hi {name}""""#),
            vec![
                TokenKind::FStr(vec![
                    RawFStrPart::Literal("hi ".into()),
                    RawFStrPart::Expr("name".into()),
                ]),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_fstr_identifier_f_without_quote_is_ident() {
        // `f` not followed by `"` stays a plain identifier
        assert_eq!(
            kinds("f"),
            vec![TokenKind::Identifier("f".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_fstr_nested_braces_in_slot() {
        // Struct literal inside `{}` — brace depth tracking keeps it together
        assert_eq!(
            kinds(r#"f"x={x}""#),
            vec![
                TokenKind::FStr(vec![
                    RawFStrPart::Literal("x=".into()),
                    RawFStrPart::Expr("x".into()),
                ]),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    // ── strings ──────────────────────────────────────────────────────────────

    #[test]
    fn test_str_simple() {
        assert_eq!(
            kinds(r#""hello""#),
            vec![TokenKind::Str("hello".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_empty() {
        assert_eq!(
            kinds(r#""""#),
            vec![TokenKind::Str("".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_escape_quote() {
        assert_eq!(
            kinds(r#""say \"hi\"""#),
            vec![TokenKind::Str(r#"say "hi""#.into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_escape_newline() {
        assert_eq!(
            kinds(r#""\n""#),
            vec![TokenKind::Str("\n".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_escape_tab() {
        assert_eq!(
            kinds(r#""\t""#),
            vec![TokenKind::Str("\t".into()), TokenKind::Semicolon, TokenKind::Eof]
        );
    }

    #[test]
    fn test_str_unterminated() {
        let err = tokenize(r#""hello"#).unwrap_err();
        assert!(matches!(err, JadeError::UnterminatedString { .. }));
    }

    #[test]
    fn test_str_auto_semicolon() {
        assert_eq!(
            kinds("\"a\"\n\"b\""),
            vec![
                TokenKind::Str("a".into()),
                TokenKind::Semicolon,
                TokenKind::Str("b".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_bracket_tokens() {
        assert_eq!(
            kinds("s[0]"),
            vec![
                TokenKind::Identifier("s".into()),
                TokenKind::LBracket,
                TokenKind::Integer(0),
                TokenKind::RBracket,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    // ── while ────────────────────────────────────────────────────────────────

    #[test]
    fn test_tokenize_while() {
        assert_eq!(kinds("while"), vec![TokenKind::While, TokenKind::Eof]);
    }

    #[test]
    fn test_while_no_semicolon_before_brace() {
        // `while i < 5 {` — no semicolon between condition and opening brace
        assert_eq!(
            kinds("while i < 5 {"),
            vec![
                TokenKind::While,
                TokenKind::Identifier("i".into()),
                TokenKind::Lt,
                TokenKind::Integer(5),
                TokenKind::LBrace,
                TokenKind::Eof,
            ]
        );
    }

    // ── LLM / prompt tokens ──────────────────────────────────────────────────

    #[test]
    fn test_lex_prompt_keyword() {
        assert_eq!(
            kinds("prompt p = \"hi\""),
            vec![
                TokenKind::Prompt,
                TokenKind::Identifier("p".into()),
                TokenKind::Equals,
                TokenKind::Str("hi".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_lex_question_mark() {
        assert_eq!(
            kinds("?p"),
            vec![
                TokenKind::Question,
                TokenKind::Identifier("p".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_lex_pipe_forward_not_pipe_gt() {
        // `|>` should be a single PipeGt token, not Pipe + Gt
        assert_eq!(kinds("|>"), vec![TokenKind::PipeGt, TokenKind::Eof]);
    }

    #[test]
    fn test_lex_typed_deref_tokens() {
        assert_eq!(
            kinds("?p |> int"),
            vec![
                TokenKind::Question,
                TokenKind::Identifier("p".into()),
                TokenKind::PipeGt,
                TokenKind::Identifier("int".into()),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }
}

mod parser {
    use crate::frontend::parser::*;
    use crate::frontend::{
        ast::{BinOpKind, DerefStyle, Expr, Program, Stmt, StructFieldDef, UnaryOpKind},
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
        let Expr::BinOp { op: BinOpKind::And, left, right, .. } = value else {
            panic!("expected And")
        };
        assert!(matches!(left.as_ref(), Expr::BinOp { op: BinOpKind::Lt, .. }));
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
        let p = parse_src(
            "fn f(x) {\n    if x {\n        return 1\n    } else {\n        return 0\n    }\n}",
        );
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
        // Nested function definitions are now a parse error.
        let err = parse_src_err(
            "fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}",
        );
        assert!(matches!(err, JadeError::NestedFunction { .. }));
    }

    // `async fn` had no such guard until v1.3.3, though it tracked the same
    // depth. Nesting one parsed, ran, and then surprised the user twice: the
    // inner function could not see the outer one's parameters, and a decorator
    // on it was dropped in silence.
    #[test]
    fn a_nested_async_fn_is_rejected() {
        let err = parse_src_err(
            "async fn outer() {\n    async fn inner() {\n        return 1\n    }\n    return 2\n}",
        );
        assert!(matches!(err, JadeError::NestedFunction { .. }), "{err:?}");
    }

    #[test]
    fn an_async_fn_nested_in_a_plain_fn_is_rejected() {
        let err = parse_src_err("fn outer() {\n    async fn inner() {\n        return 1\n    }\n}");
        assert!(matches!(err, JadeError::NestedFunction { .. }), "{err:?}");
    }

    #[test]
    fn async_fns_may_sit_beside_each_other_at_the_top_level() {
        let p = parse_src("async fn a() {\n    return 1\n}\nasync fn b() {\n    return 2\n}");
        assert_eq!(p.stmts.len(), 2);
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
        let p = parse_src(
            "fn f(n) {\n    while n > 0 {\n        let n = n - 1\n    }\n    return n\n}",
        );
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
        let p = parse_src(
            "struct Mixed {\n    x,\n    let label = \"origin\",\n    prompt sys = \"helpful\"\n}",
        );
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
        assert!(
            matches!(&fields[0], StructFieldDef::Let { name, default: Expr::StructLiteral { .. }, .. } if name == "inner")
        );
    }

    #[test]
    fn a_plain_extend_block_still_parses() {
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

    /// A stage that cannot be applied is rejected, but no longer by the parser.
    /// It used to match on the shape of the right-hand side, so the message
    /// talked about tokens ("expected function or call"). The stage now survives
    /// into the AST and type inference rejects it, which lets the error name
    /// what the stage actually turned out to be. See the `pipe_stage` tests in
    /// `compiler/tests.rs` for the rejection itself.
    #[test]
    fn test_parse_pipe_invalid_rhs_parses_and_defers_to_inference() {
        let tokens = lexer::tokenize("5 |> (1 + 2)").unwrap();
        let p = parse(tokens).expect("parses; inference rejects it");
        let Stmt::Expr(expr) = &p.stmts[0] else { panic!("expected an expression stmt") };
        assert!(matches!(expr, Expr::Pipe { .. }));
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

    /// An old `interface` block no longer parses. The message has to name the
    /// removal: with `interface` demoted to an identifier, a bare rejection
    /// would report a confusing statement-level error instead.
    #[test]
    fn an_interface_block_is_refused_by_name() {
        let e = parse_src_err("interface Displayable {\n    fn to_str(self)\n}");
        assert!(e.to_string().contains("`interface` was removed"), "got: {e}");
    }

    /// Likewise the conformance claim. Without its own arm the author gets
    /// "expected `{`, found `:`", which explains nothing.
    #[test]
    fn an_extend_conformance_claim_is_refused_by_name() {
        let e = parse_src_err("extend Foo: Bar {\n    fn go(self) {\n        return 1\n    }\n}");
        assert!(e.to_string().contains("conformance claim was removed"), "got: {e}");
    }

    #[test]
    fn test_parse_extend_block() {
        let p = parse_src("extend Foo {\n    fn go(self) {\n        return 1\n    }\n}");
        let Stmt::ExtendBlock { type_name, methods, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(type_name, "Foo");
        assert_eq!(methods.len(), 1);
    }

    #[test]
    fn test_fn_rejects_return_annotation() {
        parse_src_err("fn greet(name) -> str {\n    return \"hi\"\n}");
    }

    #[test]
    fn test_async_fn_rejects_return_annotation() {
        parse_src_err("async fn greet(name) -> str {\n    return \"hi\"\n}");
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
        else {
            panic!("expected Let with PromptDeref")
        };
        assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
        assert!(constraint.is_none());
    }

    /// The type stage is a pipe over the dereference, not a field on it. What it
    /// *means* is unchanged — inference still folds it back in as an
    /// `output_type` — but that decision now happens where the names are known.
    #[test]
    fn test_parse_prompt_deref_typed_int() {
        let p = parse_src("let x = ?p |> int");
        let Stmt::Let { value: Expr::Pipe { value, stage, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Pipe")
        };
        let Expr::PromptDeref { expr, constraint, .. } = value.as_ref() else {
            panic!("expected the piped value to be a PromptDeref")
        };
        assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
        assert!(constraint.is_none(), "the parser no longer sets a constraint");
        assert!(matches!(stage.as_ref(), Expr::Identifier { name, .. } if name == "int"));
    }

    #[test]
    fn test_prefix_deref_on_field_is_rejected() {
        // `?obj.field` reads as if `?` applied to `obj`; only the postfix forms
        // are allowed for fields.
        for src in ["let x = ?obj.system", "let x = ?obj.field |> int", "let x = ?a.b.c"] {
            let err = parse_src_err(src);
            assert!(
                matches!(err, JadeError::PrefixDerefOnField { .. }),
                "expected PrefixDerefOnField for {src}, got {err:?}"
            );
        }
    }

    #[test]
    fn test_dot_question_without_parens_is_rejected() {
        // `obj.?p` is not one of the two accepted spellings.
        let err = parse_src_err("let x = obj.?p");
        assert!(matches!(err, JadeError::UnexpectedToken { .. }), "got {err:?}");
    }

    #[test]
    fn test_prefix_deref_on_bare_prompt_still_works() {
        let p = parse_src("let x = ?p");
        let Stmt::Let { value: Expr::PromptDeref { expr, style, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with PromptDeref")
        };
        assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
        assert_eq!(*style, DerefStyle::Prefix);
    }

    #[test]
    fn test_parse_postfix_deref() {
        for src in ["let x = obj.(?system)", "let x = obj~>system"] {
            let p = parse_src(src);
            let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
            else {
                panic!("expected Let with PromptDeref for {src}")
            };
            let Expr::FieldAccess { object, field, .. } = expr.as_ref() else {
                panic!("expected FieldAccess for {src}")
            };
            assert!(matches!(object.as_ref(), Expr::Identifier { name, .. } if name == "obj"));
            assert_eq!(field, "system");
            assert!(constraint.is_none());
        }
    }

    #[test]
    fn test_postfix_deref_does_not_shadow_plain_field_access() {
        // `.` followed by an identifier must still be ordinary field access —
        // only `.(?` diverts into a deref.
        let p = parse_src("let x = obj.system");
        let Stmt::Let { value: Expr::FieldAccess { field, .. }, .. } = &p.stmts[0] else {
            panic!("expected plain FieldAccess")
        };
        assert_eq!(field, "system");
    }

    #[test]
    fn test_deref_records_its_surface_style() {
        for (src, want) in [
            ("let x = ?p", DerefStyle::Prefix),
            ("let x = obj.(?p)", DerefStyle::DotParen),
            ("let x = obj~>p", DerefStyle::Squiggly),
        ] {
            let p = parse_src(src);
            let Stmt::Let { value: Expr::PromptDeref { style, .. }, .. } = &p.stmts[0] else {
                panic!("expected Let with PromptDeref for {src}")
            };
            assert_eq!(*style, want, "wrong style for {src}");
        }
    }

    #[test]
    fn test_deref_style_is_cosmetic_only() {
        // Both accepted field spellings must agree on everything but `style` (and
        // spans, which legitimately differ since the operator sits elsewhere in
        // the line) — the sugar is a spelling, not a semantic distinction.
        for src in ["let x = obj.(?p)", "let x = obj~>p"] {
            let p = parse_src(src);
            let Stmt::Let { value: Expr::PromptDeref { expr, constraint, .. }, .. } = &p.stmts[0]
            else {
                panic!("expected PromptDeref for {src}")
            };
            let Expr::FieldAccess { object, field, .. } = expr.as_ref() else {
                panic!("expected FieldAccess for {src}")
            };
            assert!(matches!(object.as_ref(), Expr::Identifier { name, .. } if name == "obj"));
            assert_eq!(field, "p");
            assert!(constraint.is_none());
        }
    }

    /// A postfix deref takes its stage the same way a prefix one does: the `|>`
    /// sits outside the parens, so `parse_pipe` reads it and both spellings
    /// produce the identical `Pipe` over a `PromptDeref`.
    #[test]
    fn test_parse_postfix_deref_typed() {
        for src in ["let x = obj.(?field) |> int", "let x = obj~>field |> int"] {
            let p = parse_src(src);
            let Stmt::Let { value: Expr::Pipe { value, stage, .. }, .. } = &p.stmts[0] else {
                panic!("expected Let with Pipe for {src}")
            };
            let Expr::PromptDeref { expr, constraint, .. } = value.as_ref() else {
                panic!("expected a PromptDeref for {src}")
            };
            assert!(matches!(expr.as_ref(), Expr::FieldAccess { field, .. } if field == "field"));
            assert!(constraint.is_none(), "the parser no longer sets a constraint");
            assert!(matches!(stage.as_ref(), Expr::Identifier { name, .. } if name == "int"));
        }
    }

    #[test]
    fn test_parse_postfix_deref_after_call() {
        // The point of the sugar: the deref stays at the tail of a chain.
        for src in ["let x = make(1).inner.(?p)", "let x = make(1).inner~>p"] {
            let p = parse_src(src);
            let Stmt::Let { value: Expr::PromptDeref { expr, .. }, .. } = &p.stmts[0] else {
                panic!("expected Let with PromptDeref for {src}")
            };
            let Expr::FieldAccess { object, field, .. } = expr.as_ref() else {
                panic!("expected FieldAccess for {src}")
            };
            assert_eq!(field, "p");
            assert!(matches!(object.as_ref(), Expr::FieldAccess { field, .. } if field == "inner"));
        }
    }

    #[test]
    fn test_arrow_does_not_break_bitwise_not() {
        let p = parse_src("let x = ~a > b");
        let Stmt::Let { value, .. } = &p.stmts[0] else { panic!("expected Let") };
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Gt, .. }));
    }

    /// Until v1.2.0 these three were parse errors (`StreamingWithType`): the
    /// parser tracked whether it sat inside a `print(…)` call and refused a
    /// typed dereference there. The grammar was context-sensitive as a result —
    /// the same expression was legal or illegal depending on the name of the
    /// call around it. Streaming is now decided by what `print` receives, not by
    /// what the parser can see, so all three parse.
    #[test]
    fn a_typed_deref_inside_print_now_parses() {
        for src in ["print(?p |> int)", "print(obj.(?p) |> int)", "print(obj~>p |> int)"] {
            let tokens = lexer::tokenize(src).expect("lex");
            assert!(parse(tokens).is_ok(), "should parse: {src}");
        }
    }

    /// `|>` has exactly one parse path now, so a stage after a dereference is an
    /// ordinary `Expr::Pipe` and not a `PromptDeref.constraint`. This is what
    /// makes chaining possible: the old deref arm read its stage with
    /// `parse_or`, specifically so a second `|>` could not follow.
    #[test]
    fn a_deref_stage_parses_as_a_pipe_not_a_constraint() {
        let p = parse_src("let x = ?p |> int");
        let Stmt::Let { value, .. } = &p.stmts[0] else { panic!("expected Let") };
        let Expr::Pipe { value: piped, .. } = value else { panic!("expected Pipe, got {value:?}") };
        assert!(matches!(piped.as_ref(), Expr::PromptDeref { constraint: None, .. }));
    }

    #[test]
    fn a_deref_chains_left_to_right() {
        let p = parse_src("let x = ?p |> int |> double");
        let Stmt::Let { value, .. } = &p.stmts[0] else { panic!("expected Let") };
        // Outer stage is `double`; its value is the `?p |> int` pipe.
        let Expr::Pipe { value: inner, stage, .. } = value else { panic!("expected Pipe") };
        assert!(matches!(stage.as_ref(), Expr::Identifier { name, .. } if name == "double"));
        assert!(matches!(inner.as_ref(), Expr::Pipe { .. }), "left-associative");
    }

    // ── dict literal tests ────────────────────────────────────────────────────

    #[test]
    fn test_parse_dict_empty() {
        let p = parse_src("let d = {}");
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Dict")
        };
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_dict_single_entry() {
        let p = parse_src(r#"let d = {"key": 1}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Dict")
        };
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0].0, Expr::Str { value, .. } if value == "key"));
        assert!(matches!(&entries[0].1, Expr::Integer { value: 1, .. }));
    }

    #[test]
    fn test_parse_dict_multiple_entries() {
        let p = parse_src(r#"let d = {"a": 1, "b": 2}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Dict")
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_parse_dict_trailing_comma() {
        let p = parse_src(r#"let d = {"a": 1,}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Dict")
        };
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_dict_identifier_key() {
        // Variable key: the key expression is an identifier
        let p = parse_src("let d = {k: 1}");
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0] else {
            panic!("expected Let with Dict")
        };
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

    // ── decorators on declarations ────────────────────────────────────────────
    //
    // These desugar in the parser: `@f let x = v` becomes `let x = f(v)` and
    // nothing downstream sees a decorator at all. So the assertions are about
    // the *call* the parser built, not about a `decorators` field — there isn't
    // one on `Stmt::Let`, and adding one would push this syntax through every
    // later stage for no gain.

    #[test]
    fn a_decorated_let_desugars_to_a_call() {
        let p = parse_src("@shout\nlet x = \"hi\"");
        let Stmt::Let { name, value, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "x");
        let Expr::Call { callee, args, kwargs, .. } = value else { panic!("not a call") };
        assert!(matches!(&**callee, Expr::Identifier { name, .. } if name == "shout"));
        assert_eq!(args.len(), 1);
        assert!(matches!(&args[0], Expr::Str { value, .. } if value == "hi"));
        assert!(kwargs.is_empty());
    }

    #[test]
    fn a_decorated_prompt_wraps_its_text() {
        let p = parse_src("@tagged\nprompt p = \"ask\"");
        let Stmt::PromptDecl { name, body, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "p");
        let Expr::Call { callee, args, .. } = body else { panic!("not a call") };
        assert!(matches!(&**callee, Expr::Identifier { name, .. } if name == "tagged"));
        assert!(matches!(&args[0], Expr::Str { value, .. } if value == "ask"));
    }

    #[test]
    fn decorator_arguments_follow_the_decorated_value() {
        let p = parse_src("@fence(\"p\", cls = \"lead\")\nlet x = \"hi\"");
        let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
        let Expr::Call { args, kwargs, .. } = value else { panic!() };
        // The decorated value is first; the decorator's own arguments follow,
        // with keyword arguments kept apart from positional ones.
        assert_eq!(args.len(), 2);
        assert!(matches!(&args[0], Expr::Str { value, .. } if value == "hi"));
        assert!(matches!(&args[1], Expr::Str { value, .. } if value == "p"));
        assert_eq!(kwargs.len(), 1);
        assert_eq!(kwargs[0].0, "cls");
    }

    #[test]
    fn the_first_decorator_written_is_applied_first() {
        // Matching `fn`, whose decorators emit in source order. The reverse of
        // Python's rule, and the two forms have to agree with each other before
        // they agree with anything else.
        let p = parse_src("@a\n@b\nlet x = 1");
        let Stmt::Let { value, .. } = &p.stmts[0] else { panic!() };
        let Expr::Call { callee, args, .. } = value else { panic!() };
        assert!(matches!(&**callee, Expr::Identifier { name, .. } if name == "b"));
        let Expr::Call { callee: inner, .. } = &args[0] else { panic!("b's arg is not a call") };
        assert!(matches!(&**inner, Expr::Identifier { name, .. } if name == "a"));
    }

    #[test]
    fn a_namespaced_decorator_becomes_a_field_access() {
        let p = parse_src("@style::tagged\nprompt p = \"ask\"");
        let Stmt::PromptDecl { body, .. } = &p.stmts[0] else { panic!() };
        let Expr::Call { callee, .. } = body else { panic!() };
        let Expr::FieldAccess { object, field, .. } = &**callee else {
            panic!("not a field access")
        };
        assert_eq!(field, "tagged");
        assert!(matches!(&**object, Expr::Identifier { name, .. } if name == "style"));
    }

    #[test]
    fn a_decorator_on_a_bare_expression_is_refused() {
        let err = parse_src_err("@shout\nprint(\"hi\")");
        let JadeError::UnexpectedToken { expected, .. } = &err else { panic!("{err:?}") };
        assert!(
            expected.contains("`let`") && expected.contains("`prompt`"),
            "the message should name the forms that work: {expected}"
        );
    }

    #[test]
    fn a_decorated_let_works_inside_a_function() {
        // The decorator branch lives in `parse_stmt`, which is also the block
        // parser, so this comes free — but a `fn` decorator on a nested
        // definition is silently dropped at emit time, and that trap should not
        // quietly extend to declarations.
        let p = parse_src("fn f() {\n    @shout\n    let x = \"hi\"\n    return x\n}");
        let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
        let Stmt::Let { value, .. } = &body[0] else { panic!("not a let") };
        assert!(matches!(value, Expr::Call { .. }));
    }

    /// A decorator on a struct ran under `jade run` and was skipped under
    /// `jade build`, so the two engines disagreed about what a literal produced.
    /// Refused by name rather than dropped in silence.
    #[test]
    fn a_decorator_on_a_struct_is_refused_by_name() {
        let e = parse_src_err("@log(\"City\")\nstruct City {\n  name,\n}");
        assert!(e.to_string().contains("decorator on a struct was removed"), "got: {e}");
    }

    /// `@route` went for the same reason, and on the same terms.
    #[test]
    fn a_route_decorator_is_refused_by_name() {
        let e = parse_src_err(
            "@route(\"kind\")\nextend Foo {\n    fn go(self) {\n        return 1\n    }\n}",
        );
        assert!(e.to_string().contains("`@route` was removed"), "got: {e}");
    }

    /// A decorator on a `let` still works; only the struct target went.
    #[test]
    fn a_decorator_on_a_let_still_parses() {
        let p = parse_src("@shout\nlet greeting = \"hi\"");
        assert!(!p.stmts.is_empty());
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

    // ── module-path separator (`::`) ──────────────────────────────────────────

    #[test]
    fn test_parse_use_path_sep_colon_colon() {
        // `use std::math` parses and normalizes the path to slash-separated form.
        let p = parse_src("use std::math");
        let Stmt::Use { path, path_is_string, as_name, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(path, "std/math");
        assert!(!path_is_string);
        assert!(as_name.is_none());
    }

    #[test]
    fn test_parse_from_use_path_sep_colon_colon() {
        let p = parse_src("from std::math use floor, ceil");
        let Stmt::FromUse { path, names, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(path, "std/math");
        assert_eq!(names, &vec!["floor".to_string(), "ceil".to_string()]);
    }

    #[test]
    fn test_parse_use_dot_separator_is_error() {
        // The old `.` separator is no longer accepted in module-path position.
        let _ = parse_src_err("use std.math");
    }

    #[test]
    fn test_parse_decorator_namespaced_colon_colon() {
        // `@tools::register` normalizes to the dot-joined internal name `tools.register`.
        let p = parse_src("@tools::register\nfn f() {}");
        let Stmt::FnDef { decorators, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(decorators[0].0, "tools.register");
    }
}

mod ast {
    use crate::frontend::ast::*;
    use crate::frontend::error::Span;

    fn span() -> Span {
        Span { line: 1, col: 1 }
    }

    // ── StructFieldDef::name() helper ────────────────────────────────────────

    #[test]
    fn test_struct_field_def_name_required() {
        let f = StructFieldDef::Required("x".into());
        assert_eq!(f.name(), "x");
    }

    #[test]
    fn test_struct_field_def_name_let() {
        let f = StructFieldDef::Let {
            name: "count".into(),
            default: Expr::Integer { value: 0, span: span() },
        };
        assert_eq!(f.name(), "count");
    }

    #[test]
    fn test_struct_field_def_name_prompt() {
        let f = StructFieldDef::Prompt {
            name: "system".into(),
            default: Expr::Str { value: "hi".into(), span: span() },
        };
        assert_eq!(f.name(), "system");
    }

    // ── Span carriage through nodes ──────────────────────────────────────────

    #[test]
    fn test_span_carried_on_expr() {
        let e = Expr::Integer { value: 7, span: Span { line: 12, col: 4 } };
        let Expr::Integer { span, .. } = e else { panic!() };
        assert_eq!(span.line, 12);
        assert_eq!(span.col, 4);
    }

    #[test]
    fn test_span_carried_on_stmt() {
        let s = Stmt::Let {
            name: "a".into(),
            value: Expr::Bool { value: true, span: span() },
            span: Span { line: 9, col: 2 },
        };
        let Stmt::Let { span, .. } = s else { panic!() };
        assert_eq!(span.line, 9);
        assert_eq!(span.col, 2);
    }

    // ── construction / nesting ───────────────────────────────────────────────

    #[test]
    fn test_binop_construction_nesting() {
        // Build `1 + (2 * 3)` by hand and read it back through the shape.
        let inner = Expr::BinOp {
            op: BinOpKind::Mul,
            left: Box::new(Expr::Integer { value: 2, span: span() }),
            right: Box::new(Expr::Integer { value: 3, span: span() }),
            span: span(),
        };
        let outer = Expr::BinOp {
            op: BinOpKind::Add,
            left: Box::new(Expr::Integer { value: 1, span: span() }),
            right: Box::new(inner),
            span: span(),
        };
        let Expr::BinOp { op: BinOpKind::Add, right, .. } = outer else { panic!() };
        assert!(matches!(right.as_ref(), Expr::BinOp { op: BinOpKind::Mul, .. }));
    }

    #[test]
    fn test_unaryop_construction() {
        let e = Expr::UnaryOp {
            op: UnaryOpKind::Neg,
            operand: Box::new(Expr::Integer { value: 5, span: span() }),
            span: span(),
        };
        let Expr::UnaryOp { op: UnaryOpKind::Neg, operand, .. } = e else { panic!() };
        assert!(matches!(operand.as_ref(), Expr::Integer { value: 5, .. }));
    }

    #[test]
    fn test_program_holds_stmts() {
        let p = Program {
            stmts: vec![
                Stmt::Expr(Expr::Integer { value: 1, span: span() }),
                Stmt::Expr(Expr::Integer { value: 2, span: span() }),
            ],
        };
        assert_eq!(p.stmts.len(), 2);
    }

    #[test]
    fn test_fstr_part_variants() {
        let lit = FStrPart::Literal("hi ".into());
        let ex = FStrPart::Expr(Expr::Identifier { name: "name".into(), span: span() });
        assert!(matches!(lit, FStrPart::Literal(s) if s == "hi "));
        assert!(matches!(ex, FStrPart::Expr(Expr::Identifier { name, .. }) if name == "name"));
    }

    #[test]
    fn test_catch_arm_catch_all() {
        let arm = CatchArm { catch_type: None, binding: "e".into(), body: vec![] };
        assert!(arm.catch_type.is_none());
        assert_eq!(arm.binding, "e");
        assert!(arm.body.is_empty());
    }

    #[test]
    fn test_catch_arm_typed() {
        let arm = CatchArm {
            catch_type: Some("MyError".into()),
            binding: "err".into(),
            body: vec![Stmt::Return { value: None, span: span() }],
        };
        assert_eq!(arm.catch_type.as_deref(), Some("MyError"));
        assert_eq!(arm.body.len(), 1);
    }

    // ── Clone / Debug derives ────────────────────────────────────────────────

    #[test]
    fn test_expr_clone_is_independent() {
        let orig = Expr::Str { value: "keep".into(), span: span() };
        let cloned = orig.clone();
        let Expr::Str { value, .. } = cloned else { panic!() };
        assert_eq!(value, "keep");
        // original still usable
        assert!(matches!(orig, Expr::Str { .. }));
    }

    #[test]
    fn test_binopkind_debug_format() {
        assert_eq!(format!("{:?}", BinOpKind::NotIn), "NotIn");
        assert_eq!(format!("{:?}", UnaryOpKind::BitNot), "BitNot");
    }

    // ── serde round-trip (all AST nodes derive Serialize/Deserialize) ────────

    #[test]
    fn test_program_serde_roundtrip() {
        let p = Program {
            stmts: vec![Stmt::Let {
                name: "x".into(),
                value: Expr::BinOp {
                    op: BinOpKind::Add,
                    left: Box::new(Expr::Integer { value: 1, span: span() }),
                    right: Box::new(Expr::Integer { value: 2, span: span() }),
                    span: span(),
                },
                span: span(),
            }],
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Program = serde_json::from_str(&json).unwrap();
        let Stmt::Let { name, value, .. } = &back.stmts[0] else { panic!() };
        assert_eq!(name, "x");
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Add, .. }));
    }

    #[test]
    fn test_call_kwargs_default_on_deserialize() {
        // `kwargs` is `#[serde(default)]`, so a Call serialized without it
        // (or an older payload lacking the field) decodes to an empty vec.
        let call = Expr::Call {
            callee: Box::new(Expr::Identifier { name: "f".into(), span: span() }),
            args: vec![],
            kwargs: vec![],
            span: span(),
        };
        let json = serde_json::to_string(&call).unwrap();
        let back: Expr = serde_json::from_str(&json).unwrap();
        let Expr::Call { kwargs, .. } = back else { panic!() };
        assert!(kwargs.is_empty());
    }
}

mod error {
    use crate::frontend::error::{JadeError, Result, Span};

    fn span(line: usize, col: usize) -> Span {
        Span { line, col }
    }

    // ── Span carriage across variants ────────────────────────────────────────

    #[test]
    fn test_span_carried_unexpected_char() {
        let e = JadeError::UnexpectedChar { ch: '$', span: span(3, 7) };
        let JadeError::UnexpectedChar { ch, span } = e else { panic!() };
        assert_eq!(ch, '$');
        assert_eq!(span.line, 3);
        assert_eq!(span.col, 7);
    }

    #[test]
    fn test_span_carried_arity_mismatch() {
        let e = JadeError::ArityMismatch { expected: 2, got: 3, span: span(10, 1) };
        let JadeError::ArityMismatch { expected, got, span } = e else { panic!() };
        assert_eq!(expected, 2);
        assert_eq!(got, 3);
        assert_eq!(span.line, 10);
    }

    // ── Display / formatting output ──────────────────────────────────────────

    #[test]
    fn test_display_unexpected_char() {
        let e = JadeError::UnexpectedChar { ch: '$', span: span(2, 5) };
        assert_eq!(e.to_string(), "[2:5] syntax error: unexpected character '$'");
    }

    #[test]
    fn test_display_unexpected_token() {
        let e = JadeError::UnexpectedToken {
            expected: "identifier".into(),
            got: "integer".into(),
            span: span(1, 4),
        };
        assert_eq!(e.to_string(), "[1:4] syntax error: expected identifier, found integer");
    }

    #[test]
    fn test_display_division_by_zero() {
        let e = JadeError::DivisionByZero { span: span(4, 9) };
        assert_eq!(e.to_string(), "[4:9] division by zero");
    }

    #[test]
    fn test_display_invalid_shift() {
        let e = JadeError::InvalidShift { amount: 99, span: span(1, 1) };
        assert_eq!(e.to_string(), "[1:1] invalid shift amount 99");
    }

    #[test]
    fn test_display_type_error_includes_message() {
        let e = JadeError::TypeError { message: "cannot add str and int".into(), span: span(6, 2) };
        assert_eq!(e.to_string(), "[6:2] type error: cannot add str and int");
    }

    #[test]
    fn test_display_arity_mismatch() {
        let e = JadeError::ArityMismatch { expected: 1, got: 0, span: span(8, 3) };
        assert_eq!(e.to_string(), "[8:3] wrong number of arguments: expected 1, got 0");
    }

    #[test]
    fn test_display_undefined_field() {
        let e = JadeError::UndefinedField {
            type_name: "Point".into(),
            field: "z".into(),
            owner: crate::frontend::error::FieldOwner::Struct,
            span: span(2, 2),
        };
        assert_eq!(e.to_string(), "[2:2] struct 'Point' has no field 'z'");
    }

    /// The same variant reports a non-struct differently, because it used to
    /// say "struct" for every one of them: a missing method on an array read
    /// `struct 'array' has no field 'map'`, which is wrong three times over.
    /// The owner is carried rather than guessed from the name — `struct array
    /// {}` is a legal declaration, so the name cannot settle it.
    #[test]
    fn test_display_undefined_field_on_a_value() {
        use crate::frontend::error::FieldOwner;
        let arr = JadeError::UndefinedField {
            type_name: "array".into(),
            field: "map".into(),
            owner: FieldOwner::Value,
            span: span(2, 2),
        };
        assert_eq!(arr.to_string(), "[2:2] array has no method 'map'");

        let d = JadeError::UndefinedField {
            type_name: "dict".into(),
            field: "k".into(),
            owner: FieldOwner::Dict,
            span: span(1, 1),
        };
        assert_eq!(d.to_string(), "[1:1] dict has no key or method 'k'");
    }

    /// A stdlib package names itself and lists what it has. The namespace is a
    /// dict at run time, so this used to surface as `struct 'dict' has no field
    /// 'round'` — naming neither the module nor a type the reader would know.
    #[test]
    fn test_display_unknown_package_fn() {
        let e = JadeError::UnknownPackageFn {
            package: "std::math".into(),
            name: "round".into(),
            available: vec!["floor".into(), "ceil".into()],
            span: span(3, 7),
        };
        assert_eq!(
            e.to_string(),
            "[3:7] std::math has no function 'round'\n  It provides: floor, ceil."
        );
    }

    #[test]
    fn test_display_index_out_of_bounds() {
        let e = JadeError::IndexOutOfBounds { index: 5, len: 3, span: span(1, 1) };
        assert_eq!(e.to_string(), "[1:1] index 5 out of bounds (length 3)");
    }

    #[test]
    fn test_display_inherited_field_clash() {
        let e = JadeError::InheritedFieldClash {
            field: "name".into(),
            owner: "Animal".into(),
            other: "Dog".into(),
            span: span(3, 1),
        };
        let text = e.to_string();
        assert!(text.contains("'name'"), "names the field: {text}");
        assert!(text.contains("Animal") && text.contains("Dog"), "names both sides: {text}");
    }

    #[test]
    fn test_display_quoted_import_suggests_colon_notation() {
        // The Display impl strips `.jde` and rewrites `/` to `::` in the suggestion.
        let e = JadeError::QuotedImport { path: "lib/helper.jde".into(), span: span(1, 1) };
        assert!(e.to_string().starts_with(
            "[1:1] quoted file imports were removed. Import by module name with `::` notation: `use lib::helper`"
        ), "got: {}", e);
    }

    #[test]
    fn test_display_import_alias_removed() {
        let e = JadeError::ImportAlias { span: span(1, 1) };
        assert!(e.to_string().starts_with("[1:1] the `as` import alias was removed"), "got: {}", e);
    }

    #[test]
    fn test_display_infile_wraps_cause_and_omits_span() {
        // `InFile` has no span of its own; it prefixes the wrapped cause's message.
        let inner = JadeError::DivisionByZero { span: span(7, 3) };
        let e = JadeError::InFile { file: "helpers.jde".into(), cause: Box::new(inner) };
        assert_eq!(e.to_string(), "in \"helpers.jde\": [7:3] division by zero");
    }

    #[test]
    fn test_display_infile_nested_twice() {
        let inner = JadeError::UndefinedVariable { name: "x".into(), span: span(1, 1) };
        let mid = JadeError::InFile { file: "b.jde".into(), cause: Box::new(inner) };
        let outer = JadeError::InFile { file: "a.jde".into(), cause: Box::new(mid) };
        assert_eq!(outer.to_string(), "in \"a.jde\": in \"b.jde\": [1:1] undefined variable 'x'");
    }

    // ── `Result` alias ───────────────────────────────────────────────────────

    #[test]
    fn test_result_alias_ok() {
        fn ok() -> Result<i64> {
            Ok(42)
        }
        assert_eq!(ok().unwrap(), 42);
    }

    #[test]
    fn test_result_alias_err() {
        fn boom() -> Result<i64> {
            Err(JadeError::DivisionByZero { span: span(1, 1) })
        }
        assert!(matches!(boom(), Err(JadeError::DivisionByZero { .. })));
    }

    // ── Span is Copy ─────────────────────────────────────────────────────────

    #[test]
    fn test_span_is_copy() {
        let a = span(5, 6);
        let b = a; // copy, not move
        assert_eq!(a.line, b.line);
        assert_eq!(a.col, b.col);
    }
}
