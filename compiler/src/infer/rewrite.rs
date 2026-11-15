//! Turning each prompt dereference into a call into the inference prelude.
//!
//! `?p` is syntax the checker types, as `str`, or as `T` for `toT(?p)`, and
//! nothing after the checker knows. Before the merged program is checked, this
//! pass adds `prelude.jde` to it, in a namespace of its own as any imported
//! module is, and replaces each dereference with a call into it:
//!
//! ```text
//! ?p          →  reply$N(__infer.text(p))
//! toInt(?p)   →  reply_int$N(__infer.text(p))
//! ?p |> f     →  reply_pieces$N(__infer.text(p)) |> f
//! ```
//!
//! The checker then checks the prelude's functions at the types they are called
//! with, MIR lowers them like any others, and only the handful of operations
//! the prelude asks of `__infer` reach the backend as something new.
//!
//! In a pipe, `reply_pieces` is a generator, so the pipe calls its stages on
//! each piece of the reply as the model writes it.
//!
//! A dereference into a class, `City(?p)`, or through a grammar, `g(?p)`, is
//! left as it is, and MIR names it as not compiled yet.

use crate::frontend::ast::{CatchArm, Expr, ExprKind, FStrPart, Stmt, StructFieldDef};
use crate::frontend::error::Span;

/// The namespace id the prelude's names are mangled with, as `reply$<id>`.
/// Far above any id the import pass hands out, so the two never meet.
pub const PRELUDE_ID: u32 = u32::MAX - 7;

/// The prelude's source.
pub const PRELUDE: &str = include_str!("prelude.jde");

/// Purpose: This function adds the inference prelude to a program that
/// dereferences a prompt, and rewrites each dereference into a call into it.
///
/// Inputs:
/// - `stmts`: the merged program.
///
/// Outputs: the program, unchanged when it dereferences no prompt, or a message
/// when the prelude does not compile, which is a compiler bug.
pub fn inject(mut stmts: Vec<Stmt>) -> Result<Vec<Stmt>, String> {
    let mut found = false;
    for s in &mut stmts {
        stmt(s, &mut found)?;
    }
    if !found {
        return Ok(stmts);
    }
    let mut prelude =
        crate::aot::imports::namespace_source(PRELUDE, "the inference prelude", PRELUDE_ID)
            .map_err(|e| format!("internal compiler error in the inference prelude: {e}"))?;
    prelude.extend(stmts);
    Ok(prelude)
}

/// Purpose: This function says whether a program dereferences a prompt.
///
/// Inputs:
/// - `stmts`: the program.
///
/// Outputs: whether any `?p` appears in it.
pub fn dereferences(stmts: &[Stmt]) -> bool {
    let mut copy = stmts.to_vec();
    let mut found = false;
    for s in &mut copy {
        if stmt(s, &mut found).is_err() {
            return found;
        }
    }
    found
}

/// Purpose: This function names the prelude function a dereference becomes.
///
/// Inputs:
/// - `output_type`: the type a call around it names, `int` for `toInt(?p)`.
///
/// Outputs: the function's name before mangling, or `None` for a type the
/// prelude does not decode.
fn target(output_type: Option<&str>) -> Option<&'static str> {
    Some(match output_type {
        None | Some("str") => "reply",
        Some("int") => "reply_int",
        Some("float") => "reply_float",
        Some("bool") => "reply_bool",
        Some("char") => "reply_char",
        _ => return None,
    })
}

/// Purpose: This function builds an expression node.
fn node(kind: ExprKind, span: Span) -> Expr {
    Expr::new(kind, span)
}

/// Purpose: This function rewrites one expression and everything inside it.
///
/// Inputs:
/// - `e`: the expression.
/// - `found`: set when a dereference is seen.
///
/// Outputs: `Ok(())`, or an internal error for a pipe the per-file pass did
/// not flatten.
fn expr(e: &mut Expr, found: &mut bool) -> Result<(), String> {
    match &mut e.kind {
        ExprKind::PromptDeref { expr: inner, output_type, grammar_expr, .. } => {
            expr(inner, found)?;
            if let Some(g) = grammar_expr {
                expr(g, found)?;
            }
            *found = true;
            if grammar_expr.is_some() {
                return Ok(());
            }
            let Some(f) = target(output_type.as_deref()) else { return Ok(()) };
            let span = e.span;
            let text = text_of(inner, span);
            e.kind = ExprKind::Call {
                callee: Box::new(node(ExprKind::Identifier(format!("{f}${PRELUDE_ID}")), span)),
                args: vec![text],
                kwargs: Vec::new(),
            };
        }
        ExprKind::Is { value, .. } => expr(value, found)?,
        ExprKind::Call { callee, args, kwargs } => {
            expr(callee, found)?;
            for a in args {
                expr(a, found)?;
            }
            for (_, v) in kwargs {
                expr(v, found)?;
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            expr(left, found)?;
            expr(right, found)?;
        }
        ExprKind::UnaryOp { operand, .. } => expr(operand, found)?,
        ExprKind::StructLiteral { base, fields, .. } => {
            if let Some(b) = base {
                expr(b, found)?;
            }
            for (_, v, _) in fields {
                expr(v, found)?;
            }
        }
        ExprKind::FieldAccess { object, .. } => expr(object, found)?,
        ExprKind::Index { object, index } => {
            expr(object, found)?;
            expr(index, found)?;
        }
        ExprKind::Array { elements } => {
            for x in elements {
                expr(x, found)?;
            }
        }
        ExprKind::FStr { parts } => {
            for p in parts {
                if let FStrPart::Expr(x) = p {
                    expr(x, found)?;
                }
            }
        }
        ExprKind::Dict { entries } => {
            for (k, v) in entries {
                expr(k, found)?;
                expr(v, found)?;
            }
        }
        ExprKind::Await { expr: x } | ExprKind::PromptLiteral { body: x } => expr(x, found)?,
        // `?p |> f`: the source becomes the prelude's generator of the reply's
        // pieces, so the pipe calls `f` on each as the model writes it.
        ExprKind::Pipe { source, hoisted, body, .. } => {
            match &mut source.kind {
                ExprKind::PromptDeref {
                    expr: inner,
                    output_type: None,
                    grammar_expr: None,
                    ..
                } => {
                    expr(inner, found)?;
                    *found = true;
                    let span = source.span;
                    source.kind = ExprKind::Call {
                        callee: Box::new(node(
                            ExprKind::Identifier(format!("reply_pieces${PRELUDE_ID}")),
                            span,
                        )),
                        args: vec![text_of(inner, span)],
                        kwargs: Vec::new(),
                    };
                }
                _ => expr(source, found)?,
            }
            for (_, v) in hoisted {
                expr(v, found)?;
            }
            expr(body, found)?;
        }
        ExprKind::RawPipe { .. } => {
            return Err(format!(
                "[{}:{}] internal compiler error: a pipe reached the inference rewrite before \
                 the per-file pass flattened it",
                e.span.line, e.span.col
            ));
        }
        ExprKind::Integer(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Identifier(_) => {}
    }
    Ok(())
}

/// Purpose: This function builds `__infer.text(p)`, a prompt's text.
///
/// Inputs:
/// - `prompt`: the dereferenced expression.
/// - `span`: where.
///
/// Outputs: the call.
fn text_of(prompt: &Expr, span: Span) -> Expr {
    node(
        ExprKind::Call {
            callee: Box::new(node(
                ExprKind::FieldAccess {
                    object: Box::new(node(ExprKind::Identifier("__infer".into()), span)),
                    field: "text".into(),
                },
                span,
            )),
            args: vec![prompt.clone()],
            kwargs: Vec::new(),
        },
        span,
    )
}

/// Purpose: This function rewrites every expression in a block.
fn block(stmts: &mut [Stmt], found: &mut bool) -> Result<(), String> {
    for s in stmts {
        stmt(s, found)?;
    }
    Ok(())
}

/// Purpose: This function rewrites every expression in one statement.
fn stmt(s: &mut Stmt, found: &mut bool) -> Result<(), String> {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::Yield { value, .. }
        | Stmt::FieldAssign { value, .. }
        | Stmt::PromptDecl { body: value, .. }
        | Stmt::Raise { value, .. }
        | Stmt::Expr(value) => expr(value, found),
        Stmt::Return { value, .. } => match value {
            Some(v) => expr(v, found),
            None => Ok(()),
        },
        Stmt::IndexAssign { index, value, .. } => {
            expr(index, found)?;
            expr(value, found)
        }
        Stmt::FnDef { params, body, .. } | Stmt::AsyncFnDef { params, body, .. } => {
            for (_, d) in params {
                if let Some(d) = d {
                    expr(d, found)?;
                }
            }
            block(body, found)
        }
        Stmt::If { condition, then_body, else_body, .. } => {
            expr(condition, found)?;
            block(then_body, found)?;
            match else_body {
                Some(b) => block(b, found),
                None => Ok(()),
            }
        }
        Stmt::While { condition, body, .. } => {
            expr(condition, found)?;
            block(body, found)
        }
        Stmt::For { iterable, body, .. } => {
            expr(iterable, found)?;
            block(body, found)
        }
        Stmt::ExtendBlock { methods, .. } => block(methods, found),
        Stmt::TryCatch { body, arms, .. } => {
            block(body, found)?;
            for CatchArm { body, .. } in arms {
                block(body, found)?;
            }
            Ok(())
        }
        // A copy-with literal may take a field's default from the class when
        // its base lacks the field, so a default is rewritten like any value.
        Stmt::StructDef { fields, .. } => {
            for f in fields {
                if let StructFieldDef::Let { default, .. }
                | StructFieldDef::Prompt { default, .. } = f
                {
                    expr(default, found)?;
                }
            }
            Ok(())
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Use { .. } | Stmt::FromUse { .. } => {
            Ok(())
        }
    }
}
