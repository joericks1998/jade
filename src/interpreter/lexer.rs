use super::error::{JadeError, Result, Span};

/// Every kind of token the lexer can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Integer(i64),
    Float(f64),

    // Identifiers and keywords
    Identifier(String),
    Let,
    Fn,
    Return,
    If,
    Else,
    While,
    Struct,
    Impl,
    True,
    False,

    // Arithmetic operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    // Bitwise operators
    Ampersand,
    Pipe,
    Caret,
    Tilde,
    LtLt,
    GtGt,

    // Logical operators
    AmpAmp,
    PipePipe,
    Bang,

    // Comparison operators
    EqEq,
    BangEq,
    Lt,
    Gt,
    LtEq,
    GtEq,

    // Assignment
    Equals,

    // Punctuation
    Comma,
    Semicolon,
    Dot,
    Colon,

    // Grouping
    LParen,
    RParen,
    LBrace,
    RBrace,

    // End of file sentinel
    Eof,
}

/// A token: a kind plus the source position where it starts.
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Returns true if this token kind triggers auto-semicolon insertion
/// when it appears at the end of a line.
fn is_line_terminator(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Integer(_)
            | TokenKind::Float(_)
            | TokenKind::Identifier(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::RParen
            // RBrace triggers semicolons so struct literals (`let p = Point { … }`)
            // on their own line terminate the statement correctly. The parser handles
            // `} else {` by consuming the inserted semicolon before checking for `else`.
            | TokenKind::RBrace
    )
}

/// Tokenize Jade source into a flat Vec of tokens.
/// Semicolons are auto-inserted — the parser never sees raw newlines.
pub fn tokenize(source: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut line = 1usize;
    let mut col = 1usize;

    while i < chars.len() {
        let ch = chars[i];

        match ch {
            // Newline: possibly insert a semicolon, then advance to next line
            '\n' => {
                if let Some(last) = tokens.last() {
                    if is_line_terminator(&last.kind) {
                        tokens.push(Token {
                            kind: TokenKind::Semicolon,
                            span: Span { line, col },
                        });
                    }
                }
                line += 1;
                col = 1;
                i += 1;
            }

            // Skip other whitespace
            ' ' | '\t' | '\r' => {
                col += 1;
                i += 1;
            }

            // Integer and float literals
            '0'..='9' => {
                let start_col = col;
                let mut num_str = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    num_str.push(chars[i]);
                    i += 1;
                    col += 1;
                }
                // If followed by '.' and then a digit, parse as float
                if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    num_str.push('.');
                    i += 1;
                    col += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        num_str.push(chars[i]);
                        i += 1;
                        col += 1;
                    }
                    let value: f64 = num_str.parse().map_err(|_| JadeError::LiteralOverflow {
                        span: Span { line, col: start_col },
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::Float(value),
                        span: Span { line, col: start_col },
                    });
                } else {
                    let value: i64 = num_str.parse().map_err(|_| JadeError::LiteralOverflow {
                        span: Span { line, col: start_col },
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::Integer(value),
                        span: Span { line, col: start_col },
                    });
                }
            }

            // Identifiers and keywords
            'a'..='z' | 'A'..='Z' | '_' => {
                let start_col = col;
                let mut name = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    name.push(chars[i]);
                    i += 1;
                    col += 1;
                }
                let kind = match name.as_str() {
                    "let"    => TokenKind::Let,
                    "fn"     => TokenKind::Fn,
                    "return" => TokenKind::Return,
                    "if"     => TokenKind::If,
                    "else"   => TokenKind::Else,
                    "while"  => TokenKind::While,
                    "struct" => TokenKind::Struct,
                    "impl"   => TokenKind::Impl,
                    "true"   => TokenKind::True,
                    "false"  => TokenKind::False,
                    _        => TokenKind::Identifier(name),
                };
                tokens.push(Token {
                    kind,
                    span: Span { line, col: start_col },
                });
            }

            // Unambiguous single-character tokens
            '+' => { tokens.push(Token { kind: TokenKind::Plus,    span: Span { line, col } }); col += 1; i += 1; }
            '-' => { tokens.push(Token { kind: TokenKind::Minus,   span: Span { line, col } }); col += 1; i += 1; }
            '*' => { tokens.push(Token { kind: TokenKind::Star,    span: Span { line, col } }); col += 1; i += 1; }
            // `/` or `//` (line comment)
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '/' {
                    // Skip everything until the end of the line
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                } else {
                    tokens.push(Token { kind: TokenKind::Slash, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }
            '%' => { tokens.push(Token { kind: TokenKind::Percent, span: Span { line, col } }); col += 1; i += 1; }
            '~' => { tokens.push(Token { kind: TokenKind::Tilde,   span: Span { line, col } }); col += 1; i += 1; }
            '^' => { tokens.push(Token { kind: TokenKind::Caret,   span: Span { line, col } }); col += 1; i += 1; }
            '(' => { tokens.push(Token { kind: TokenKind::LParen,  span: Span { line, col } }); col += 1; i += 1; }
            ')' => { tokens.push(Token { kind: TokenKind::RParen,  span: Span { line, col } }); col += 1; i += 1; }
            '{' => { tokens.push(Token { kind: TokenKind::LBrace,  span: Span { line, col } }); col += 1; i += 1; }
            '}' => { tokens.push(Token { kind: TokenKind::RBrace,  span: Span { line, col } }); col += 1; i += 1; }
            ',' => { tokens.push(Token { kind: TokenKind::Comma,   span: Span { line, col } }); col += 1; i += 1; }
            '.' => { tokens.push(Token { kind: TokenKind::Dot,     span: Span { line, col } }); col += 1; i += 1; }
            ':' => { tokens.push(Token { kind: TokenKind::Colon,   span: Span { line, col } }); col += 1; i += 1; }

            // `&` or `&&`
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token { kind: TokenKind::AmpAmp,    span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Ampersand, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // `|` or `||`
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token { kind: TokenKind::PipePipe, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Pipe,     span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // `=` or `==`
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { kind: TokenKind::EqEq,   span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Equals, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // `!` or `!=`
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { kind: TokenKind::BangEq, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Bang,   span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // `<`, `<<`, or `<=`
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '<' {
                    tokens.push(Token { kind: TokenKind::LtLt, span: Span { line, col } });
                    col += 2; i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { kind: TokenKind::LtEq, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Lt,   span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // `>`, `>>`, or `>=`
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { kind: TokenKind::GtGt, span: Span { line, col } });
                    col += 2; i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { kind: TokenKind::GtEq, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Gt,   span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

            // Anything else is an error
            _ => {
                return Err(JadeError::UnexpectedChar {
                    ch,
                    span: Span { line, col },
                });
            }
        }
    }

    // Apply semicolon insertion for the final line (no trailing newline case)
    if let Some(last) = tokens.last() {
        if is_line_terminator(&last.kind) {
            tokens.push(Token {
                kind: TokenKind::Semicolon,
                span: Span { line, col },
            });
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { line, col },
    });

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::error::JadeError;

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
    fn test_unexpected_char_error() {
        let err = tokenize("@").unwrap_err();
        assert!(matches!(err, JadeError::UnexpectedChar { ch: '@', .. }));
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
    fn test_tokenize_braces() {
        assert_eq!(
            kinds("{}"),
            vec![TokenKind::LBrace, TokenKind::RBrace, TokenKind::Eof]
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
        // `1.` is tokenized as Integer(1), then `.` is an unexpected character.
        // Float literals require at least one digit after the decimal point: `1.0`.
        let err = tokenize("let x = 1.").unwrap_err();
        assert!(matches!(err, JadeError::UnexpectedChar { ch: '.', .. }));
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
}
