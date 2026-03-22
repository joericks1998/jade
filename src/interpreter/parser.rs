use super::{
    ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result},
    lexer::{Token, TokenKind},
};

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

/// Public entry point. Builds a Parser and drives it to produce a Program.
pub fn parse(tokens: Vec<Token>) -> Result<Program> {
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_program()
}

impl Parser {
    /// Returns a reference to the current token without advancing.
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Returns the current token and advances the cursor.
    fn advance(&mut self) -> &Token {
        let token = &self.tokens[self.pos];
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    /// If the current token matches `kind`, advance and return a clone.
    /// Otherwise return an error.
    fn expect(&mut self, kind: &TokenKind) -> Result<Token> {
        let token = self.peek().clone();
        if std::mem::discriminant(&token.kind) == std::mem::discriminant(kind) {
            self.advance();
            Ok(token)
        } else {
            Err(JadeError::UnexpectedToken {
                expected: format!("{:?}", kind),
                got: format!("{:?}", token.kind),
                span: token.span,
            })
        }
    }

    /// Parse zero or more statements until Eof.
    fn parse_program(&mut self) -> Result<Program> {
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::Eof {
            stmts.push(self.parse_stmt()?);
        }
        Ok(Program { stmts })
    }

    /// Parse a single statement.
    fn parse_stmt(&mut self) -> Result<Stmt> {
        match self.peek().kind {
            TokenKind::Let => self.parse_let(),
            _ => {
                let token = self.peek().clone();
                Err(JadeError::UnexpectedToken {
                    expected: "statement".to_string(),
                    got: format!("{:?}", token.kind),
                    span: token.span,
                })
            }
        }
    }

    /// Parse `let <ident> = <expr> ;`
    fn parse_let(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `let`

        let name_token = self.peek().clone();
        let name = match &name_token.kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => {
                return Err(JadeError::UnexpectedToken {
                    expected: "identifier".to_string(),
                    got: format!("{:?}", name_token.kind),
                    span: name_token.span,
                });
            }
        };

        self.expect(&TokenKind::Equals)?;
        let value = self.parse_bitor()?;
        self.expect(&TokenKind::Semicolon)?;

        Ok(Stmt::Let { name, value, span })
    }

    /// Extract the span from any expression node.
    fn expr_span(e: &Expr) -> super::error::Span {
        match e {
            Expr::Integer { span, .. } => *span,
            Expr::Identifier { span, .. } => *span,
            Expr::BinOp { span, .. } => *span,
            Expr::UnaryOp { span, .. } => *span,
        }
    }

    /// Lowest precedence: `|` (bitwise OR).
    fn parse_bitor(&mut self) -> Result<Expr> {
        let mut left = self.parse_bitxor()?;
        loop {
            if self.peek().kind == TokenKind::Pipe {
                let span = Self::expr_span(&left);
                self.advance();
                let right = self.parse_bitxor()?;
                left = Expr::BinOp { op: BinOpKind::BitOr, left: Box::new(left), right: Box::new(right), span };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `^` (bitwise XOR).
    fn parse_bitxor(&mut self) -> Result<Expr> {
        let mut left = self.parse_bitand()?;
        loop {
            if self.peek().kind == TokenKind::Caret {
                let span = Self::expr_span(&left);
                self.advance();
                let right = self.parse_bitand()?;
                left = Expr::BinOp { op: BinOpKind::BitXor, left: Box::new(left), right: Box::new(right), span };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `&` (bitwise AND).
    fn parse_bitand(&mut self) -> Result<Expr> {
        let mut left = self.parse_shift()?;
        loop {
            if self.peek().kind == TokenKind::Ampersand {
                let span = Self::expr_span(&left);
                self.advance();
                let right = self.parse_shift()?;
                left = Expr::BinOp { op: BinOpKind::BitAnd, left: Box::new(left), right: Box::new(right), span };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `<<` and `>>` (bit shifts).
    fn parse_shift(&mut self) -> Result<Expr> {
        let mut left = self.parse_expr()?;
        loop {
            match self.peek().kind {
                TokenKind::LtLt => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_expr()?;
                    left = Expr::BinOp { op: BinOpKind::Shl, left: Box::new(left), right: Box::new(right), span };
                }
                TokenKind::GtGt => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_expr()?;
                    left = Expr::BinOp { op: BinOpKind::Shr, left: Box::new(left), right: Box::new(right), span };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// `+` and `-` (additive).
    fn parse_expr(&mut self) -> Result<Expr> {
        let mut left = self.parse_term()?;
        loop {
            match self.peek().kind {
                TokenKind::Plus => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_term()?;
                    left = Expr::BinOp { op: BinOpKind::Add, left: Box::new(left), right: Box::new(right), span };
                }
                TokenKind::Minus => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_term()?;
                    left = Expr::BinOp { op: BinOpKind::Sub, left: Box::new(left), right: Box::new(right), span };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// `*`, `/`, `%` (multiplicative).
    fn parse_term(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek().kind {
                TokenKind::Star => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::BinOp { op: BinOpKind::Mul, left: Box::new(left), right: Box::new(right), span };
                }
                TokenKind::Slash => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::BinOp { op: BinOpKind::Div, left: Box::new(left), right: Box::new(right), span };
                }
                TokenKind::Percent => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::BinOp { op: BinOpKind::Mod, left: Box::new(left), right: Box::new(right), span };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// Unary `~` (bitwise NOT).
    fn parse_unary(&mut self) -> Result<Expr> {
        if self.peek().kind == TokenKind::Tilde {
            let span = self.peek().span;
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::UnaryOp { op: UnaryOpKind::BitNot, operand: Box::new(operand), span });
        }
        self.parse_primary()
    }

    /// Parse a primary: an integer literal or an identifier.
    fn parse_primary(&mut self) -> Result<Expr> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Integer(value) => {
                self.advance();
                Ok(Expr::Integer { value, span: token.span })
            }
            TokenKind::Identifier(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::Identifier { name, span: token.span })
            }
            TokenKind::Eof => Err(JadeError::UnexpectedEof { span: token.span }),
            _ => Err(JadeError::UnexpectedToken {
                expected: "integer or identifier".to_string(),
                got: format!("{:?}", token.kind),
                span: token.span,
            }),
        }
    }
}
