use super::error::{JadeError, Result, Span};

/// A raw part of an f-string as produced by the lexer.
/// The `Expr` variant holds the raw source text inside `{…}` — it is
/// parsed into an `Expr` node later by the parser.
#[derive(Debug, Clone, PartialEq)]
pub enum RawFStrPart {
    Literal(String),
    Expr(String),
}

/// Every kind of token the lexer can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Integer(i64),
    Float(f64),
    Str(String),
    /// An interpolated string: `f"…"` or `f"""…"""`.
    FStr(Vec<RawFStrPart>),

    // Identifiers and keywords
    Identifier(String),
    Let,
    Fn,
    Return,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Struct,
    Extend,
    Interface,
    Prompt,
    Use,
    True,
    False,
    Raise,
    Try,
    Catch,

    // Prompt dereference operator
    Question,

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

    // Pipe operator
    PipeGt,
    Bang,

    // Comparison operators
    EqEq,
    BangEq,
    Lt,
    Gt,
    LtEq,
    GtEq,

    /// `->` (return type annotation in interface method signatures)
    Arrow,

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
    LBracket,
    RBracket,

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
            | TokenKind::Str(_)
            | TokenKind::FStr(_)
            | TokenKind::Identifier(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::RParen
            // RBrace triggers semicolons so struct literals (`let p = Point { … }`)
            // on their own line terminate the statement correctly. The parser handles
            // `} else {` by consuming the inserted semicolon before checking for `else`.
            | TokenKind::RBrace
            // RBracket triggers semicolons after index expressions: `s[0]` at end of line.
            | TokenKind::RBracket
    )
}

/// Scan the content of a plain string (`"…"` or `"""…"""`) after the opening
/// quote(s) have been consumed.  Advances `i`, `col`, and `line` in-place.
fn scan_str_content(
    chars: &[char],
    i: &mut usize,
    col: &mut usize,
    line: &mut usize,
    start_line: usize,
    start_col: usize,
    triple: bool,
) -> Result<String> {
    let mut content = String::new();
    loop {
        if *i >= chars.len() {
            return Err(JadeError::UnterminatedString {
                span: Span { line: start_line, col: start_col },
            });
        }
        // Closing-quote detection
        if triple {
            if chars.get(*i) == Some(&'"')
                && chars.get(*i + 1) == Some(&'"')
                && chars.get(*i + 2) == Some(&'"')
            {
                *col += 3;
                *i += 3;
                break;
            }
        } else if chars[*i] == '"' {
            *col += 1;
            *i += 1;
            break;
        }
        match chars[*i] {
            '\\' => {
                *i += 1;
                *col += 1;
                if *i >= chars.len() {
                    return Err(JadeError::UnterminatedString {
                        span: Span { line: start_line, col: start_col },
                    });
                }
                match chars[*i] {
                    '"'  => { content.push('"');  *i += 1; *col += 1; }
                    '\\' => { content.push('\\'); *i += 1; *col += 1; }
                    'n'  => { content.push('\n'); *i += 1; *col += 1; }
                    't'  => { content.push('\t'); *i += 1; *col += 1; }
                    'r'  => { content.push('\r'); *i += 1; *col += 1; }
                    other => return Err(JadeError::UnexpectedChar {
                        ch: other,
                        span: Span { line: *line, col: *col },
                    }),
                }
            }
            '\n' => { content.push('\n'); *line += 1; *col = 1; *i += 1; }
            ch   => { content.push(ch);  *col += 1; *i += 1; }
        }
    }
    Ok(content)
}

/// Scan the content of an f-string (`f"…"` or `f"""…"""`) after the opening
/// quote(s) have been consumed.  Returns the raw parts (literal segments and
/// expression source texts).  Advances `i`, `col`, and `line` in-place.
fn scan_fstr_content(
    chars: &[char],
    i: &mut usize,
    col: &mut usize,
    line: &mut usize,
    start_line: usize,
    start_col: usize,
    triple: bool,
) -> Result<Vec<RawFStrPart>> {
    let mut parts: Vec<RawFStrPart> = Vec::new();
    let mut literal = String::new();
    loop {
        if *i >= chars.len() {
            return Err(JadeError::UnterminatedString {
                span: Span { line: start_line, col: start_col },
            });
        }
        // Closing-quote detection
        if triple {
            if chars.get(*i) == Some(&'"')
                && chars.get(*i + 1) == Some(&'"')
                && chars.get(*i + 2) == Some(&'"')
            {
                *col += 3;
                *i += 3;
                break;
            }
        } else if chars[*i] == '"' {
            *col += 1;
            *i += 1;
            break;
        }
        match chars[*i] {
            '{' => {
                // Flush accumulated literal segment
                if !literal.is_empty() {
                    parts.push(RawFStrPart::Literal(std::mem::take(&mut literal)));
                }
                *col += 1;
                *i += 1; // consume '{'
                // Scan expression source until the matching '}'
                let mut expr_src = String::new();
                let mut depth = 1usize;
                loop {
                    if *i >= chars.len() {
                        return Err(JadeError::UnterminatedString {
                            span: Span { line: start_line, col: start_col },
                        });
                    }
                    match chars[*i] {
                        '{' => { depth += 1; expr_src.push('{'); *col += 1; *i += 1; }
                        '}' => {
                            depth -= 1;
                            if depth == 0 { *col += 1; *i += 1; break; }
                            expr_src.push('}'); *col += 1; *i += 1;
                        }
                        '\n' => { expr_src.push('\n'); *line += 1; *col = 1; *i += 1; }
                        c    => { expr_src.push(c); *col += 1; *i += 1; }
                    }
                }
                parts.push(RawFStrPart::Expr(expr_src));
            }
            '\\' => {
                *i += 1;
                *col += 1;
                if *i >= chars.len() {
                    return Err(JadeError::UnterminatedString {
                        span: Span { line: start_line, col: start_col },
                    });
                }
                match chars[*i] {
                    '"'  => { literal.push('"');  *i += 1; *col += 1; }
                    '\\' => { literal.push('\\'); *i += 1; *col += 1; }
                    'n'  => { literal.push('\n'); *i += 1; *col += 1; }
                    't'  => { literal.push('\t'); *i += 1; *col += 1; }
                    'r'  => { literal.push('\r'); *i += 1; *col += 1; }
                    '{'  => { literal.push('{');  *i += 1; *col += 1; }
                    '}'  => { literal.push('}');  *i += 1; *col += 1; }
                    other => return Err(JadeError::UnexpectedChar {
                        ch: other,
                        span: Span { line: *line, col: *col },
                    }),
                }
            }
            '\n' => { literal.push('\n'); *line += 1; *col = 1; *i += 1; }
            ch   => { literal.push(ch);  *col += 1; *i += 1; }
        }
    }
    if !literal.is_empty() {
        parts.push(RawFStrPart::Literal(literal));
    }
    Ok(parts)
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

            // String literals: "..." or """..."""
            '"' => {
                let start_col = col;
                let start_line = line;
                let triple = chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"');
                if triple { col += 3; i += 3; } else { col += 1; i += 1; }
                let content = scan_str_content(
                    &chars, &mut i, &mut col, &mut line,
                    start_line, start_col, triple,
                )?;
                tokens.push(Token {
                    kind: TokenKind::Str(content),
                    span: Span { line: start_line, col: start_col },
                });
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
                    "elif"   => TokenKind::Elif,
                    "else"   => TokenKind::Else,
                    "while"  => TokenKind::While,
                    "for"    => TokenKind::For,
                    "in"     => TokenKind::In,
                    "struct"    => TokenKind::Struct,
                    "extend"    => TokenKind::Extend,
                    "interface" => TokenKind::Interface,
                    "prompt"    => TokenKind::Prompt,
                    "use"       => TokenKind::Use,
                    "raise"     => TokenKind::Raise,
                    "try"       => TokenKind::Try,
                    "catch"     => TokenKind::Catch,
                    "true"      => TokenKind::True,
                    "false"  => TokenKind::False,
                    // f-string: `f"…"` or `f"""…"""`
                    "f" if chars.get(i) == Some(&'"') => {
                        let start_line = line; // capture before mutable borrow
                        let triple = chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"');
                        if triple { col += 3; i += 3; } else { col += 1; i += 1; }
                        let parts = scan_fstr_content(
                            &chars, &mut i, &mut col, &mut line,
                            start_line, start_col, triple,
                        )?;
                        TokenKind::FStr(parts)
                    }
                    _        => TokenKind::Identifier(name),
                };
                tokens.push(Token {
                    kind,
                    span: Span { line, col: start_col },
                });
            }

            // Unambiguous single-character tokens
            '+' => { tokens.push(Token { kind: TokenKind::Plus,    span: Span { line, col } }); col += 1; i += 1; }
            '-' => {
                if chars.get(i + 1) == Some(&'>') {
                    tokens.push(Token { kind: TokenKind::Arrow, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Minus, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }
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
            '{' => { tokens.push(Token { kind: TokenKind::LBrace,    span: Span { line, col } }); col += 1; i += 1; }
            '}' => { tokens.push(Token { kind: TokenKind::RBrace,    span: Span { line, col } }); col += 1; i += 1; }
            '[' => { tokens.push(Token { kind: TokenKind::LBracket,  span: Span { line, col } }); col += 1; i += 1; }
            ']' => { tokens.push(Token { kind: TokenKind::RBracket,  span: Span { line, col } }); col += 1; i += 1; }
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

            // `|`, `||`, or `|>`
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token { kind: TokenKind::PipePipe, span: Span { line, col } });
                    col += 2; i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { kind: TokenKind::PipeGt,   span: Span { line, col } });
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

            // `?` — prompt dereference operator
            '?' => { tokens.push(Token { kind: TokenKind::Question, span: Span { line, col } }); col += 1; i += 1; }

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
}
