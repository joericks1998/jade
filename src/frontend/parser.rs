use super::{
    ast::{
        BinOpKind, CatchArm, DerefStyle, Expr, FStrPart, Program, Stmt, StructFieldDef, UnaryOpKind,
    },
    error::{JadeError, Result, Span},
    lexer::{RawFStrPart, Token, TokenKind, token_kind_desc},
};

/// The `@dec(a, k = v)` lines attached to one declaration, in source order.
///
/// Each entry is `(name, args)`, and each argument is `(keyword, expr)` with the
/// keyword absent for a positional one. A namespaced `@a::b` arrives here as the
/// single name `"a.b"`; the dot is what every consumer splits on.
type Decorators = Vec<(String, Vec<(Option<String>, Expr)>)>;

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Depth of nested `fn` definitions. Used to detect and reject nested fns.
    fn_depth: usize,
    /// How many loops enclose the statement being parsed, so `break` and
    /// `continue` can be refused where there is nothing to leave. Reset — not
    /// merely saved — across a function boundary: a loop outside a `fn` is not
    /// a loop the body can break out of.
    loop_depth: usize,
    /// Depth of nested `async fn` definitions. Allows `await` to know it is inside an async context.
    async_fn_depth: usize,
    /// When false, a bare identifier followed by `{` is NOT parsed as a struct
    /// literal. Set to false while parsing `if`/`while` conditions so that
    /// `while running { … }` does not try to read `running {…}` as a struct.
    struct_literal_allowed: bool,
}

/// Public entry point. Builds a Parser and drives it to produce a Program.
pub fn parse(tokens: Vec<Token>) -> Result<Program> {
    if tokens.is_empty() {
        return Err(JadeError::UnexpectedEof { span: Span { line: 1, col: 1 } });
    }
    let mut parser = Parser {
        tokens,
        pos: 0,
        fn_depth: 0,
        loop_depth: 0,
        async_fn_depth: 0,
        struct_literal_allowed: true,
    };
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
                got: token_kind_desc(&token.kind),
                span: token.span,
            }),
        }
    }

    /// Returns a reference to the current token without advancing.
    // Safety: `parse()` rejects empty token streams. `advance()` is clamped at
    // the Eof sentinel, so `self.pos` is always a valid index. The fallback to
    // the last token is an extra safety net — it returns Eof rather than panicking.
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or_else(|| &self.tokens[self.tokens.len() - 1])
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
            TokenKind::Semicolon => {
                self.advance();
                Ok(())
            }
            TokenKind::RBrace | TokenKind::Eof => Ok(()),
            _ => {
                let token = self.peek().clone();
                Err(JadeError::UnexpectedToken {
                    expected: "end of statement".to_string(),
                    got: token_kind_desc(&token.kind),
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
                expected: token_kind_desc(kind),
                got: token_kind_desc(&token.kind),
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
    fn parse_decorators(&mut self) -> Result<Decorators> {
        let mut decorators = Vec::new();
        while self.peek().kind == TokenKind::At {
            self.advance(); // consume `@`
            let mut name = self.expect_ident("decorator name")?;
            // Support namespaced decorator names: @tools::register → "tools.register".
            // The `::` separator is normalized to a dot internally so downstream
            // resolution (emit/vm) can keep splitting on `.`.
            while self.peek().kind == TokenKind::ColonColon {
                self.advance();
                let part = self.expect_ident("decorator field")?;
                name = format!("{}.{}", name, part);
            }
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

    /// Wrap a declaration's value in its decorators: `@a @b let x = v` parses as
    /// `let x = b(a(v))`.
    ///
    /// This happens in the parser, so nothing downstream learns that a
    /// declaration can carry a decorator — `type_infer`, `emit`, the VM and the
    /// AOT backend all see an ordinary call. A `fn` decorator cannot work this
    /// way, because the value it wraps is a function the emitter has yet to
    /// build; that path lives in `emit.rs` and is the reason the two look
    /// different despite meaning the same thing.
    ///
    /// Source order is innermost-first, matching `fn`: the decorator written
    /// first is applied first. That is the reverse of Python's rule, and
    /// matching `fn` matters more than matching Python — two decorators on a
    /// `let` and two on a `fn` in the same file have to nest the same way.
    fn apply_decorators(value: Expr, decorators: Decorators, span: Span) -> Expr {
        let mut acc = value;
        for (name, dec_args) in decorators {
            // `@tools::register` arrived here normalized to "tools.register".
            // Rebuild it as a field access so it resolves exactly as a
            // hand-written `tools.register(v)` would.
            let mut parts = name.split('.');
            let mut callee =
                Expr::Identifier { name: parts.next().unwrap_or_default().to_string(), span };
            for part in parts {
                callee =
                    Expr::FieldAccess { object: Box::new(callee), field: part.to_string(), span };
            }
            // The decorated value is the first argument; the decorator's own
            // arguments follow, keeping positional and keyword forms apart.
            let mut args = vec![acc];
            let mut kwargs = Vec::new();
            for (kw, arg) in dec_args {
                match kw {
                    Some(k) => kwargs.push((k, arg)),
                    None => args.push(arg),
                }
            }
            acc = Expr::Call { callee: Box::new(callee), args, kwargs, span };
        }
        acc
    }

    /// Parse a single statement.
    fn parse_stmt(&mut self) -> Result<Stmt> {
        let decorators = self.parse_decorators()?;
        if !decorators.is_empty() {
            // Decorators are valid on fn, async fn, struct, extend, let, and prompt.
            return match self.peek().kind {
                TokenKind::Fn => self.parse_fn_with_decorators(decorators),
                TokenKind::Async => self.parse_async_fn_with_decorators(decorators),
                TokenKind::Struct => self.parse_struct_def_with_decorators(decorators),
                TokenKind::Extend => self.parse_extend_block_with_decorators(decorators),
                TokenKind::Let => self.parse_let_with_decorators(decorators),
                TokenKind::Prompt => self.parse_prompt_decl_with_decorators(decorators),
                _ => {
                    let t = self.peek().clone();
                    Err(JadeError::UnexpectedToken {
                        expected: "`fn`, `async fn`, `struct`, `extend`, `let`, or `prompt` after decorator".to_string(),
                        got: token_kind_desc(&t.kind),
                        span: t.span,
                    })
                }
            };
        }
        match self.peek().kind {
            TokenKind::Let => self.parse_let_with_decorators(vec![]),
            TokenKind::Fn => self.parse_fn_with_decorators(vec![]),
            TokenKind::Async => self.parse_async_fn_with_decorators(vec![]),
            TokenKind::Return => self.parse_return(),
            TokenKind::Yield => self.parse_yield(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Break => self.parse_break(),
            TokenKind::Continue => self.parse_continue(),
            TokenKind::Struct => self.parse_struct_def_with_decorators(vec![]),
            TokenKind::Extend => self.parse_extend_block_with_decorators(vec![]),
            TokenKind::Interface => Err(JadeError::InterfaceRemoved { span: self.peek().span }),
            TokenKind::Prompt => self.parse_prompt_decl_with_decorators(vec![]),
            TokenKind::Use => self.parse_use(),
            TokenKind::From => self.parse_from_use(),
            TokenKind::Raise => self.parse_raise(),
            TokenKind::Try => self.parse_try_catch(),
            TokenKind::Identifier(_) => {
                // Disambiguate identifier-led statement forms:
                //   `ident =`              → bare variable assignment
                //   `ident . ident =`      → struct field assignment
                //   `ident [ expr ] =`     → array index assignment
                //   anything else          → expression statement (e.g. method call)
                let next_is_eq =
                    self.peek_at(1).map(|t| t.kind == TokenKind::Equals).unwrap_or(false);
                let next_is_dot =
                    self.peek_at(1).map(|t| t.kind == TokenKind::Dot).unwrap_or(false);
                let dot_field_eq = next_is_dot
                    && self
                        .peek_at(2)
                        .map(|t| matches!(t.kind, TokenKind::Identifier(_)))
                        .unwrap_or(false)
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
            // Implicit self field assignment: `.field = expr` → `self.field = expr`
            TokenKind::Dot => {
                let is_implicit_field_assign = self
                    .peek_at(1)
                    .map(|t| matches!(t.kind, TokenKind::Identifier(_)))
                    .unwrap_or(false)
                    && self.peek_at(2).map(|t| t.kind == TokenKind::Equals).unwrap_or(false);
                if is_implicit_field_assign {
                    let span = self.peek().span;
                    self.advance(); // consume `.`
                    let field = self.expect_ident("field name")?;
                    self.expect(&TokenKind::Equals)?;
                    let value = self.parse_pipe()?;
                    self.consume_semicolon()?;
                    Ok(Stmt::FieldAssign { object: "self".to_string(), field, value, span })
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
    fn parse_let_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
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
                    expected: "variable name".to_string(),
                    got: token_kind_desc(&name_token.kind),
                    span: name_token.span,
                });
            }
        };

        self.expect(&TokenKind::Equals)?;
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;

        let value = Self::apply_decorators(value, decorators, span);
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
                    TokenKind::Identifier(p) => {
                        let n = p.clone();
                        self.advance();
                        n
                    }
                    _ => {
                        return Err(JadeError::UnexpectedToken {
                            expected: "parameter name".to_string(),
                            got: token_kind_desc(&param_token.kind),
                            span: param_token.span,
                        });
                    }
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
    fn parse_fn_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
        if self.fn_depth > 0 {
            let span = self.peek().span;
            return Err(JadeError::NestedFunction { span });
        }
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
                    got: token_kind_desc(&name_token.kind),
                    span: name_token.span,
                });
            }
        };

        // Parameter list
        let params = self.parse_param_list()?;

        // Body block
        self.fn_depth += 1;
        let outer_loops = std::mem::take(&mut self.loop_depth);
        let body = self.parse_block()?;
        self.loop_depth = outer_loops;
        self.fn_depth -= 1;

        Ok(Stmt::FnDef { name, params, body, decorators, span })
    }

    /// Parse `async fn <ident> ( <params> ) { <body> }` with pre-collected decorators.
    fn parse_async_fn_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
        // Same rule as `fn`, which had this guard and `async fn` did not — an
        // omission rather than a decision, since the body below increments
        // `fn_depth` exactly as the plain form does. A nested `async fn` used to
        // parse and run, and then hand the user two surprises: it cannot see the
        // enclosing function's parameters (a closure captures top-level globals
        // only), so it failed at *run* time with `undefined variable`; and a
        // decorator on it was dropped without a word, because decorators are
        // applied at emit time only to a global. One rule for both forms turns
        // both into a compile error naming the problem.
        if self.fn_depth > 0 {
            let span = self.peek().span;
            return Err(JadeError::NestedFunction { span });
        }
        let span = self.peek().span;
        self.advance(); // consume `async`

        // Must be followed by `fn`
        if self.peek().kind != TokenKind::Fn {
            let t = self.peek().clone();
            return Err(JadeError::UnexpectedToken {
                expected: "`fn` after `async`".to_string(),
                got: token_kind_desc(&t.kind),
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
            _ => {
                return Err(JadeError::UnexpectedToken {
                    expected: "function name".to_string(),
                    got: token_kind_desc(&name_token.kind),
                    span: name_token.span,
                });
            }
        };

        let params = self.parse_param_list()?;

        self.fn_depth += 1;
        self.async_fn_depth += 1;
        let outer_loops = std::mem::take(&mut self.loop_depth);
        let body = self.parse_block()?;
        self.loop_depth = outer_loops;
        self.async_fn_depth -= 1;
        self.fn_depth -= 1;

        Ok(Stmt::AsyncFnDef { name, params, body, decorators, span })
    }

    /// Parse `return <expr> ;` or `return ;`
    /// `yield expr`. Rejected at the top level for the same reason `return` is:
    /// there is no function whose stream the value would join.
    fn parse_yield(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        if self.fn_depth == 0 {
            return Err(JadeError::YieldOutsideFunction { span });
        }
        self.advance(); // consume `yield`
        let value = self.parse_pipe()?;
        self.consume_semicolon()?;
        Ok(Stmt::Yield { value, span })
    }

    fn parse_return(&mut self) -> Result<Stmt> {
        let span = self.peek().span;

        if self.fn_depth == 0 {
            return Err(JadeError::ReturnOutsideFunction { span });
        }

        self.advance(); // consume `return`

        // If the next token ends the statement without a value, it's a bare return
        match self.peek().kind {
            TokenKind::Semicolon => {
                self.advance();
                return Ok(Stmt::Return { value: None, span });
            }
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
    /// `break` — leave the innermost loop.
    fn parse_break(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        if self.loop_depth == 0 {
            return Err(JadeError::BreakOutsideLoop { span });
        }
        self.advance();
        self.consume_semicolon()?;
        Ok(Stmt::Break { span })
    }

    /// `continue` — go on to the innermost loop's next iteration.
    fn parse_continue(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        if self.loop_depth == 0 {
            return Err(JadeError::ContinueOutsideLoop { span });
        }
        self.advance();
        self.consume_semicolon()?;
        Ok(Stmt::Continue { span })
    }

    fn parse_while(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `while`

        let condition = self.parse_condition()?;
        self.loop_depth += 1;
        let body = self.parse_block()?;
        self.loop_depth -= 1;

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
            _ => {
                return Err(JadeError::UnexpectedToken {
                    expected: "loop variable name after `for`".to_string(),
                    got: token_kind_desc(&var_token.kind),
                    span: var_token.span,
                });
            }
        };

        self.expect(&TokenKind::In)?;
        let iterable = self.parse_condition()?;
        self.loop_depth += 1;
        let body = self.parse_block()?;
        self.loop_depth -= 1;

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

    /// Parse `use "path/to/file.jde" [as name] ;`
    fn parse_use(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `use`
        let path_is_string = matches!(self.peek().kind, TokenKind::Str(_));
        let path = self.parse_import_path()?;
        // Optional `as alias` — only meaningful for .jde file imports; ignored for stdlib.
        let as_name = if self.peek().kind == TokenKind::As {
            self.advance(); // consume `as`
            Some(self.expect_ident("alias name after `as`")?)
        } else {
            None
        };
        self.consume_semicolon()?;
        Ok(Stmt::Use { path, as_name, path_is_string, span })
    }

    fn parse_from_use(&mut self) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `from`
        let path_is_string = matches!(self.peek().kind, TokenKind::Str(_));
        let path = self.parse_import_path()?;
        // expect `use`
        let use_tok = self.peek().clone();
        if use_tok.kind != TokenKind::Use {
            return Err(JadeError::UnexpectedToken {
                expected: "`use` after import path in `from … use …`".to_string(),
                got: token_kind_desc(&use_tok.kind),
                span: use_tok.span,
            });
        }
        self.advance(); // consume `use`
        // parse comma-separated name list
        let mut names = Vec::new();
        loop {
            let name = self.expect_ident("imported name")?;
            names.push(name);
            if self.peek().kind == TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.consume_semicolon()?;
        Ok(Stmt::FromUse { path, names, path_is_string, span })
    }

    /// Parse an import path: either a string literal `"std/time"` or `::` notation
    /// `std::time` / `llm`. The `::` separator converts to `/` so `std::time` → `"std/time"`.
    fn parse_import_path(&mut self) -> Result<String> {
        let tok = self.peek().clone();
        match &tok.kind {
            TokenKind::Str(s) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            TokenKind::Identifier(first) => {
                let mut parts = vec![first.clone()];
                self.advance();
                while self.peek().kind == TokenKind::ColonColon {
                    // peek ahead: only consume the `::` if the next token is an identifier
                    let next = self
                        .peek_at(1)
                        .map(|t| matches!(t.kind, TokenKind::Identifier(_)))
                        .unwrap_or(false);
                    if !next {
                        break;
                    }
                    self.advance(); // consume `::`
                    let ident_tok = self.peek().clone();
                    if let TokenKind::Identifier(part) = &ident_tok.kind {
                        parts.push(part.clone());
                        self.advance();
                    } else {
                        break;
                    }
                }
                Ok(parts.join("/"))
            }
            _ => Err(JadeError::UnexpectedToken {
                expected: "module path (string or `::` notation) after `use`".to_string(),
                got: token_kind_desc(&tok.kind),
                span: tok.span,
            }),
        }
    }

    /// Parse `prompt name = expr`, optionally decorated.
    ///
    /// A decorator here wraps the *text*, not the prompt: `@tagged prompt p =
    /// "x"` is `prompt p = tagged("x")`. That is the useful direction — it is
    /// how a file gives every prompt the framing a model expects without
    /// burying the content it is framing — and it means `?p` still means one
    /// thing, since the wrapping already happened when the value was built.
    fn parse_prompt_decl_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `prompt`
        let name = self.expect_ident("prompt variable name")?;
        self.expect(&TokenKind::Equals)?;
        let body = self.parse_pipe()?;
        self.consume_semicolon()?;
        let body = Self::apply_decorators(body, decorators, span);
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
            let second_is_ident = self
                .peek_at(1)
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
    fn parse_struct_def_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `struct`
        let name = self.expect_ident("struct name")?;
        // A decorator on a struct ran under `jade run` and was skipped under
        // `jade build`, so the two engines disagreed about what a literal
        // produced. Refused by name rather than dropped in silence.
        if !decorators.is_empty() {
            return Err(JadeError::StructDecoratorRemoved { span });
        }
        let parents = self.parse_parent_list()?;
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
                        got: token_kind_desc(&t.kind),
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
        Ok(Stmt::StructDef { name, fields, parents, span })
    }

    /// Parse an optional `(Parent, Other)` after a struct's name.
    ///
    /// Unambiguous with one token of lookahead: the name has already been taken,
    /// so what follows is either `(` or the body's `{`. A parent may be dotted
    /// (`shapes.Animal`), which is how an imported struct is spelled everywhere
    /// else in the language.
    ///
    /// No semicolon-skipping here, unlike the field loop below. The lexer only
    /// inserts one where `bracket_depth` is 0, and `(` raises that depth, so a
    /// parent list split across lines never sees one.
    fn parse_parent_list(&mut self) -> Result<Vec<String>> {
        if self.peek().kind != TokenKind::LParen {
            return Ok(Vec::new());
        }
        self.advance(); // consume `(`
        let mut parents = Vec::new();
        while self.peek().kind != TokenKind::RParen {
            let mut name = self.expect_ident("parent struct name")?;
            while self.peek().kind == TokenKind::Dot {
                self.advance();
                let seg = self.expect_ident("parent struct name after `.`")?;
                name = format!("{name}.{seg}");
            }
            parents.push(name);
            if self.peek().kind != TokenKind::Comma {
                break;
            }
            self.advance();
        }
        self.expect(&TokenKind::RParen)?;
        Ok(parents)
    }

    /// Parse `extend TypeName { fn method(self, …) { … } … }`
    fn parse_extend_block_with_decorators(&mut self, decorators: Decorators) -> Result<Stmt> {
        let span = self.peek().span;
        self.advance(); // consume `extend`
        let type_name = self.expect_ident("type name")?;
        // `extend Type: Interface` went with interfaces. Caught here so the
        // message names the removal; without it the author gets
        // "expected `{`, found `:`", which explains nothing.
        if self.peek().kind == TokenKind::Colon {
            return Err(JadeError::ExtendConformanceRemoved { span: self.peek().span });
        }
        if decorators.iter().any(|(n, _)| n == "route") {
            return Err(JadeError::RouteDecoratorRemoved { span });
        }
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
                        expected: "`fn` method definition".to_string(),
                        got: token_kind_desc(&t.kind),
                        span: t.span,
                    });
                }
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Stmt::ExtendBlock { type_name, methods, decorators, span })
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
            Expr::Integer { span, .. } => *span,
            Expr::Float { span, .. } => *span,
            Expr::Bool { span, .. } => *span,
            Expr::Str { span, .. } => *span,
            Expr::Identifier { span, .. } => *span,
            Expr::Call { span, .. } => *span,
            Expr::BinOp { span, .. } => *span,
            Expr::UnaryOp { span, .. } => *span,
            Expr::StructLiteral { span, .. } => *span,
            Expr::FieldAccess { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::Array { span, .. } => *span,
            Expr::FStr { span, .. } => *span,
            Expr::PromptLiteral { span, .. } => *span,
            Expr::PromptDeref { span, .. } => *span,
            Expr::Dict { span, .. } => *span,
            Expr::Closure { span, .. } => *span,
            Expr::Await { span, .. } => *span,
            Expr::Pipe { span, .. } => *span,
        }
    }

    /// Re-lex and re-parse the raw expression source from inside an f-string slot.
    /// Called once per `{…}` interpolation during `parse_primary` for `FStr` tokens.
    fn parse_fstr_expr(src: &str, span: Span, fn_depth: usize) -> Result<Expr> {
        let sub_tokens = super::lexer::tokenize(src).map_err(|_| JadeError::UnexpectedToken {
            expected: "expression".to_string(),
            got: format!("invalid expression `{}`", src),
            span,
        })?;
        let mut sub = Parser {
            tokens: sub_tokens,
            pos: 0,
            fn_depth,
            loop_depth: 0,
            async_fn_depth: 0,
            struct_literal_allowed: true,
        };
        sub.parse_pipe()
    }

    /// Lowest precedence: `|>` (pipe). Left-associative, so `a |> f |> g` is
    /// `g(f(a))` and a prompt dereference chains like anything else.
    ///
    /// This is the **only** place `|>` is consumed. It builds an `Expr::Pipe`
    /// per stage and decides nothing else: whether a stage is a type, a Grammar,
    /// or a function is a question about what its name refers to, which the
    /// parser cannot answer. `compiler::type_infer::infer_pipe` does.
    ///
    /// Two things used to happen here that no longer do. The stage was desugared
    /// straight into `Expr::Call` by matching on its shape, which made a stage
    /// that was not an identifier, call, or field access a *syntax* error
    /// phrased in terms of tokens. And `?p |> …` was parsed somewhere else
    /// entirely — see `parse_primary`'s `Question` arm — so the same operator
    /// had two parse paths and two meanings.
    fn parse_pipe(&mut self) -> Result<Expr> {
        let mut left = self.parse_or()?;
        while self.peek().kind == TokenKind::PipeGt {
            let span = Self::expr_span(&left);
            self.advance(); // consume `|>`
            let stage = self.parse_or()?;
            left = Expr::Pipe { value: Box::new(left), stage: Box::new(stage), span };
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
                left = Expr::BinOp {
                    op: BinOpKind::Or,
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
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
                left = Expr::BinOp {
                    op: BinOpKind::And,
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// `==`, `!=`, `<`, `>`, `<=`, `>=`, `in`, `not in` (comparison, non-associative).
    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_bitor()?;
        loop {
            // `not in` — two-token membership test
            let is_not_in = self.peek().kind == TokenKind::Bang
                && self.peek_at(1).map(|t| t.kind == TokenKind::In).unwrap_or(false);
            let op = if is_not_in {
                self.advance(); // consume `not`
                BinOpKind::NotIn
            } else {
                match self.peek().kind {
                    TokenKind::EqEq => BinOpKind::Eq,
                    TokenKind::BangEq => BinOpKind::Ne,
                    TokenKind::Lt => BinOpKind::Lt,
                    TokenKind::Gt => BinOpKind::Gt,
                    TokenKind::LtEq => BinOpKind::Le,
                    TokenKind::GtEq => BinOpKind::Ge,
                    TokenKind::In => BinOpKind::In,
                    _ => break,
                }
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
                left = Expr::BinOp {
                    op: BinOpKind::BitOr,
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
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
                left = Expr::BinOp {
                    op: BinOpKind::BitXor,
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
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
                left = Expr::BinOp {
                    op: BinOpKind::BitAnd,
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
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
                    left = Expr::BinOp {
                        op: BinOpKind::Shl,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
                }
                TokenKind::GtGt => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_additive()?;
                    left = Expr::BinOp {
                        op: BinOpKind::Shr,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
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
                    left = Expr::BinOp {
                        op: BinOpKind::Add,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
                }
                TokenKind::Minus => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_term()?;
                    left = Expr::BinOp {
                        op: BinOpKind::Sub,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
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
                    left = Expr::BinOp {
                        op: BinOpKind::Mul,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
                }
                TokenKind::Slash => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::BinOp {
                        op: BinOpKind::Div,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
                }
                TokenKind::Percent => {
                    let span = Self::expr_span(&left);
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::BinOp {
                        op: BinOpKind::Mod,
                        left: Box::new(left),
                        right: Box::new(right),
                        span,
                    };
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
    /// Finish a postfix prompt dereference (`obj.(?field)` / `obj~>field`) after
    /// the operator and its `?` have been consumed.  Reads the field name and
    /// closes the parenthesis for the `.(?…)` form.
    ///
    /// A trailing `|> …` is deliberately *not* read here. It sits outside the
    /// parens (`obj.(?p) |> int`), so `parse_pipe` picks it up as an ordinary
    /// stage over this expression, and a postfix deref pipes exactly like a
    /// prefix one.
    fn finish_postfix_deref(
        &mut self,
        object: Expr,
        style: DerefStyle,
        span: Span,
    ) -> Result<Expr> {
        let field = self.expect_ident("prompt field name")?;
        if style == DerefStyle::DotParen {
            self.expect(&TokenKind::RParen)?;
        }
        let target = Expr::FieldAccess { object: Box::new(object), field, span };
        Ok(Expr::PromptDeref { expr: Box::new(target), constraint: None, style, span })
    }

    fn parse_call(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.peek().kind == TokenKind::LParen {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `(`
                let mut args = Vec::new();
                let mut kwargs = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    loop {
                        // Look-ahead: `Ident =` (single `=`, not `==`) means keyword arg.
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
                        let val = self.parse_pipe()?;
                        match kw {
                            None => args.push(val),
                            Some(k) => kwargs.push((k, val)),
                        }
                        if self.peek().kind == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RParen)?;
                expr = Expr::Call { callee: Box::new(expr), args, kwargs, span };
            } else if self.peek().kind == TokenKind::Dot
                && self.peek_ahead(1).kind == TokenKind::LParen
                && self.peek_ahead(2).kind == TokenKind::Question
            {
                // `obj.(?field)` — postfix prompt dereference. The `?` sits next to
                // the field it actually applies to, rather than back at the head
                // of the chain (C's `p->x` vs `(*p).x`).
                let span = Self::expr_span(&expr);
                self.advance(); // consume `.`
                self.advance(); // consume `(`
                self.advance(); // consume `?`
                expr = self.finish_postfix_deref(expr, DerefStyle::DotParen, span)?;
            } else if self.peek().kind == TokenKind::Dot {
                let span = Self::expr_span(&expr);
                self.advance(); // consume `.`
                let field = self.expect_ident("field name")?;
                expr = Expr::FieldAccess { object: Box::new(expr), field, span };
            } else if self.peek().kind == TokenKind::TildeGt {
                // `obj~>field` — terse spelling of `obj.?field`.
                let span = Self::expr_span(&expr);
                self.advance(); // consume `~>`
                expr = self.finish_postfix_deref(expr, DerefStyle::Squiggly, span)?;
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
                // `TypeName { field: expr, … }` — plain struct literal.
                if self.struct_literal_allowed && self.peek().kind == TokenKind::LBrace {
                    return self.parse_struct_literal_body(name, token.span);
                }
                // `ns.TypeName { field: expr, … }` — namespace-qualified struct literal.
                // Requires: `.` then `Identifier` then `{` (3-token lookahead).
                if self.struct_literal_allowed
                    && self.peek().kind == TokenKind::Dot
                    && matches!(
                        self.tokens.get(self.pos + 1).map(|t| &t.kind),
                        Some(TokenKind::Identifier(_))
                    )
                    && self.tokens.get(self.pos + 2).map(|t| &t.kind) == Some(&TokenKind::LBrace)
                {
                    self.advance(); // consume `.`
                    let type_name = self.expect_ident("struct type name")?;
                    let qualified = format!("{}.{}", name, type_name);
                    return self.parse_struct_literal_body(qualified, token.span);
                }
                Ok(Expr::Identifier { name, span: token.span })
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
                // Parse the target expression: supports ?name, ?arr[i], etc.
                let expr = self.parse_call()?;
                // Field access is deliberately excluded: `?obj.field` reads as if
                // the `?` applied to `obj`, when it actually applies to `field`.
                // The postfix forms `obj.(?field)` / `obj~>field` say it properly.
                if let Expr::FieldAccess { field, span: fspan, .. } = &expr {
                    return Err(JadeError::PrefixDerefOnField {
                        field: field.clone(),
                        span: *fspan,
                    });
                }
                // A trailing `|> …` is left for `parse_pipe`. This arm used to
                // take it with `parse_or` — never `parse_pipe` — precisely so a
                // chain could not form, which is why `?p |> int |> double` was
                // unwritable before v1.2.0.
                Ok(Expr::PromptDeref {
                    expr: Box::new(expr),
                    constraint: None,
                    style: DerefStyle::Prefix,
                    span,
                })
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
            // Implicit self: `.field` desugars to `self.field` inside method bodies.
            TokenKind::Dot => {
                let span = token.span;
                self.advance(); // consume `.`
                let field = self.expect_ident("field name")?;
                let self_expr = Expr::Identifier { name: "self".to_string(), span };
                Ok(Expr::FieldAccess { object: Box::new(self_expr), field, span })
            }
            TokenKind::Eof => Err(JadeError::UnexpectedEof { span: token.span }),
            _ => Err(JadeError::UnexpectedToken {
                expected: "expression".to_string(),
                got: token_kind_desc(&token.kind),
                span: token.span,
            }),
        }
    }

    /// Parse the body of a closure: `{ stmts }` or a single expression (implicit return).
    fn parse_closure_body(&mut self, span: Span) -> Result<Vec<Stmt>> {
        self.fn_depth += 1;
        let outer_loops = std::mem::take(&mut self.loop_depth);
        let body = if self.peek().kind == TokenKind::LBrace {
            self.parse_block()?
        } else {
            // Single expression: wrap as implicit return so eval_block returns it.
            let expr = self.parse_pipe()?;
            vec![Stmt::Return { value: Some(expr), span }]
        };
        self.loop_depth = outer_loops;
        self.fn_depth -= 1;
        Ok(body)
    }
}
