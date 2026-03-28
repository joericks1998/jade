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
    if tokens.is_empty() {
        return Err(JadeError::UnexpectedEof {
            span: super::error::Span { line: 1, col: 1 },
        });
    }
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_program()
}

impl Parser {
    /// Returns a reference to the current token without advancing.
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos)
            .expect("parser advanced past end of token stream — missing Eof sentinel")
    }

    /// Returns the current token and advances the cursor.
    fn advance(&mut self) -> &Token {
        let token = self.tokens.get(self.pos)
            .expect("parser advanced past end of token stream — missing Eof sentinel");
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
        let value = self.parse_or()?;
        self.expect(&TokenKind::Semicolon)?;

        Ok(Stmt::Let { name, value, span })
    }

    /// Extract the span from any expression node.
    fn expr_span(e: &Expr) -> super::error::Span {
        match e {
            Expr::Integer    { span, .. } => *span,
            Expr::Float      { span, .. } => *span,
            Expr::Bool       { span, .. } => *span,
            Expr::Identifier { span, .. } => *span,
            Expr::BinOp      { span, .. } => *span,
            Expr::UnaryOp    { span, .. } => *span,
        }
    }

    /// Lowest precedence: `||` (logical OR).
    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        loop {
            if self.peek().kind == TokenKind::PipePipe {
                let span = Self::expr_span(&left);
                self.advance();
                let right = self.parse_and()?;
                left = Expr::BinOp { op: BinOpKind::Or, left: Box::new(left), right: Box::new(right), span };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `&&` (logical AND).
    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_comparison()?;
        loop {
            if self.peek().kind == TokenKind::AmpAmp {
                let span = Self::expr_span(&left);
                self.advance();
                let right = self.parse_comparison()?;
                left = Expr::BinOp { op: BinOpKind::And, left: Box::new(left), right: Box::new(right), span };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `==`, `!=`, `<`, `>`, `<=`, `>=` (comparison, non-associative).
    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_bitor()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::EqEq  => BinOpKind::Eq,
                TokenKind::BangEq => BinOpKind::Ne,
                TokenKind::Lt    => BinOpKind::Lt,
                TokenKind::Gt    => BinOpKind::Gt,
                TokenKind::LtEq  => BinOpKind::Le,
                TokenKind::GtEq  => BinOpKind::Ge,
                _ => break,
            };
            let span = Self::expr_span(&left);
            self.advance();
            let right = self.parse_bitor()?;
            left = Expr::BinOp { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    /// `|` (bitwise OR).
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

    /// Unary `~` (bitwise NOT), `!` (logical NOT), `-` (negation).
    fn parse_unary(&mut self) -> Result<Expr> {
        match self.peek().kind {
            TokenKind::Tilde => {
                let span = self.peek().span;
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::UnaryOp { op: UnaryOpKind::BitNot, operand: Box::new(operand), span })
            }
            TokenKind::Bang => {
                let span = self.peek().span;
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::UnaryOp { op: UnaryOpKind::Not, operand: Box::new(operand), span })
            }
            TokenKind::Minus => {
                let span = self.peek().span;
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::UnaryOp { op: UnaryOpKind::Neg, operand: Box::new(operand), span })
            }
            _ => self.parse_primary(),
        }
    }

    /// Parse a primary: literal, identifier, or parenthesized expression.
    fn parse_primary(&mut self) -> Result<Expr> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Integer(value) => {
                self.advance();
                Ok(Expr::Integer { value, span: token.span })
            }
            TokenKind::Float(value) => {
                self.advance();
                Ok(Expr::Float { value, span: token.span })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Bool { value: true, span: token.span })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Bool { value: false, span: token.span })
            }
            TokenKind::Identifier(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::Identifier { name, span: token.span })
            }
            TokenKind::LParen => {
                self.advance(); // consume `(`
                let expr = self.parse_or()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            TokenKind::Eof => Err(JadeError::UnexpectedEof { span: token.span }),
            _ => Err(JadeError::UnexpectedToken {
                expected: "expression".to_string(),
                got: format!("{:?}", token.kind),
                span: token.span,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{ast::{BinOpKind, Expr, Stmt, UnaryOpKind}, error::JadeError, lexer};

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
        let Stmt::Let { name, value, .. } = &p.stmts[0];
        assert_eq!(name, "x");
        assert!(matches!(value, Expr::Integer { value: 5, .. }));
    }

    #[test]
    fn test_parse_let_float() {
        let p = parse_src("let x = 1.5");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::Float { .. }));
    }

    #[test]
    fn test_parse_addition() {
        let p = parse_src("let x = 1 + 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Add, .. }));
    }

    #[test]
    fn test_parse_subtraction() {
        let p = parse_src("let x = 5 - 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Sub, .. }));
    }

    #[test]
    fn test_parse_multiplication() {
        let p = parse_src("let x = 2 * 4");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Mul, .. }));
    }

    #[test]
    fn test_parse_division() {
        let p = parse_src("let x = 8 / 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Div, .. }));
    }

    #[test]
    fn test_parse_modulo() {
        let p = parse_src("let x = 7 % 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Mod, .. }));
    }

    #[test]
    fn test_parse_bitwise_and() {
        let p = parse_src("let x = 6 & 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitAnd, .. }));
    }

    #[test]
    fn test_parse_bitwise_or() {
        let p = parse_src("let x = 6 | 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitOr, .. }));
    }

    #[test]
    fn test_parse_bitwise_xor() {
        let p = parse_src("let x = 6 ^ 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::BitXor, .. }));
    }

    #[test]
    fn test_parse_shl() {
        let p = parse_src("let x = 1 << 3");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Shl, .. }));
    }

    #[test]
    fn test_parse_shr() {
        let p = parse_src("let x = 16 >> 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Shr, .. }));
    }

    #[test]
    fn test_parse_bitnot() {
        let p = parse_src("let x = ~5");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::BitNot, .. }));
    }

    #[test]
    fn test_parse_neg() {
        let p = parse_src("let x = -5");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::Neg, .. }));
    }

    #[test]
    fn test_parse_identifier_ref() {
        let p = parse_src("let a = 1\nlet b = a");
        assert_eq!(p.stmts.len(), 2);
        let Stmt::Let { value, .. } = &p.stmts[1];
        assert!(matches!(value, Expr::Identifier { .. }));
    }

    #[test]
    fn test_parse_precedence_mul_before_add() {
        // 2 + 3 * 4  →  Add(2, Mul(3, 4))
        let p = parse_src("let x = 2 + 3 * 4");
        let Stmt::Let { value, .. } = &p.stmts[0];
        let Expr::BinOp { op: BinOpKind::Add, right, .. } = value else { panic!("expected Add") };
        assert!(matches!(right.as_ref(), Expr::BinOp { op: BinOpKind::Mul, .. }));
    }

    #[test]
    fn test_parse_precedence_grouped_overrides() {
        // (2 + 3) * 4  →  Mul(Add(2,3), 4)
        let p = parse_src("let x = (2 + 3) * 4");
        let Stmt::Let { value, .. } = &p.stmts[0];
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
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::Bool { value: true, .. }));
    }

    #[test]
    fn test_parse_bool_literal_false() {
        let p = parse_src("let x = false");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::Bool { value: false, .. }));
    }

    #[test]
    fn test_parse_logical_and() {
        let p = parse_src("let x = true && false");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::And, .. }));
    }

    #[test]
    fn test_parse_logical_or() {
        let p = parse_src("let x = true || false");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Or, .. }));
    }

    #[test]
    fn test_parse_logical_not() {
        let p = parse_src("let x = !true");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::UnaryOp { op: UnaryOpKind::Not, .. }));
    }

    #[test]
    fn test_parse_eq() {
        let p = parse_src("let x = 1 == 1");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Eq, .. }));
    }

    #[test]
    fn test_parse_ne() {
        let p = parse_src("let x = 1 != 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Ne, .. }));
    }

    #[test]
    fn test_parse_lt() {
        let p = parse_src("let x = 1 < 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Lt, .. }));
    }

    #[test]
    fn test_parse_gt() {
        let p = parse_src("let x = 2 > 1");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Gt, .. }));
    }

    #[test]
    fn test_parse_le() {
        let p = parse_src("let x = 1 <= 2");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Le, .. }));
    }

    #[test]
    fn test_parse_ge() {
        let p = parse_src("let x = 2 >= 1");
        let Stmt::Let { value, .. } = &p.stmts[0];
        assert!(matches!(value, Expr::BinOp { op: BinOpKind::Ge, .. }));
    }

    #[test]
    fn test_parse_precedence_compare_before_and() {
        // 1 < 2 && 3 > 0  →  And(Lt(1,2), Gt(3,0))
        let p = parse_src("let x = 1 < 2 && 3 > 0");
        let Stmt::Let { value, .. } = &p.stmts[0];
        let Expr::BinOp { op: BinOpKind::And, left, right, .. } = value else { panic!("expected And") };
        assert!(matches!(left.as_ref(),  Expr::BinOp { op: BinOpKind::Lt, .. }));
        assert!(matches!(right.as_ref(), Expr::BinOp { op: BinOpKind::Gt, .. }));
    }
}
