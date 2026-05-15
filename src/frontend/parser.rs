use super::{
    ast::{BinOpKind, CatchArm, Expr, FStrPart, InterfaceMethod, Program, StructFieldDef, Stmt, UnaryOpKind},
    error::{JadeError, Result, Span},
    lexer::{RawFStrPart, Token, TokenKind},
};

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Depth of nested `fn` definitions. Used to detect and reject nested fns.
    fn_depth: usize,
    /// Depth of nested `async fn` definitions. Allows `await` to know it is inside an async context.
    async_fn_depth: usize,
    /// When false, a bare identifier followed by `{` is NOT parsed as a struct
    /// literal. Set to false while parsing `if`/`while` conditions so that
    /// `while running { … }` does not try to read `running {…}` as a struct.
    struct_literal_allowed: bool,
    /// Set to true while parsing the argument list of a `print(…)` call.
    /// Used to detect the forbidden `?p |> Type` inside print.
    in_print_call: bool,
}

/// Public entry point. Builds a Parser and drives it to produce a Program.
pub fn parse(tokens: Vec<Token>) -> Result<Program> {
    if tokens.is_empty() {
        return Err(JadeError::UnexpectedEof {
            span: Span { line: 1, col: 1 },
        });
    }
    let mut parser = Parser { tokens, pos: 0, fn_depth: 0, async_fn_depth: 0, struct_literal_allowed: true, in_print_call: false };
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

    /// Look `offset` tokens ahead without advancing. Clamped to the last token.
    fn peek_ahead(&self, offset: usize) -> &Token {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx]
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

    /// Consume a semicolon if the next token is one; otherwise accept an implicit
    /// semicolon when the next token is `}` (end of block) or `Eof`.
    /// This lets single-line function bodies work: `fn f(x) { return x * 2 }`.
    fn consume_semicolon(&mut self) -> Result<()> {
        match self.peek().kind {
            TokenKind::Semicolon => { self.advance(); Ok(()) }
            TokenKind::RBrace | TokenKind::Eof => Ok(()),
            _ => {
                let token = self.peek().clone();
                Err(JadeError::UnexpectedToken {
                    expected: "';'".to_string(),
                    got: format!("{:?}", token.kind),
                    span: token.span,
                })
            }
        }
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

    /// Parse zero or more `@ident` decorator lines preceding a fn/struct definition.
    fn parse_decorators(&mut self) -> Result<Vec<(String, Vec<(Option<String>, Expr)>)>> {
        let mut decorators = Vec::new();
        while self.peek().kind == TokenKind::At {
            self.advance(); // consume `@`
            let name = self.expect_ident("decorator name")?;
            // Optional argument list: @dec(pos, key = val, ...)
            let args = if self.peek().kind == TokenKind::LParen {
                self.advance(); // consume `(`
                let mut args = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    loop {
                        // Look-ahead: `ident =` means keyword arg.
                        let kw = if let TokenKind::Identifier(kname) = self.peek().kind.clone() {
                            if self.peek_ahead(1).kind == TokenKind::Equals {
                                self.advance(); // consume ident
                                self.advance(); // consume `=`
                                Some(kname)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        args.push((kw, self.parse_pipe()?));
                        if self.peek().kind == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RParen)?;
                args
            } else {
                Vec::new()
            };
            decorators.push((name, args));
            // Consume the auto-semicolon inserted after the decorator line.
            if self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
        }
        Ok(decorators)
    }

    /// Parse a single statement.
    fn parse_stmt(&mut self) -> Result<Stmt> {
        let decorators = self.parse_decorators()?;
        if !decorators.is_empty() {
            // Decorators are valid on fn, async fn, struct, and extend.
            return match self.peek().kind {
                TokenKind::Fn     => self.parse_fn_with_decorators(decorators),
                TokenKind::Async  => self.parse_async_fn_with_decorators(decorators),
                TokenKind::Struct => self.parse_struct_def_with_decorators(decorators),
                TokenKind::Extend => self.parse_extend_block_with_decorators(decorators),
                _ => {
                    let t = self.peek().clone();
                    Err(JadeError::UnexpectedToken {
                        expected: "fn, async fn, struct, or extend after decorator".to_string(),
                        got: format!("{:?}", t.kind),
                        span: t.span,
                    })
                }
            };
        }
        match self.peek().kind {
            TokenKind::Let    => self.parse_let(),
            TokenKind::Fn     => self.parse_fn_with_decorators(vec![]),
            TokenKind::Async  => self.parse_async_fn_with_decorators(vec![]),
            TokenKind::Return => self.parse_return(),
            TokenKind::If     => self.parse_if(),
            TokenKind::While  => self.parse_while(),
            TokenKind::For    => self.parse_for(),
            TokenKind::Struct     => self.parse_struct_def_with_decorators(vec![]),
            TokenKind::Extend     => self.parse_extend_block_with_decorators(vec![]),
            TokenKind::Interface  => self.parse_interface_def(),
            TokenKind::Prompt     => self.parse_prompt_decl(),
            TokenKind::Use        => self.parse_use(),
            TokenKind::Raise      => self.parse_raise(),
            TokenKind::Try        => self.parse_try_catch(),
            TokenKind::Identifier(_) => {
                // Disambiguate identifier-led statement forms:
                //   `ident =`              → bare variable assignment
                //   `ident . ident =`      → struct field assignment
                //   `ident [ expr ] =`     → array index assignment
                //   anything else          → expression statement (e.g. method call)
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
                } else if self.is_index_assign() {
                    self.parse_index_assign()
                } else {
                    self.parse_expr_stmt()
                }
            }
            _ => self.parse_expr_stmt(),
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
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
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
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;

        Ok(Stmt::Let { name, value, span })
    }

    /// Parse a parenthesised parameter list `( name, name = expr, ... )`.
    /// Returns `(name, default_expr)` pairs; required params have `None`.
    fn parse_param_list(&mut self) -> Result<Vec<(String, Option<Expr>)>> {
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                let param_token = self.peek().clone();
                let name = match &param_token.kind {
                    TokenKind::Identifier(p) => { let n = p.clone(); self.advance(); n }
                    _ => return Err(JadeError::UnexpectedToken {
                        expected: "parameter name".to_string(),
                        got: format!("{:?}", param_token.kind),
                        span: param_token.span,
                    }),
                };
                let default = if self.peek().kind == TokenKind::Equals {
                    self.advance(); // consume `=`
                    Some(self.parse_pipe()?)
                } else {
                    None
                };
                params.push((name, default));
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen)?;
        Ok(params)
    }

    /// Parse `fn <ident> ( <params> ) { <body> }` with pre-collected decorators.
    fn parse_fn_with_decorators(&mut self, decorators: Vec<(String, Vec<(Option<String>, Expr)>)>) -> Result<Stmt> {
        let span = self.peek().span;
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
        let params = self.parse_param_list()?;

        // Optional `-> type` return annotation — parsed and discarded at tree-walk stage.
        if self.peek().kind == TokenKind::Arrow {
            self.advance(); // consume `->`
            self.expect_ident("return type")?; // consume type name
        }

        // Body block
        self.fn_depth += 1;
        let body = self.parse_block()?;
        self.fn_depth -= 1;

        Ok(Stmt::FnDef { name, params, body, decorators, span })
    }

    /// Parse `async fn <ident> ( <params> ) { <body> }` with pre-collected decorators.
    fn parse_async_fn_with_decorators(&mut self, decorators: Vec<(String, Vec<(Option<String>, Expr)>)>) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `async`

        // Must be followed by `fn`
        if self.peek().kind != TokenKind::Fn {
            let t = self.peek().clone();
            return Err(JadeError::UnexpectedToken {
                expected: "fn after async".to_string(),
                got: format!("{:?}", t.kind),
                span: t.span,
            });
        }

        self.advance(); // consume `fn`

        let name_token = self.peek().clone();
        let name = match &name_token.kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => return Err(JadeError::UnexpectedToken {
                expected: "function name".to_string(),
                got: format!("{:?}", name_token.kind),
                span: name_token.span,
            }),
        };

        let params = self.parse_param_list()?;

        // Optional `-> type` return annotation
        if self.peek().kind == TokenKind::Arrow {
            self.advance();
            self.expect_ident("return type")?;
        }

        self.fn_depth += 1;
        self.async_fn_depth += 1;
        let body = self.parse_block()?;
        self.async_fn_depth -= 1;
        self.fn_depth -= 1;

        Ok(Stmt::AsyncFnDef { name, params, body, decorators, span })
    }

    /// Parse `return <expr> ;` or `return ;`
    fn parse_return(&mut self) -> Result<Stmt> {
        let span = self.peek().span;

        if self.fn_depth == 0 {
            return Err(JadeError::ReturnOutsideFunction { span });
        }

        self.advance(); // consume `return`

        // If the next token ends the statement without a value, it's a bare return
        match self.peek().kind {
            TokenKind::Semicolon => { self.advance(); return Ok(Stmt::Return { value: None, span }); }
            TokenKind::RBrace | TokenKind::Eof => return Ok(Stmt::Return { value: None, span }),
            _ => {}
        }

        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::Return { value: Some(value), span })
    }

    /// Parse an expression that will be used as a control-flow condition.
    /// Struct literals are disallowed here so that `while running { … }` does
    /// not try to interpret `running {…}` as a struct literal.
    fn parse_condition(&mut self) -> Result<Expr> {
        let saved = self.struct_literal_allowed;
        self.struct_literal_allowed = false;
        let cond = self.parse_pipe()?;
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

    /// Parse `for <var> in <iterable> { <body> }`
    fn parse_for(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `for`

        let var_token = self.peek().clone();
        let var = match &var_token.kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => return Err(JadeError::UnexpectedToken {
                expected: "identifier after `for`".to_string(),
                got: format!("{:?}", var_token.kind),
                span: var_token.span,
            }),
        };

        self.expect(&TokenKind::In)?;
        let iterable = self.parse_condition()?;
        let body = self.parse_block()?;

        Ok(Stmt::For { var, iterable, body, span })
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

    /// Parse `prompt name = expr ;`
    /// Parse `use "path/to/file.jde" ;`
    fn parse_use(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `use`
        let path_token = self.peek().clone();
        let path = match &path_token.kind {
            TokenKind::Str(s) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => {
                return Err(JadeError::UnexpectedToken {
                    expected: "string path after `use`".to_string(),
                    got: format!("{:?}", path_token.kind),
                    span: path_token.span,
                });
            }
        };
        self.consume_semicolon()?;
        Ok(Stmt::Use { path, span })
    }

    fn parse_prompt_decl(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `prompt`
        let name = self.expect_ident("prompt variable name")?;
        self.expect(&TokenKind::Equals)?;
        let body = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::PromptDecl { name, body, span })
    }

    /// Parse `raise expr ;`
    fn parse_raise(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `raise`
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::Raise { value, span })
    }

    /// Parse `try { body } catch [TypeName] binding { arm } …`
    fn parse_try_catch(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `try`
        let body = self.parse_block()?;

        // Consume the auto-inserted semicolon after the closing `}` — same
        // pattern used by `if/else`.
        if self.peek().kind == TokenKind::Semicolon {
            self.advance();
        }

        let mut arms = Vec::new();
        while self.peek().kind == TokenKind::Catch {
            self.advance(); // consume `catch`

            // Disambiguate between:
            //   `catch TypeName binding { … }` — two identifiers before `{`
            //   `catch binding { … }`           — one identifier before `{`
            let second_is_ident = self.peek_at(1)
                .map(|t| matches!(t.kind, TokenKind::Identifier(_)))
                .unwrap_or(false);

            let (catch_type, binding) = if second_is_ident {
                let type_name = self.expect_ident("exception type name")?;
                let bind_name = self.expect_ident("catch binding name")?;
                (Some(type_name), bind_name)
            } else {
                let bind_name = self.expect_ident("catch binding name")?;
                (None, bind_name)
            };

            let arm_body = self.parse_block()?;

            // Consume the auto-inserted semicolon before the next `catch` or end.
            if self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }

            arms.push(CatchArm { catch_type, binding, body: arm_body });
        }

        Ok(Stmt::TryCatch { body, arms, span })
    }

    /// Parse `struct Name { field, … }`
    /// Fields may be bare identifiers (required), `let name = expr` (optional with default),
    /// or `prompt name = expr` (optional prompt field with default text).
    fn parse_struct_def_with_decorators(&mut self, decorators: Vec<(String, Vec<(Option<String>, Expr)>)>) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `struct`
        let name = self.expect_ident("struct name")?;
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        loop {
            // Skip auto-semicolons between field declarations (from trailing newlines)
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            match self.peek().kind.clone() {
                TokenKind::Identifier(_) => {
                    let field_name = self.expect_ident("field name")?;
                    fields.push(StructFieldDef::Required(field_name));
                }
                TokenKind::Let => {
                    self.advance(); // consume `let`
                    let field_name = self.expect_ident("field name after `let`")?;
                    self.expect(&TokenKind::Equals)?;
                    let default = self.parse_pipe()?;
                    fields.push(StructFieldDef::Let { name: field_name, default });
                }
                TokenKind::Prompt => {
                    self.advance(); // consume `prompt`
                    let field_name = self.expect_ident("field name after `prompt`")?;
                    self.expect(&TokenKind::Equals)?;
                    let default = self.parse_pipe()?;
                    fields.push(StructFieldDef::Prompt { name: field_name, default });
                }
                _ => {
                    let t = self.peek().clone();
                    return Err(JadeError::UnexpectedToken {
                        expected: "field name, `let`, or `prompt`".to_string(),
                        got: format!("{:?}", t.kind),
                        span: t.span,
                    });
                }
            }
            // Allow a trailing comma or semicolon after each field declaration
            if self.peek().kind == TokenKind::Comma || self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Stmt::StructDef { name, fields, decorators, span })
    }

    /// Parse `extend TypeName { fn method(self, …) { … } … }`
    /// or    `extend TypeName: InterfaceName { fn method(self, …) { … } … }`
    fn parse_extend_block_with_decorators(&mut self, decorators: Vec<(String, Vec<(Option<String>, Expr)>)>) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `extend`
        let type_name = self.expect_ident("type name")?;
        // Optional `: InterfaceName`
        let interface_name = if self.peek().kind == TokenKind::Colon {
            self.advance(); // consume `:`
            Some(self.expect_ident("interface name")?)
        } else {
            None
        };
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
                TokenKind::Fn => methods.push(self.parse_fn_with_decorators(vec![])?),
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
        Ok(Stmt::ExtendBlock { type_name, interface_name, methods, decorators, span })
    }

    /// Parse `interface Name { fn method(self, …) -> type }`
    /// Method bodies are absent — these are signatures only.
    fn parse_interface_def(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `interface`
        let name = self.expect_ident("interface name")?;
        self.expect(&TokenKind::LBrace)?;
        let mut methods = Vec::new();
        loop {
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            // Expect `fn`
            let method_span = self.peek().span;
            match self.peek().kind.clone() {
                TokenKind::Fn => { self.advance(); } // consume `fn`
                _ => {
                    let t = self.peek().clone();
                    return Err(JadeError::UnexpectedToken {
                        expected: "fn".to_string(),
                        got: format!("{:?}", t.kind),
                        span: t.span,
                    });
                }
            }
            let method_name = self.expect_ident("method name")?;
            self.expect(&TokenKind::LParen)?;
            let mut params = Vec::new();
            while self.peek().kind != TokenKind::RParen && self.peek().kind != TokenKind::Eof {
                params.push(self.expect_ident("parameter name")?);
                if self.peek().kind == TokenKind::Comma {
                    self.advance();
                }
            }
            self.expect(&TokenKind::RParen)?;
            // Optional `-> type`
            let return_type = if self.peek().kind == TokenKind::Arrow {
                self.advance(); // consume `->`
                Some(self.expect_ident("return type")?)
            } else {
                None
            };
            methods.push(InterfaceMethod { name: method_name, params, return_type, span: method_span });
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Stmt::InterfaceDef { name, methods, span })
    }

    /// Parse `object.field = expr ;`
    fn parse_field_assign(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        let object = self.expect_ident("object name")?;
        self.expect(&TokenKind::Dot)?;
        let field = self.expect_ident("field name")?;
        self.expect(&TokenKind::Equals)?;
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::FieldAssign { object, field, value, span })
    }

    /// Returns true when the current position looks like `ident [ … ] =`.
    /// Scans forward to find the matching `]`, then checks that the next token is `=`.
    fn is_index_assign(&self) -> bool {
        if !matches!(self.peek_at(1).map(|t| &t.kind), Some(TokenKind::LBracket)) {
            return false;
        }
        let mut depth = 1usize;
        let mut i = self.pos + 2;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::LBracket => depth += 1,
                TokenKind::RBracket => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(i + 1).map(|t| &t.kind),
                            Some(TokenKind::Equals)
                        );
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Parse `<ident> [ <expr> ] = <expr> ;`
    fn parse_index_assign(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        let name = self.expect_ident("array name")?;
        self.advance(); // consume `[`
        let index = self.parse_pipe()?;
        // Skip any auto-inserted semicolons before `]` (multiline index expr)
        while self.peek().kind == TokenKind::Semicolon {
            self.advance();
        }
        self.expect(&TokenKind::RBracket)?;
        self.expect(&TokenKind::Equals)?;
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::IndexAssign { name, index, value, span })
    }

    /// Parse an array literal starting after `[` has been consumed.
    /// Handles trailing commas and auto-inserted semicolons (multiline arrays).
    fn parse_array_literal(&mut self, span: Span) -> Result<Expr> {
        let mut elements = Vec::new();
        loop {
            // Skip auto-inserted semicolons (e.g. after an element on its own line)
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBracket || self.peek().kind == TokenKind::Eof {
                break;
            }
            elements.push(self.parse_pipe()?);
            // Skip auto-inserted semicolons between element and `,` or `]`
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::Comma {
                self.advance(); // consume `,`, then loop (handles trailing comma too)
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBracket)?;
        Ok(Expr::Array { elements, span })
    }

    /// Parse an expression used as a statement (value discarded), e.g. `obj.method(args)`.
    fn parse_expr_stmt(&mut self) -> Result<Stmt> {
        let expr = self.parse_pipe()?;
        self.consume_semicolon()?;
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
            Expr::Array        { span, .. } => *span,
            Expr::FStr         { span, .. } => *span,
            Expr::PromptLiteral{ span, .. } => *span,
            Expr::PromptDeref  { span, .. } => *span,
            Expr::Dict         { span, .. } => *span,
            Expr::Closure      { span, .. } => *span,
            Expr::Await        { span, .. } => *span,
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
            async_fn_depth: 0,
            struct_literal_allowed: true,
            in_print_call: false,
        };
        sub.parse_pipe()
    }

    /// Lowest precedence: `|>` (pipe).
    /// `val |> f`       → `f(val)`
    /// `val |> f(a, b)` → `f(val, a, b)` (lhs inserted as first argument)
    /// Left-associative: `a |> f |> g` = `g(f(a))`.
    fn parse_pipe(&mut self) -> Result<Expr> {
        let mut left = self.parse_or()?;
        loop {
            if self.peek().kind != TokenKind::PipeGt {
                break;
            }
            let span = Self::expr_span(&left);
            self.advance(); // consume `|>`
            let right = self.parse_or()?;
            let rhs_span = Self::expr_span(&right);
            left = match right {
                Expr::Identifier { name, span: id_span } => Expr::Call {
                    callee: Box::new(Expr::Identifier { name, span: id_span }),
                    args: vec![left],
                    span,
                },
                Expr::Call { callee, mut args, span: call_span } => {
                    args.insert(0, left);
                    Expr::Call { callee, args, span: call_span }
                }
                _ => return Err(JadeError::UnexpectedToken {
                    expected: "function or call on right side of |>".to_string(),
                    got: "expression".to_string(),
                    span: rhs_span,
                }),
            };
        }
        Ok(left)
    }

    /// `||` (logical OR).
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

    /// Unary `~` (bitwise NOT), `!` (logical NOT), `-` (negation), `await`.
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
            TokenKind::Await => {
                let span = self.peek().span;
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::Await { expr: Box::new(expr), span })
            }
            _ => self.parse_call(),
        }
    }

    /// Parse a dict literal starting after `{` has been consumed.
    /// Keys are full expressions that must evaluate to strings at runtime.
    /// Handles trailing commas and auto-inserted semicolons (multiline dicts).
    fn parse_dict_literal(&mut self, span: Span) -> Result<Expr> {
        let mut entries = Vec::new();
        loop {
            // Skip auto-inserted semicolons
            while self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
            if self.peek().kind == TokenKind::RBrace || self.peek().kind == TokenKind::Eof {
                break;
            }
            // Disable struct literals when parsing the key to prevent `TypeName {` ambiguity
            let was_allowed = self.struct_literal_allowed;
            self.struct_literal_allowed = false;
            let key = self.parse_pipe()?;
            self.struct_literal_allowed = was_allowed;
            self.expect(&TokenKind::Colon)?;
            let value = self.parse_pipe()?;
            entries.push((key, value));
            if self.peek().kind == TokenKind::Comma || self.peek().kind == TokenKind::Semicolon {
                self.advance();
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::Dict { entries, span })
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
            let value = self.parse_pipe()?;
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

                // Track whether we're inside `print(…)` to detect the forbidden
                // `?p |> Type` streaming pattern.
                let was_in_print_call = self.in_print_call;
                if let Expr::Identifier { ref name, .. } = expr {
                    if name == "print" {
                        self.in_print_call = true;
                    }
                }

                self.advance(); // consume `(`
                let mut args = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    args.push(self.parse_pipe()?);
                    while self.peek().kind == TokenKind::Comma {
                        self.advance(); // consume `,`
                        args.push(self.parse_pipe()?);
                    }
                }
                self.expect(&TokenKind::RParen)?;
                self.in_print_call = was_in_print_call;
                expr = Expr::Call { callee: Box::new(expr), args, span };
            } else if self.peek().kind == TokenKind::Dot {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `.`
                let field = self.expect_ident("field name")?;
                expr = Expr::FieldAccess { object: Box::new(expr), field, span };
            } else if self.peek().kind == TokenKind::LBracket {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `[`
                let index = self.parse_pipe()?;
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
                let expr = self.parse_pipe()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            TokenKind::LBracket => {
                let span = token.span;
                self.advance(); // consume `[`
                self.parse_array_literal(span)
            }
            TokenKind::LBrace => {
                let span = token.span;
                self.advance(); // consume `{`
                self.parse_dict_literal(span)
            }
            TokenKind::Question => {
                let span = token.span;
                self.advance(); // consume `?`
                // Parse the target expression: supports ?name, ?obj.field, ?arr[i], etc.
                let expr = self.parse_call()?;
                // Check for optional `|> TypeName` typed dereference suffix.
                let output_type = if self.peek().kind == TokenKind::PipeGt {
                    if self.in_print_call {
                        return Err(JadeError::StreamingWithType { span });
                    }
                    self.advance(); // consume `|>`
                    let type_name = self.expect_ident("type name after |>")?;
                    Some(type_name)
                } else {
                    None
                };
                Ok(Expr::PromptDeref { expr: Box::new(expr), output_type, span })
            }
            // ── Closures: `|x, y| expr` or `|x, y| { body }` ────────────────
            TokenKind::Pipe => {
                let span = token.span;
                self.advance(); // consume first `|`
                let mut params = Vec::new();
                // Parse parameters until the closing `|`
                while self.peek().kind != TokenKind::Pipe {
                    if self.peek().kind == TokenKind::Eof {
                        return Err(JadeError::UnexpectedEof { span: self.peek().span });
                    }
                    let p = self.expect_ident("closure parameter")?;
                    params.push(p);
                    if self.peek().kind == TokenKind::Comma {
                        self.advance();
                    }
                }
                self.expect(&TokenKind::Pipe)?; // consume closing `|`
                let body = self.parse_closure_body(span)?;
                Ok(Expr::Closure { params, body, span })
            }
            // ── Empty-param closure: `|| expr` or `|| { body }` ─────────────
            TokenKind::PipePipe => {
                let span = token.span;
                self.advance(); // consume `||`
                let body = self.parse_closure_body(span)?;
                Ok(Expr::Closure { params: Vec::new(), body, span })
            }
            // ── Prompt literal as expression: `prompt <expr>` ────────────────
            // Allows `let p = prompt "text"` inside function bodies, in addition
            // to the top-level `prompt p = "text"` declaration form.
            // Body uses parse_or (not parse_pipe) so that `?prompt "..." |> Type`
            // leaves the `|>` for the enclosing `?` typed-deref handler to consume.
            TokenKind::Prompt => {
                let span = token.span;
                self.advance(); // consume `prompt`
                let body = self.parse_or()?;
                Ok(Expr::PromptLiteral { body: Box::new(body), span })
            }
            TokenKind::Eof => Err(JadeError::UnexpectedEof { span: token.span }),
            _ => Err(JadeError::UnexpectedToken {
                expected: "expression".to_string(),
                got: format!("{:?}", token.kind),
                span: token.span,
            }),
        }
    }

    /// Parse the body of a closure: `{ stmts }` or a single expression (implicit return).
    fn parse_closure_body(&mut self, span: Span) -> Result<Vec<Stmt>> {
        self.fn_depth += 1;
        let body = if self.peek().kind == TokenKind::LBrace {
            self.parse_block()?
        } else {
            // Single expression: wrap as implicit return so eval_block returns it.
            let expr = self.parse_pipe()?;
            vec![Stmt::Return { value: Some(expr), span }]
        };
        self.fn_depth -= 1;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{
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
    fn test_parse_nested_fn_ok() {
        // Nested function definitions are now allowed.
        let prog = parse_src("fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}");
        assert_eq!(prog.stmts.len(), 1);
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
        let p = parse_src("struct Mixed {\n    x,\n    let label = \"origin\",\n    prompt sys = \"helpful\"\n}");
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
        assert!(matches!(&fields[0], StructFieldDef::Let { name, default: Expr::StructLiteral { .. }, .. } if name == "inner"));
    }

    #[test]
    fn test_parse_extend_block() {
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

    #[test]
    fn test_parse_pipe_invalid_rhs_is_error() {
        // `|>` requires a function name or call on the right, not a raw expression
        let tokens = lexer::tokenize("5 |> (1 + 2)").unwrap();
        assert!(parse(tokens).is_err());
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

    #[test]
    fn test_parse_interface_def() {
        let p = parse_src("interface Displayable {\n    fn to_str(self) -> str\n}");
        let Stmt::InterfaceDef { name, methods, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "Displayable");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "to_str");
        assert_eq!(methods[0].params, vec!["self"]);
        assert_eq!(methods[0].return_type.as_deref(), Some("str"));
    }

    #[test]
    fn test_parse_interface_def_no_return_type() {
        let p = parse_src("interface Runnable {\n    fn run(self)\n}");
        let Stmt::InterfaceDef { name, methods, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "Runnable");
        assert_eq!(methods[0].name, "run");
        assert!(methods[0].return_type.is_none());
    }

    #[test]
    fn test_parse_extend_with_interface() {
        let p = parse_src("extend Foo: Bar {\n    fn go(self) {\n        return 1\n    }\n}");
        let Stmt::ExtendBlock { type_name, interface_name, methods, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(type_name, "Foo");
        assert_eq!(interface_name.as_deref(), Some("Bar"));
        assert_eq!(methods.len(), 1);
    }

    #[test]
    fn test_parse_extend_without_interface() {
        let p = parse_src("extend Foo {\n    fn go(self) {\n        return 1\n    }\n}");
        let Stmt::ExtendBlock { type_name, interface_name, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(type_name, "Foo");
        assert!(interface_name.is_none());
    }

    #[test]
    fn test_parse_fn_with_return_type() {
        let p = parse_src("fn greet(name) -> str {\n    return \"hi\"\n}");
        let Stmt::FnDef { name, params, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(name, "greet");
        assert_eq!(params[0].0, "name");
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
        let Stmt::Let { value: Expr::PromptDeref { expr, output_type, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with PromptDeref") };
        assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
        assert!(output_type.is_none());
    }

    #[test]
    fn test_parse_prompt_deref_typed_int() {
        let p = parse_src("let x = ?p |> int");
        let Stmt::Let { value: Expr::PromptDeref { expr, output_type, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with PromptDeref") };
        assert!(matches!(expr.as_ref(), Expr::Identifier { name, .. } if name == "p"));
        assert_eq!(output_type.as_deref(), Some("int"));
    }

    #[test]
    fn test_parse_prompt_deref_field_access() {
        let p = parse_src("let x = ?obj.system");
        let Stmt::Let { value: Expr::PromptDeref { expr, output_type, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with PromptDeref") };
        assert!(matches!(expr.as_ref(), Expr::FieldAccess { field, .. } if field == "system"));
        assert!(output_type.is_none());
    }

    #[test]
    fn test_parse_prompt_deref_field_access_typed() {
        let p = parse_src("let x = ?obj.field |> int");
        let Stmt::Let { value: Expr::PromptDeref { expr, output_type, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with PromptDeref") };
        assert!(matches!(expr.as_ref(), Expr::FieldAccess { field, .. } if field == "field"));
        assert_eq!(output_type.as_deref(), Some("int"));
    }

    #[test]
    fn test_parse_streaming_prohibition() {
        let tokens = super::super::lexer::tokenize("print(?p |> int)").expect("lex");
        let err = parse(tokens).unwrap_err();
        assert!(matches!(err, super::super::error::JadeError::StreamingWithType { .. }));
    }

    // ── dict literal tests ────────────────────────────────────────────────────

    #[test]
    fn test_parse_dict_empty() {
        let p = parse_src("let d = {}");
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with Dict") };
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_dict_single_entry() {
        let p = parse_src(r#"let d = {"key": 1}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with Dict") };
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0].0, Expr::Str { value, .. } if value == "key"));
        assert!(matches!(&entries[0].1, Expr::Integer { value: 1, .. }));
    }

    #[test]
    fn test_parse_dict_multiple_entries() {
        let p = parse_src(r#"let d = {"a": 1, "b": 2}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with Dict") };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_parse_dict_trailing_comma() {
        let p = parse_src(r#"let d = {"a": 1,}"#);
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with Dict") };
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_dict_identifier_key() {
        // Variable key: the key expression is an identifier
        let p = parse_src("let d = {k: 1}");
        let Stmt::Let { value: Expr::Dict { entries, .. }, .. } = &p.stmts[0]
            else { panic!("expected Let with Dict") };
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

    #[test]
    fn test_parse_struct_decorator_with_arg() {
        let p = parse_src("@log(\"City\")\nstruct City {\n  name,\n}");
        let Stmt::StructDef { decorators, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(decorators.len(), 1);
        assert_eq!(decorators[0].0, "log");
        let (kw, expr) = &decorators[0].1[0];
        assert!(kw.is_none());
        assert!(matches!(expr, Expr::Str { value, .. } if value == "City"));
    }

    #[test]
    fn test_parse_extend_decorator_route_positional() {
        let p = parse_src("@route(\"action\")\nextend Cmd {}");
        let Stmt::ExtendBlock { decorators, type_name, .. } = &p.stmts[0] else { panic!() };
        assert_eq!(type_name, "Cmd");
        assert_eq!(decorators[0].0, "route");
        let (kw, expr) = &decorators[0].1[0];
        assert!(kw.is_none());
        assert!(matches!(expr, Expr::Str { value, .. } if value == "action"));
    }

    #[test]
    fn test_parse_extend_decorator_route_kwarg() {
        let p = parse_src("@route(on = \"field\")\nextend T {}");
        let Stmt::ExtendBlock { decorators, .. } = &p.stmts[0] else { panic!() };
        let (kw, expr) = &decorators[0].1[0];
        assert_eq!(kw.as_deref(), Some("on"));
        assert!(matches!(expr, Expr::Str { value, .. } if value == "field"));
    }
}
