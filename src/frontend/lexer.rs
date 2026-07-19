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
    Async,
    Await,
    From,
    As,

    // Prompt dereference operator
    Question,
    // Postfix prompt dereference: `obj~>field` ≡ `obj.(?field)`
    TildeGt,

    // Decorator prefix
    At,

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

    // Assignment
    Equals,

    // Punctuation
    Comma,
    Semicolon,
    Dot,
    Colon,
    ColonColon,

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

/// Returns a concise, human-readable description of a token kind.
/// Used in parser error messages so users see `` `{` `` instead of `LBrace`.
pub fn token_kind_desc(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Integer(n)    => format!("`{}`", n),
        TokenKind::Float(f)      => format!("`{}`", f),
        TokenKind::Str(_)        => "string literal".to_string(),
        TokenKind::FStr(_)       => "f-string".to_string(),
        TokenKind::Identifier(s) => format!("`{}`", s),
        TokenKind::Let           => "`let`".to_string(),
        TokenKind::Fn            => "`fn`".to_string(),
        TokenKind::Return        => "`return`".to_string(),
        TokenKind::If            => "`if`".to_string(),
        TokenKind::Elif          => "`elif`".to_string(),
        TokenKind::Else          => "`else`".to_string(),
        TokenKind::While         => "`while`".to_string(),
        TokenKind::For           => "`for`".to_string(),
        TokenKind::In            => "`in`".to_string(),
        TokenKind::Struct        => "`struct`".to_string(),
        TokenKind::Extend        => "`extend`".to_string(),
        TokenKind::Interface     => "`interface`".to_string(),
        TokenKind::Prompt        => "`prompt`".to_string(),
        TokenKind::Use           => "`use`".to_string(),
        TokenKind::True          => "`true`".to_string(),
        TokenKind::False         => "`false`".to_string(),
        TokenKind::Raise         => "`raise`".to_string(),
        TokenKind::Try           => "`try`".to_string(),
        TokenKind::Catch         => "`catch`".to_string(),
        TokenKind::Async         => "`async`".to_string(),
        TokenKind::Await         => "`await`".to_string(),
        TokenKind::From          => "`from`".to_string(),
        TokenKind::As            => "`as`".to_string(),
        TokenKind::Question      => "`?`".to_string(),
        TokenKind::TildeGt       => "`~>`".to_string(),
        TokenKind::At            => "`@`".to_string(),
        TokenKind::Plus          => "`+`".to_string(),
        TokenKind::Minus         => "`-`".to_string(),
        TokenKind::Star          => "`*`".to_string(),
        TokenKind::Slash         => "`/`".to_string(),
        TokenKind::Percent       => "`%`".to_string(),
        TokenKind::Ampersand     => "`&`".to_string(),
        TokenKind::Pipe          => "`|`".to_string(),
        TokenKind::Caret         => "`^`".to_string(),
        TokenKind::Tilde         => "`~`".to_string(),
        TokenKind::LtLt          => "`<<`".to_string(),
        TokenKind::GtGt          => "`>>`".to_string(),
        TokenKind::AmpAmp        => "`&&`".to_string(),
        TokenKind::PipePipe      => "`||`".to_string(),
        TokenKind::PipeGt        => "`|>`".to_string(),
        TokenKind::Bang          => "`!`".to_string(),
        TokenKind::EqEq          => "`==`".to_string(),
        TokenKind::BangEq        => "`!=`".to_string(),
        TokenKind::Lt            => "`<`".to_string(),
        TokenKind::Gt            => "`>`".to_string(),
        TokenKind::LtEq          => "`<=`".to_string(),
        TokenKind::GtEq          => "`>=`".to_string(),
        TokenKind::Equals        => "`=`".to_string(),
        TokenKind::Comma         => "`,`".to_string(),
        TokenKind::Semicolon     => "`;`".to_string(),
        TokenKind::Dot           => "`.`".to_string(),
        TokenKind::Colon         => "`:`".to_string(),
        TokenKind::ColonColon    => "`::`".to_string(),
        TokenKind::LParen        => "`(`".to_string(),
        TokenKind::RParen        => "`)`".to_string(),
        TokenKind::LBrace        => "`{`".to_string(),
        TokenKind::RBrace        => "`}`".to_string(),
        TokenKind::LBracket      => "`[`".to_string(),
        TokenKind::RBracket      => "`]`".to_string(),
        TokenKind::Eof           => "end of file".to_string(),
    }
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

/// Scan the content of a plain string after the opening quote(s) have been consumed.
/// `quote` is either `'"'` or `'\''`.  Advances `i`, `col`, and `line` in-place.
fn scan_str_content(
    chars: &[char],
    i: &mut usize,
    col: &mut usize,
    line: &mut usize,
    start_line: usize,
    start_col: usize,
    triple: bool,
    quote: char,
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
            if chars.get(*i) == Some(&quote)
                && chars.get(*i + 1) == Some(&quote)
                && chars.get(*i + 2) == Some(&quote)
            {
                *col += 3;
                *i += 3;
                break;
            }
        } else if chars[*i] == quote {
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
                let esc = chars[*i];
                if esc == quote {
                    content.push(quote); *i += 1; *col += 1;
                } else {
                    match esc {
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
            }
            '\n' => { content.push('\n'); *line += 1; *col = 1; *i += 1; }
            ch   => { content.push(ch);  *col += 1; *i += 1; }
        }
    }
    Ok(content)
}

/// Scan the content of an f-string after the opening quote(s) have been consumed.
/// `quote` is either `'"'` or `'\''`.  Returns the raw parts (literal segments and
/// expression source texts).  Advances `i`, `col`, and `line` in-place.
fn scan_fstr_content(
    chars: &[char],
    i: &mut usize,
    col: &mut usize,
    line: &mut usize,
    start_line: usize,
    start_col: usize,
    triple: bool,
    quote: char,
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
            if chars.get(*i) == Some(&quote)
                && chars.get(*i + 1) == Some(&quote)
                && chars.get(*i + 2) == Some(&quote)
            {
                *col += 3;
                *i += 3;
                break;
            }
        } else if chars[*i] == quote {
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
                let esc = chars[*i];
                if esc == quote {
                    literal.push(quote); *i += 1; *col += 1;
                } else {
                    match esc {
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
    // Track open ( and [ depth: semicolons are never inserted inside them so that
    // multi-line argument lists (e.g. join(\n  f(),\n  g()\n)) parse correctly.
    let mut bracket_depth: i32 = 0;

    while i < chars.len() {
        let ch = chars[i];

        match ch {
            // Newline: possibly insert a semicolon, then advance to next line
            '\n' => {
                if bracket_depth == 0 {
                    if let Some(last) = tokens.last() {
                        if is_line_terminator(&last.kind) {
                            tokens.push(Token {
                                kind: TokenKind::Semicolon,
                                span: Span { line, col },
                            });
                        }
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

            // String literals: "..." / """...""" or '...' / '''...'''
            '"' | '\'' => {
                let quote = ch;
                let start_col = col;
                let start_line = line;
                let triple = chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote);
                if triple { col += 3; i += 3; } else { col += 1; i += 1; }
                let content = scan_str_content(
                    &chars, &mut i, &mut col, &mut line,
                    start_line, start_col, triple, quote,
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
                    "async"     => TokenKind::Async,
                    "await"     => TokenKind::Await,
                    "from"      => TokenKind::From,
                    "as"        => TokenKind::As,
                    "true"      => TokenKind::True,
                    "false"     => TokenKind::False,
                    "and"       => TokenKind::AmpAmp,
                    "or"        => TokenKind::PipePipe,
                    "not"       => TokenKind::Bang,
                    // f-string: `f"…"` / `f"""…"""` or `f'…'` / `f'''…'''`
                    "f" if matches!(chars.get(i), Some(&'"') | Some(&'\'')) => {
                        let quote = chars[i];
                        let start_line = line;
                        let triple = chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote);
                        if triple { col += 3; i += 3; } else { col += 1; i += 1; }
                        let parts = scan_fstr_content(
                            &chars, &mut i, &mut col, &mut line,
                            start_line, start_col, triple, quote,
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
            // `~` (bitwise NOT) or `~>` (postfix prompt dereference)
            '~' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { kind: TokenKind::TildeGt, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Tilde, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }
            '^' => { tokens.push(Token { kind: TokenKind::Caret,   span: Span { line, col } }); col += 1; i += 1; }
            '(' => { tokens.push(Token { kind: TokenKind::LParen,  span: Span { line, col } }); bracket_depth += 1; col += 1; i += 1; }
            ')' => { tokens.push(Token { kind: TokenKind::RParen,  span: Span { line, col } }); bracket_depth -= 1; col += 1; i += 1; }
            '{' => { tokens.push(Token { kind: TokenKind::LBrace,    span: Span { line, col } }); col += 1; i += 1; }
            '}' => { tokens.push(Token { kind: TokenKind::RBrace,    span: Span { line, col } }); col += 1; i += 1; }
            '[' => { tokens.push(Token { kind: TokenKind::LBracket,  span: Span { line, col } }); bracket_depth += 1; col += 1; i += 1; }
            ']' => { tokens.push(Token { kind: TokenKind::RBracket,  span: Span { line, col } }); bracket_depth -= 1; col += 1; i += 1; }
            ',' => { tokens.push(Token { kind: TokenKind::Comma,   span: Span { line, col } }); col += 1; i += 1; }
            '.' => { tokens.push(Token { kind: TokenKind::Dot,     span: Span { line, col } }); col += 1; i += 1; }
            ':' => {
                if i + 1 < chars.len() && chars[i + 1] == ':' {
                    tokens.push(Token { kind: TokenKind::ColonColon, span: Span { line, col } });
                    col += 2; i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Colon, span: Span { line, col } });
                    col += 1; i += 1;
                }
            }

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

            // `@` — decorator prefix
            '@' => { tokens.push(Token { kind: TokenKind::At, span: Span { line, col } }); col += 1; i += 1; }

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
    if bracket_depth == 0 {
        if let Some(last) = tokens.last() {
            if is_line_terminator(&last.kind) {
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    span: Span { line, col },
                });
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { line, col },
    });

    Ok(tokens)
}
