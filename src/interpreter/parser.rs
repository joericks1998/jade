use super::{
    ast::{BinOpKind, Expr, FStrPart, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result, Span},
    lexer::{RawFStrPart, Token, TokenKind},
};

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Depth of nested `fn` definitions. Used to detect and reject nested fns.
    fn_depth: usize,
    /// When false, a bare identifier followed by `{` is NOT parsed as a struct
    /// literal. Set to false while parsing `if`/`while` conditions so that
    /// `while running { … }` does not try to read `running {…}` as a struct.
    struct_literal_allowed: bool,
}

/// Public entry point. Builds a Parser and drives it to produce a Program.
pub fn parse(tokens: Vec<Token>) -> Result<Program> {
    if tokens.is_empty() {
        return Err(JadeError::UnexpectedEof {
            span: Span { line: 1, col: 1 },
        });
    }
    let mut parser = Parser { tokens, pos: 0, fn_depth: 0, struct_literal_allowed: true };
    parser.parse_program()
}

impl Parser {
    /// Returns a reference to the token `offset` positions ahead without advancing.
    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    /// Consume the current token if it is an identifier and return its name;
    /// otherwise return an `UnexpectedToken` error.
    fn expect_ident(&mut self, context: &str) -> Result<String> {
        let token = self.peek().clone();
        match &token.kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(JadeError::UnexpectedToken {
                expected: context.to_string(),
                got: format!("{:?}", token.kind),
                span: token.span,
            }),
        }
    }

    /// Returns a reference to the current token without advancing.
    // Safety: `parse()` rejects empty token streams. `advance()` is clamped at
    // the Eof sentinel, so `self.pos` is always a valid index. The fallback to
    // the last token is an extra safety net — it returns Eof rather than panicking.
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos)
            .unwrap_or_else(|| &self.tokens[self.tokens.len() - 1])
    }

    /// Returns the current token and advances the cursor.
    /// Clamped at the `Eof` sentinel: once `pos` reaches the last token
    /// (`Eof`), further calls return `Eof` without advancing past it.
    fn advance(&mut self) -> &Token {
        let token = &self.tokens[self.pos];
        // Clamped at Eof sentinel: pos never exceeds tokens.len()-1
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    /// If the current token's *variant* matches `kind`, advance and return a clone.
    /// Note: only the discriminant is compared — the payload (e.g. the integer value
    /// inside `Integer(n)`) is ignored. This is intentional: callers always pass a
    /// representative value like `&TokenKind::Semicolon` where only the variant matters.
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
    /// Semicolons between top-level statements are skipped; they can appear
    /// after any closing `}` now that `RBrace` is a line-terminator.
    fn parse_program(&mut self) -> Result<Program> {
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::Eof {
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::Eof {
                break;
            }
            stmts.push(self.parse_stmt()?);
        }
        Ok(Program { stmts })
    }

    /// Parse a single statement.
    fn parse_stmt(&mut self) -> Result<Stmt> {
        match self.peek().kind {
            TokenKind::Let    => self.parse_let(),
            TokenKind::Fn     => self.parse_fn(),
            TokenKind::Return => self.parse_return(),
            TokenKind::If     => self.parse_if(),
            TokenKind::While  => self.parse_while(),
            TokenKind::Struct => self.parse_struct_def(),
            TokenKind::Extend => self.parse_extend_block(),
            TokenKind::Identifier(_) => {
                // Disambiguate the three identifier-led statement forms:
                //   `ident =`            → bare variable assignment
                //   `ident . ident =`    → struct field assignment
                //   anything else        → expression statement (e.g. method call)
                let next_is_eq = self.peek_at(1)
                    .map(|t| t.kind == TokenKind::Equals).unwrap_or(false);
                let next_is_dot = self.peek_at(1)
                    .map(|t| t.kind == TokenKind::Dot).unwrap_or(false);
                let dot_field_eq = next_is_dot
                    && self.peek_at(2).map(|t| matches!(t.kind, TokenKind::Identifier(_))).unwrap_or(false)
                    && self.peek_at(3).map(|t| t.kind == TokenKind::Equals).unwrap_or(false);

                if next_is_eq {
                    self.parse_assign()
                } else if dot_field_eq {
                    self.parse_field_assign()
                } else {
                    self.parse_expr_stmt()
                }
            }
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

    /// Parse `<ident> = <expr> ;`
    fn parse_assign(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        let name = match &self.peek().kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => unreachable!("parse_assign called without leading identifier"),
        };
        self.expect(&TokenKind::Equals)?;
        let value = self.parse_or()?;
        self.expect(&TokenKind::Semicolon)?;
        Ok(Stmt::Assign { name, value, span })
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

    /// Parse `fn <ident> ( <params> ) { <body> }`
    fn parse_fn(&mut self) -> Result<Stmt> {
        let span = self.peek().span;

        // Nested fn definitions are not allowed
        if self.fn_depth > 0 {
            return Err(JadeError::NestedFunction { span });
        }

        self.advance(); // consume `fn`

        // Function name
        let name_token = self.peek().clone();
        let name = match &name_token.kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => {
                return Err(JadeError::UnexpectedToken {
                    expected: "function name".to_string(),
                    got: format!("{:?}", name_token.kind),
                    span: name_token.span,
                });
            }
        };

        // Parameter list
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                let param_token = self.peek().clone();
                match &param_token.kind {
                    TokenKind::Identifier(p) => {
                        params.push(p.clone());
                        self.advance();
                    }
                    _ => {
                        return Err(JadeError::UnexpectedToken {
                            expected: "parameter name".to_string(),
                            got: format!("{:?}", param_token.kind),
                            span: param_token.span,
                        });
                    }
                }
                if self.peek().kind == TokenKind::Comma {
                    self.advance(); // consume `,`
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen)?;

        // Body block
        self.fn_depth += 1;
        let body = self.parse_block()?;
        self.fn_depth -= 1;

        Ok(Stmt::FnDef { name, params, body, span })
    }

    /// Parse `return <expr> ;` or `return ;`
    fn parse_return(&mut self) -> Result<Stmt> {
        let span = self.peek().span;

        if self.fn_depth == 0 {
            return Err(JadeError::ReturnOutsideFunction { span });
        }

        self.advance(); // consume `return`

        // If the next token is a semicolon, it's a bare return
        if self.peek().kind == TokenKind::Semicolon {
            self.advance();
            return Ok(Stmt::Return { value: None, span });
        }

        let value = self.parse_or()?;
        self.expect(&TokenKind::Semicolon)?;
        Ok(Stmt::Return { value: Some(value), span })
    }

    /// Parse an expression that will be used as a control-flow condition.
    /// Struct literals are disallowed here so that `while running { … }` does
    /// not try to interpret `running {…}` as a struct literal.
    fn parse_condition(&mut self) -> Result<Expr> {
        let saved = self.struct_literal_allowed;
        self.struct_literal_allowed = false;
        let cond = self.parse_or()?;
        self.struct_literal_allowed = saved;
        Ok(cond)
    }

    /// Parse `if <condition> { <then> }`, optional `elif` chain, optional `else`.
    fn parse_if(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `if`

        let condition = self.parse_condition()?;
        let then_body = self.parse_block()?;

        // RBrace inserts an auto-semicolon; consume it before checking for elif/else.
        if self.peek().kind == TokenKind::Semicolon {
            self.advance();
        }

        let else_body = if self.peek().kind == TokenKind::Elif {
            // Desugar `elif cond { … }` into `else { if cond { … } … }`
            Some(vec![self.parse_elif()?])
        } else if self.peek().kind == TokenKind::Else {
            self.advance(); // consume `else`
            Some(self.parse_block()?)
        } else {
            None
        };

        Ok(Stmt::If { condition, then_body, else_body, span })
    }

    /// Parse `elif <condition> { <then> }` (and any further elif/else chain).
    /// Returns a `Stmt::If` that represents the desugared else-branch.
    fn parse_elif(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `elif`

        let condition = self.parse_condition()?;
        let then_body = self.parse_block()?;

        if self.peek().kind == TokenKind::Semicolon {
            self.advance();
        }

        let else_body = if self.peek().kind == TokenKind::Elif {
            Some(vec![self.parse_elif()?])
        } else if self.peek().kind == TokenKind::Else {
            self.advance(); // consume `else`
            Some(self.parse_block()?)
        } else {
            None
        };

        Ok(Stmt::If { condition, then_body, else_body, span })
    }

    /// Parse `while <condition> { <body> }`
    fn parse_while(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `while`

        let condition = self.parse_condition()?;
        let body = self.parse_block()?;

        Ok(Stmt::While { condition, body, span })
    }

    /// Parse `{ <stmts> }` — a brace-delimited block of statements.
    /// Leading semicolons between statements are skipped; they can appear after
    /// any closing `}` now that `RBrace` is a line-terminator.
    fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        loop {
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(stmts)
    }

    /// Parse `struct Name { field, … }`
    fn parse_struct_def(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `struct`
        let name = self.expect_ident("struct name")?;
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        loop {
            // Skip auto-semicolons between field names (from trailing newlines)
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            fields.push(self.expect_ident("field name")?);
            // Allow a trailing comma or semicolon after each field name
            if self.peek().kind == TokenKind::Comma || self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Stmt::StructDef { name, fields, span })
    }

    /// Parse `extend TypeName { fn method(self, …) { … } … }`
    fn parse_extend_block(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `extend`
        let type_name = self.expect_ident("type name")?;
        self.expect(&TokenKind::LBrace)?;
        let mut methods = Vec::new();
        loop {
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            match self.peek().kind {
                TokenKind::Fn => methods.push(self.parse_fn()?),
                _ => {
                    let t = self.peek().clone();
                    return Err(JadeError::UnexpectedToken {
                        expected: "fn".to_string(),
                        got: format!("{:?}", t.kind),
                        span: t.span,
                    });
                }
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Stmt::ExtendBlock { type_name, methods, span })
    }

    /// Parse `object.field = expr ;`
    fn parse_field_assign(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        let object = self.expect_ident("object name")?;
        self.expect(&TokenKind::Dot)?;
        let field = self.expect_ident("field name")?;
        self.expect(&TokenKind::Equals)?;
        let value = self.parse_or()?;
        self.expect(&TokenKind::Semicolon)?;
        Ok(Stmt::FieldAssign { object, field, value, span })
    }

    /// Parse an expression used as a statement (value discarded), e.g. `obj.method(args)`.
    fn parse_expr_stmt(&mut self) -> Result<Stmt> {
        let expr = self.parse_or()?;
        self.expect(&TokenKind::Semicolon)?;
        Ok(Stmt::Expr(expr))
    }

    /// Extract the span from any expression node.
    fn expr_span(e: &Expr) -> Span {
        match e {
            Expr::Integer      { span, .. } => *span,
            Expr::Float        { span, .. } => *span,
            Expr::Bool         { span, .. } => *span,
            Expr::Str          { span, .. } => *span,
            Expr::Identifier   { span, .. } => *span,
            Expr::Call         { span, .. } => *span,
            Expr::BinOp        { span, .. } => *span,
            Expr::UnaryOp      { span, .. } => *span,
            Expr::StructLiteral{ span, .. } => *span,
            Expr::FieldAccess  { span, .. } => *span,
            Expr::Index        { span, .. } => *span,
            Expr::FStr         { span, .. } => *span,
        }
    }

    /// Re-lex and re-parse the raw expression source from inside an f-string slot.
    /// Called once per `{…}` interpolation during `parse_primary` for `FStr` tokens.
    fn parse_fstr_expr(src: &str, span: Span, fn_depth: usize) -> Result<Expr> {
        let sub_tokens = super::lexer::tokenize(src).map_err(|_| JadeError::UnexpectedToken {
            expected: "expression".to_string(),
            got: format!("invalid f-string expression: {:?}", src),
            span,
        })?;
        let mut sub = Parser {
            tokens: sub_tokens,
            pos: 0,
            fn_depth,
            struct_literal_allowed: true,
        };
        sub.parse_or()
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
                TokenKind::EqEq   => BinOpKind::Eq,
                TokenKind::BangEq => BinOpKind::Ne,
                TokenKind::Lt     => BinOpKind::Lt,
                TokenKind::Gt     => BinOpKind::Gt,
                TokenKind::LtEq   => BinOpKind::Le,
                TokenKind::GtEq   => BinOpKind::Ge,
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
        let mut left = self.parse_additive()?;
        loop {
            match self.peek().kind {
                TokenKind::LtLt => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_additive()?;
                    left = Expr::BinOp { op: BinOpKind::Shl, left: Box::new(left), right: Box::new(right), span };
                }
                TokenKind::GtGt => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_additive()?;
                    left = Expr::BinOp { op: BinOpKind::Shr, left: Box::new(left), right: Box::new(right), span };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// `+` and `-` (additive).
    fn parse_additive(&mut self) -> Result<Expr> {
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
            _ => self.parse_call(),
        }
    }

    /// Parse the `{ field: expr, … }` body of a struct literal, given that the
    /// type name has already been consumed.
    fn parse_struct_literal_body(&mut self, type_name: String, span: Span) -> Result<Expr> {
        self.advance(); // consume `{`
        let mut fields = Vec::new();
        loop {
            // Skip any auto-inserted semicolons (e.g. after the last field's value)
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            let field_name = self.expect_ident("field name")?;
            self.expect(&TokenKind::Colon)?;
            let value = self.parse_or()?;
            fields.push((field_name, value));
            // Allow a trailing comma or semicolon after each field value
            if self.peek().kind == TokenKind::Comma || self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::StructLiteral { type_name, fields, span })
    }

    /// Parse a primary expression, then handle any trailing `.field` or `(args)` postfix.
    /// This naturally chains: `p.method(arg)` → FieldAccess then Call.
    fn parse_call(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.peek().kind == TokenKind::LParen {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `(`
                let mut args = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    args.push(self.parse_or()?);
                    while self.peek().kind == TokenKind::Comma {
                        self.advance(); // consume `,`
                        args.push(self.parse_or()?);
                    }
                }
                self.expect(&TokenKind::RParen)?;
                expr = Expr::Call { callee: Box::new(expr), args, span };
            } else if self.peek().kind == TokenKind::Dot {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `.`
                let field = self.expect_ident("field name")?;
                expr = Expr::FieldAccess { object: Box::new(expr), field, span };
            } else if self.peek().kind == TokenKind::LBracket {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `[`
                let index = self.parse_or()?;
                self.expect(&TokenKind::RBracket)?;
                expr = Expr::Index { object: Box::new(expr), index: Box::new(index), span };
            } else {
                break;
            }
        }
        Ok(expr)
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
            TokenKind::Str(ref value) => {
                let value = value.clone();
                self.advance();
                Ok(Expr::Str { value, span: token.span })
            }
            TokenKind::FStr(raw_parts) => {
                let span = token.span;
                self.advance();
                let mut parts = Vec::with_capacity(raw_parts.len());
                for part in raw_parts {
                    match part {
                        RawFStrPart::Literal(s) => parts.push(FStrPart::Literal(s)),
                        RawFStrPart::Expr(src) => {
                            let expr = Self::parse_fstr_expr(&src, span, self.fn_depth)?;
                            parts.push(FStrPart::Expr(expr));
                        }
                    }
                }
                Ok(Expr::FStr { parts, span })
            }
            TokenKind::Identifier(ref name) => {
                let name = name.clone();
                self.advance();
                // `TypeName { field: expr, … }` is a struct literal, but only when
                // struct literals are allowed in this position (not in if/while conditions).
                if self.struct_literal_allowed && self.peek().kind == TokenKind::LBrace {
                    self.parse_struct_literal_body(name, token.span)
                } else {
                    Ok(Expr::Identifier { name, span: token.span })
                }
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
    use crate::interpreter::{
        ast::{BinOpKind, Expr, Stmt, UnaryOpKind},
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
        let Expr::BinOp { op: BinOpKind::And, left, right, .. } = value else { panic!("expected And") };
        assert!(matches!(left.as_ref(),  Expr::BinOp { op: BinOpKind::Lt, .. }));
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
        assert_eq!(params, &["a", "b"]);
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
        let p = parse_src("fn f(x) {\n    if x {\n        return 1\n    } else {\n        return 0\n    }\n}");
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
    fn test_parse_nested_fn_error() {
        let err = parse_src_err("fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}");
        assert!(matches!(err, JadeError::NestedFunction { .. }));
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
        let p = parse_src("fn f(n) {\n    while n > 0 {\n        let n = n - 1\n    }\n    return n\n}");
        let Stmt::FnDef { body, .. } = &p.stmts[0] else { panic!() };
        assert!(matches!(body[0], Stmt::While { .. }));
    }

    // ── struct / extend ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_struct_def() {
        let p = parse_src("struct Point {\n    x,\n    y\n}");
        let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "Point");
        assert_eq!(fields, &["x", "y"]);
    }

    #[test]
    fn test_parse_struct_def_empty() {
        let p = parse_src("struct Empty {\n}");
        let Stmt::StructDef { name, fields, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "Empty");
        assert!(fields.is_empty());
    }

    #[test]
    fn test_parse_extend_block() {
        let p = parse_src("extend Foo {\n    fn get(self) {\n        return 1\n    }\n}");
        let Stmt::ExtendBlock { type_name, methods, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(type_name, "Foo");
        assert_eq!(methods.len(), 1);
        let Stmt::FnDef { name, params, .. } = &methods[0] else { panic!() };
        assert_eq!(name, "get");
        assert_eq!(params[0], "self");
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
}
