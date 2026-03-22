use super::error::{JadeError, Result, Span};

/// Every kind of token the lexer can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Integer(i64),

    // Identifiers and keywords
    Identifier(String),
    Let,

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

    // Assignment
    Equals,

    // Auto-inserted punctuation
    Semicolon,

    // Grouping
    LParen,
    RParen,

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
    matches!(kind, TokenKind::Integer(_) | TokenKind::Identifier(_) | TokenKind::RParen)
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

            // Integer literals
            '0'..='9' => {
                let start_col = col;
                let mut num_str = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    num_str.push(chars[i]);
                    i += 1;
                    col += 1;
                }
                let value: i64 = num_str.parse().unwrap();
                tokens.push(Token {
                    kind: TokenKind::Integer(value),
                    span: Span { line, col: start_col },
                });
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
                    "let" => TokenKind::Let,
                    _ => TokenKind::Identifier(name),
                };
                tokens.push(Token {
                    kind,
                    span: Span { line, col: start_col },
                });
            }

            // Single-character tokens
            '+' => { tokens.push(Token { kind: TokenKind::Plus,      span: Span { line, col } }); col += 1; i += 1; }
            '-' => { tokens.push(Token { kind: TokenKind::Minus,     span: Span { line, col } }); col += 1; i += 1; }
            '*' => { tokens.push(Token { kind: TokenKind::Star,      span: Span { line, col } }); col += 1; i += 1; }
            '/' => { tokens.push(Token { kind: TokenKind::Slash,     span: Span { line, col } }); col += 1; i += 1; }
            '%' => { tokens.push(Token { kind: TokenKind::Percent,   span: Span { line, col } }); col += 1; i += 1; }
            '&' => { tokens.push(Token { kind: TokenKind::Ampersand, span: Span { line, col } }); col += 1; i += 1; }
            '|' => { tokens.push(Token { kind: TokenKind::Pipe,      span: Span { line, col } }); col += 1; i += 1; }
            '^' => { tokens.push(Token { kind: TokenKind::Caret,     span: Span { line, col } }); col += 1; i += 1; }
            '~' => { tokens.push(Token { kind: TokenKind::Tilde,     span: Span { line, col } }); col += 1; i += 1; }
            '=' => { tokens.push(Token { kind: TokenKind::Equals,    span: Span { line, col } }); col += 1; i += 1; }
            '(' => { tokens.push(Token { kind: TokenKind::LParen,    span: Span { line, col } }); col += 1; i += 1; }
            ')' => { tokens.push(Token { kind: TokenKind::RParen,    span: Span { line, col } }); col += 1; i += 1; }

            // Two-character tokens: << and >>
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '<' {
                    tokens.push(Token { kind: TokenKind::LtLt, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    return Err(JadeError::UnexpectedChar { ch, span: Span { line, col } });
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { kind: TokenKind::GtGt, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    return Err(JadeError::UnexpectedChar { ch, span: Span { line, col } });
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
