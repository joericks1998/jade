//! Turning each prompt dereference into a call into the inference prelude.
//!
//! `?p` is syntax the checker types, as `str`, or as `T` for `?p |> T`, and
//! nothing after the checker knows. Before the merged program is checked, this
//! pass adds `prelude.jde` to it, in a namespace of its own as any imported
//! module is, and replaces each dereference with a call into it:
//!
//! ```text
//! ?p          →  reply$N(__infer.text(p))
//! ?p |> int   →  reply_int$N(__infer.text(p))
//! ```
//!
//! The checker then checks the prelude's functions at the types they are called
//! with, MIR lowers them like any others, and only the handful of operations
//! the prelude asks of `__infer` reach the backend as something new.
//!
//! A dereference into a class, `?p |> City`, or through a grammar is left as it
//! is, and MIR names it as not compiled yet.

use crate::compiler::tir::{JadeType, TCatchArm, TExpr, TExprKind, TFStrPart, TStmt};
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
pub fn inject(mut stmts: Vec<TStmt>) -> Result<Vec<TStmt>, String> {
    let mut found = false;
    for s in &mut stmts {
        stmt(s, &mut found);
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
pub fn dereferences(stmts: &[TStmt]) -> bool {
    let mut copy = stmts.to_vec();
    let mut found = false;
    for s in &mut copy {
        stmt(s, &mut found);
    }
    found
}

/// Purpose: This function names the prelude function a dereference becomes.
///
/// Inputs:
/// - `output_type`: the `|> T` it names, if any.
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
fn node(kind: TExprKind, span: Span) -> TExpr {
    TExpr { kind, ty: JadeType::Unknown, span }
}

/// Purpose: This function rewrites one expression and everything inside it.
///
/// Inputs:
/// - `e`: the expression.
/// - `found`: set when a dereference is seen.
///
/// Outputs: none.
fn expr(e: &mut TExpr, found: &mut bool) {
    match &mut e.kind {
        TExprKind::PromptDeref { expr: inner, output_type, grammar_expr } => {
            expr(inner, found);
            if let Some(g) = grammar_expr {
                expr(g, found);
            }
            *found = true;
            if grammar_expr.is_some() {
                return;
            }
            let Some(f) = target(output_type.as_deref()) else { return };
            let span = e.span;
            let text = node(
                TExprKind::Call {
                    callee: Box::new(node(
                        TExprKind::FieldAccess {
                            object: Box::new(node(TExprKind::Identifier("__infer".into()), span)),
                            field: "text".into(),
                        },
                        span,
                    )),
                    args: vec![(**inner).clone()],
                    kwargs: Vec::new(),
                },
                span,
            );
            e.kind = TExprKind::Call {
                callee: Box::new(node(TExprKind::Identifier(format!("{f}${PRELUDE_ID}")), span)),
                args: vec![text],
                kwargs: Vec::new(),
            };
        }
        TExprKind::Is { value, .. } => expr(value, found),
        TExprKind::Call { callee, args, kwargs } => {
            expr(callee, found);
            for a in args {
                expr(a, found);
            }
            for (_, v) in kwargs {
                expr(v, found);
            }
        }
        TExprKind::BinOp { left, right, .. } => {
            expr(left, found);
            expr(right, found);
        }
        TExprKind::UnaryOp { operand, .. } => expr(operand, found),
        TExprKind::StructLiteral { base, fields, .. } => {
            if let Some(b) = base {
                expr(b, found);
            }
            for (_, v, _) in fields {
                expr(v, found);
            }
        }
        TExprKind::FieldAccess { object, .. } => expr(object, found),
        TExprKind::Index { object, index } => {
            expr(object, found);
            expr(index, found);
        }
        TExprKind::Array { elements } => {
            for x in elements {
                expr(x, found);
            }
        }
        TExprKind::FStr { parts } => {
            for p in parts {
                if let TFStrPart::Expr(x) = p {
                    expr(x, found);
                }
            }
        }
        TExprKind::Dict { entries } => {
            for (k, v) in entries {
                expr(k, found);
                expr(v, found);
            }
        }
        TExprKind::Await { expr: x } | TExprKind::PromptLiteral { body: x } => expr(x, found),
        TExprKind::Integer(_)
        | TExprKind::Float(_)
        | TExprKind::Bool(_)
        | TExprKind::Str(_)
        | TExprKind::Identifier(_) => {}
    }
}

/// Purpose: This function rewrites every expression in a block.
fn block(stmts: &mut [TStmt], found: &mut bool) {
    for s in stmts {
        stmt(s, found);
    }
}

/// Purpose: This function rewrites every expression in one statement.
fn stmt(s: &mut TStmt, found: &mut bool) {
    match s {
        TStmt::Let { value, .. }
        | TStmt::Assign { value, .. }
        | TStmt::Yield { value, .. }
        | TStmt::FieldAssign { value, .. }
        | TStmt::PromptDecl { body: value, .. }
        | TStmt::Raise { value, .. }
        | TStmt::Expr(value) => expr(value, found),
        TStmt::Return { value, .. } => {
            if let Some(v) = value {
                expr(v, found);
            }
        }
        TStmt::IndexAssign { index, value, .. } => {
            expr(index, found);
            expr(value, found);
        }
        TStmt::FnDef { params, body, .. } | TStmt::AsyncFnDef { params, body, .. } => {
            for (_, d) in params {
                if let Some(d) = d {
                    expr(d, found);
                }
            }
            block(body, found);
        }
        TStmt::If { condition, then_body, else_body, .. } => {
            expr(condition, found);
            block(then_body, found);
            if let Some(b) = else_body {
                block(b, found);
            }
        }
        TStmt::While { condition, body, .. } => {
            expr(condition, found);
            block(body, found);
        }
        TStmt::For { iterable, body, .. } => {
            expr(iterable, found);
            block(body, found);
        }
        TStmt::ExtendBlock { methods, .. } => block(methods, found),
        TStmt::TryCatch { body, arms, .. } => {
            block(body, found);
            for TCatchArm { body, .. } in arms {
                block(body, found);
            }
        }
        TStmt::Break { .. }
        | TStmt::Continue { .. }
        | TStmt::StructDef { .. }
        | TStmt::Use { .. }
        | TStmt::FromUse { .. } => {}
    }
}
