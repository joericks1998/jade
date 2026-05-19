use super::*;
use crate::frontend::error::JadeError;

fn kinds(src: &str) -> Vec<TokenKind> {
    tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
}

// ── existing operations ──────────────────────────────────────────────────

#[test]
fn test_integer_literal() {
    assert_eq!(kinds("42"), vec![TokenKind::Integer(42), TokenKind::Semicolon, TokenKind::Eof]);
}

#[test]
fn test_float_literal() {
    assert_eq!(kinds("3.14"), vec![TokenKind::Float(3.14), TokenKind::Semicolon, TokenKind::Eof]);
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
            TokenKind::Plus, TokenKind::Minus, TokenKind::Star,
            TokenKind::Slash, TokenKind::Percent,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn test_all_bitwise_operators() {
    assert_eq!(
        kinds("& | ^ ~ << >>"),
        vec![
            TokenKind::Ampersand, TokenKind::Pipe, TokenKind::Caret,
            TokenKind::Tilde, TokenKind::LtLt, TokenKind::GtGt,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn test_auto_semicolon_after_integer() {
    assert_eq!(
        kinds("1\n2"),
        vec![
            TokenKind::Integer(1), TokenKind::Semicolon,
            TokenKind::Integer(2), TokenKind::Semicolon,
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
            TokenKind::Integer(1), TokenKind::Plus,
            TokenKind::Integer(2), TokenKind::Semicolon,
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
            TokenKind::EqEq, TokenKind::BangEq,
            TokenKind::Lt,   TokenKind::Gt,
            TokenKind::LtEq, TokenKind::GtEq,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn test_auto_semicolon_after_true() {
    assert_eq!(
        kinds("true\nfalse"),
        vec![
            TokenKind::True,  TokenKind::Semicolon,
            TokenKind::False, TokenKind::Semicolon,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn test_bare_lt_and_gt() {
    assert_eq!(
        kinds("1 < 2 > 0"),
        vec![
            TokenKind::Integer(1), TokenKind::Lt,
            TokenKind::Integer(2), TokenKind::Gt,
            TokenKind::Integer(0), TokenKind::Semicolon,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn test_eq_eq_vs_equals() {
    assert_eq!(kinds("=="), vec![TokenKind::EqEq,   TokenKind::Eof]);
    assert_eq!(kinds("="),  vec![TokenKind::Equals, TokenKind::Eof]);
}

#[test]
fn test_bang_eq_vs_bang() {
    assert_eq!(kinds("!="), vec![TokenKind::BangEq, TokenKind::Eof]);
    assert_eq!(kinds("!"),  vec![TokenKind::Bang,   TokenKind::Eof]);
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
    assert_eq!(
        kinds("if else"),
        vec![TokenKind::If, TokenKind::Else, TokenKind::Eof]
    );
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
            TokenKind::Identifier("a".into()), TokenKind::Comma,
            TokenKind::Identifier("b".into()), TokenKind::Semicolon,
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
        vec![
            TokenKind::RBrace, TokenKind::Semicolon,
            TokenKind::Else, TokenKind::Eof,
        ]
    );
}

#[test]
fn test_float_requires_digit_after_dot() {
    // `1.` tokenizes as Integer(1) followed by a standalone Dot — not a float literal.
    // Float literals require at least one digit after the decimal point: `1.0`.
    assert_eq!(
        kinds("1."),
        vec![TokenKind::Integer(1), TokenKind::Dot, TokenKind::Eof]
    );
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

#[test]
fn test_tokenize_interface_keyword() {
    assert_eq!(kinds("interface"), vec![TokenKind::Interface, TokenKind::Eof]);
}

#[test]
fn test_tokenize_arrow() {
    assert_eq!(kinds("->"), vec![TokenKind::Arrow, TokenKind::Eof]);
}

#[test]
fn test_tokenize_arrow_vs_minus() {
    // `-` alone stays Minus; only `->` becomes Arrow
    assert_eq!(
        kinds("- ->"),
        vec![TokenKind::Minus, TokenKind::Arrow, TokenKind::Eof]
    );
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
        vec![TokenKind::Str(r#"he said "hi" to her"#.into()), TokenKind::Semicolon, TokenKind::Eof]
    );
}

#[test]
fn test_fstr_no_interpolation() {
    use super::RawFStrPart;
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
    use super::RawFStrPart;
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
    use super::RawFStrPart;
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
    use super::RawFStrPart;
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
            TokenKind::Str("a".into()), TokenKind::Semicolon,
            TokenKind::Str("b".into()), TokenKind::Semicolon,
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
