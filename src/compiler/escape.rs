//! Type-aware escape analysis for arena allocation (Phase 2, increment 4).
//!
//! Decides which array literals in a function may be allocated in the per-frame
//! bump arena (`jade_runtime::arena`) instead of the reference-counted heap: an
//! arena value is freed in bulk when its region resets, so it must never outlive
//! that region. Getting this wrong is a use-after-free, so the analysis is
//! deliberately conservative — it marks a literal arena-eligible only when it can
//! *prove* non-escape, and treats anything it doesn't understand as escaping.
//!
//! ## What v1 accepts (intentionally narrow)
//!
//! An `Array { elements }` literal bound by `let x = [...]` is eligible when:
//!
//!  1. every element is an **immediate scalar** — `Int`/`Float`/`Bool`/`Nil`.
//!     These are value words with no heap ownership, so storing them in the arena
//!     without a refcount is safe and `reset` needs to run no destructors.
//!     (`Str` is excluded — it is a heap pointer with its own lifetime;
//!     collections are excluded — they would have to be arena too; both are later
//!     work. Dicts/structs are excluded entirely because their `String` keys /
//!     field names would leak at `reset`.)
//!  2. `x` **does not escape**: every use of `x` in the function is as the object
//!     of an indexing expression `x[i]` whose result is itself a scalar. Any other
//!     use — returning it, passing it to a call, assigning it elsewhere, storing
//!     it into another collection, taking `x` without indexing — is an escape.
//!  3. the function is **not async** (an arena value must not cross an `await`;
//!     async resumes on another thread with a different arena).
//!  4. `x` is bound exactly once (no shadowing/reassignment to reason about).
//!
//! The result also reports the arena-bound variable names, because the codegen
//! must exclude their slots from the function's scope-exit decref (decref'ing an
//! arena pointer would send `free_obj` into arena memory) and store them without
//! refcounting.

use std::collections::{HashMap, HashSet};

use crate::compiler::tir::{JadeType, TExpr, TExprKind, TFStrPart, TStmt};

/// Identifies a literal by its source position `(line, col)`. The emit computes
/// the same key when it lowers the literal, to look it up in [`ArenaPlan`].
/// (`Span` itself is not `Hash`.)
pub type SpanKey = (usize, usize);

/// The arena-allocation plan for one function.
#[derive(Debug, Default, Clone)]
pub struct ArenaPlan {
    /// Source positions of `Array` literals to allocate in the arena.
    pub eligible: HashSet<SpanKey>,
    /// Names of variables bound to arena arrays — their slots are excluded from
    /// scope-exit decref and stored without refcounting.
    pub arena_vars: HashSet<String>,
}

impl ArenaPlan {
    pub fn is_empty(&self) -> bool {
        self.eligible.is_empty()
    }
}

/// Whether a type is an immediate scalar word (no heap ownership).
fn is_immediate_scalar(ty: &JadeType) -> bool {
    matches!(ty, JadeType::Int | JadeType::Float | JadeType::Bool | JadeType::Nil)
}

/// Analyze a function body, returning which array literals may be arena-allocated.
pub fn analyze(body: &[TStmt]) -> ArenaPlan {
    let mut plan = ArenaPlan::default();

    // An arena value must not cross an await; if the function suspends anywhere,
    // decline the whole function (simple and sound).
    if stmts_contain_await(body) {
        return plan;
    }

    // Count how many times each name is `let`-bound across the function; a name
    // bound more than once (shadowing) is not analyzed (conservative).
    let mut bind_count: HashMap<&str, usize> = HashMap::new();
    count_bindings(body, &mut bind_count);

    for st in body {
        collect_eligible(st, body, &bind_count, &mut plan);
    }
    plan
}

/// Walk statements looking for `let x = [scalars…]` that qualifies. Recurses into
/// nested blocks so a literal inside a loop body is considered against uses in
/// that same function (uses of a block-local `x` cannot appear outside its block,
/// so scanning the whole function body is a sound over-approximation).
fn collect_eligible(
    st: &TStmt,
    fn_body: &[TStmt],
    binds: &HashMap<&str, usize>,
    plan: &mut ArenaPlan,
) {
    match st {
        TStmt::Let { name, value, .. } => {
            if let TExprKind::Array { elements } = &value.kind {
                let all_scalar = elements.iter().all(|e| is_immediate_scalar(&e.ty));
                let bound_once = binds.get(name.as_str()).copied().unwrap_or(0) == 1;
                if all_scalar && bound_once && !escapes(name, fn_body) {
                    plan.eligible.insert((value.span.line, value.span.col));
                    plan.arena_vars.insert(name.clone());
                }
            }
            // A literal could also be nested in the initializer; v1 only handles
            // the direct `let x = [...]` form, so no deeper recursion here.
        }
        TStmt::If { then_body, else_body, .. } => {
            for s in then_body {
                collect_eligible(s, fn_body, binds, plan);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    collect_eligible(s, fn_body, binds, plan);
                }
            }
        }
        TStmt::While { body, .. } | TStmt::For { body, .. } => {
            for s in body {
                collect_eligible(s, fn_body, binds, plan);
            }
        }
        TStmt::TryCatch { body, arms, .. } => {
            for s in body {
                collect_eligible(s, fn_body, binds, plan);
            }
            for arm in arms {
                for s in &arm.body {
                    collect_eligible(s, fn_body, binds, plan);
                }
            }
        }
        // Nested function definitions are analyzed on their own, not here.
        _ => {}
    }
}

/// Whether `name` escapes anywhere in `stmts`: true unless every use is the object
/// of a scalar-returning index expression.
fn escapes(name: &str, stmts: &[TStmt]) -> bool {
    let mut ok = 0usize;
    let mut bad = 0usize;
    for st in stmts {
        scan_stmt(st, name, &mut ok, &mut bad);
    }
    bad > 0
}

/// A use of `name` that is *not* a scalar-index object read. Any occurrence bumps
/// `bad` unless it is consumed as `name[i]` with a scalar result.
fn scan_expr(e: &TExpr, name: &str, ok: &mut usize, bad: &mut usize) {
    match &e.kind {
        TExprKind::Identifier(n) => {
            if n == name {
                *bad += 1;
            }
        }
        TExprKind::Index { object, index } => {
            // The one allowed context: `name[index]` yielding a scalar.
            if let TExprKind::Identifier(n) = &object.kind
                && n == name
                && is_immediate_scalar(&e.ty)
            {
                *ok += 1;
                scan_expr(index, name, ok, bad); // `name` inside `index` is not OK
                return;
            }
            scan_expr(object, name, ok, bad);
            scan_expr(index, name, ok, bad);
        }
        TExprKind::Integer(_) | TExprKind::Float(_) | TExprKind::Bool(_) | TExprKind::Str(_) => {}
        TExprKind::Call { callee, args, kwargs } => {
            scan_expr(callee, name, ok, bad);
            for a in args {
                scan_expr(a, name, ok, bad);
            }
            for (_, v) in kwargs {
                scan_expr(v, name, ok, bad);
            }
        }
        TExprKind::BinOp { left, right, .. } => {
            scan_expr(left, name, ok, bad);
            scan_expr(right, name, ok, bad);
        }
        TExprKind::UnaryOp { operand, .. } => scan_expr(operand, name, ok, bad),
        TExprKind::StructLiteral { base, fields, .. } => {
            // The `...base` counts as a use like any other. Skipping it would
            // let a name escape into the copy unseen, and an escaping value
            // read as arena-eligible is a dangling pointer.
            if let Some(b) = base {
                scan_expr(b, name, ok, bad);
            }
            for (_, v, _) in fields {
                scan_expr(v, name, ok, bad);
            }
        }
        TExprKind::FieldAccess { object, .. } => scan_expr(object, name, ok, bad),
        TExprKind::Array { elements } => {
            for el in elements {
                scan_expr(el, name, ok, bad);
            }
        }
        TExprKind::FStr { parts } => {
            for p in parts {
                if let TFStrPart::Expr(x) = p {
                    scan_expr(x, name, ok, bad);
                }
            }
        }
        TExprKind::PromptDeref { expr, grammar_expr, .. } => {
            scan_expr(expr, name, ok, bad);
            if let Some(g) = grammar_expr {
                scan_expr(g, name, ok, bad);
            }
        }
        TExprKind::Dict { entries } => {
            for (k, v) in entries {
                scan_expr(k, name, ok, bad);
                scan_expr(v, name, ok, bad);
            }
        }
        TExprKind::Closure { body, .. } => {
            // A closure could capture `name`; treat any use inside as escaping.
            for s in body {
                scan_stmt(s, name, ok, bad);
            }
        }
        TExprKind::Await { expr } => scan_expr(expr, name, ok, bad),
        TExprKind::PromptLiteral { body } => scan_expr(body, name, ok, bad),
    }
}

fn scan_stmt(st: &TStmt, name: &str, ok: &mut usize, bad: &mut usize) {
    match st {
        TStmt::Let { value, .. } | TStmt::Assign { value, .. } => {
            scan_expr(value, name, ok, bad)
        }
        TStmt::Expr(e) => scan_expr(e, name, ok, bad),
        TStmt::Return { value, .. } => {
            if let Some(v) = value {
                scan_expr(v, name, ok, bad);
            }
        }
        // A yielded value outlives the frame that produced it — it lands in the
        // stream's buffer and the caller reads it later — so it must not be
        // arena-allocated, exactly like a returned one.
        TStmt::Yield { value, .. } => scan_expr(value, name, ok, bad),
        TStmt::If { condition, then_body, else_body, .. } => {
            scan_expr(condition, name, ok, bad);
            for s in then_body {
                scan_stmt(s, name, ok, bad);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    scan_stmt(s, name, ok, bad);
                }
            }
        }
        TStmt::While { condition, body, .. } => {
            scan_expr(condition, name, ok, bad);
            for s in body {
                scan_stmt(s, name, ok, bad);
            }
        }
        TStmt::For { iterable, body, .. } => {
            scan_expr(iterable, name, ok, bad);
            for s in body {
                scan_stmt(s, name, ok, bad);
            }
        }
        TStmt::FieldAssign { value, .. } => scan_expr(value, name, ok, bad),
        TStmt::IndexAssign { index, value, .. } => {
            scan_expr(index, name, ok, bad);
            scan_expr(value, name, ok, bad);
        }
        TStmt::PromptDecl { body, .. } => scan_expr(body, name, ok, bad),
        TStmt::Raise { value, .. } => scan_expr(value, name, ok, bad),
        TStmt::TryCatch { body, arms, .. } => {
            for s in body {
                scan_stmt(s, name, ok, bad);
            }
            for arm in arms {
                for s in &arm.body {
                    scan_stmt(s, name, ok, bad);
                }
            }
        }
        TStmt::FnDef { body, .. } | TStmt::AsyncFnDef { body, .. } => {
            // A nested fn referencing `name` captures it — escaping.
            for s in body {
                scan_stmt(s, name, ok, bad);
            }
        }
        TStmt::ExtendBlock { methods, .. } => {
            for s in methods {
                scan_stmt(s, name, ok, bad);
            }
        }
        TStmt::StructDef { .. }
        | TStmt::Use { .. }
        | TStmt::FromUse { .. }
        // Control flow, holding no expression to scan.
        | TStmt::Break { .. }
        | TStmt::Continue { .. } => {}
    }
}

/// Count `let` bindings per name across the function (nested blocks included).
fn count_bindings<'a>(stmts: &'a [TStmt], out: &mut HashMap<&'a str, usize>) {
    for st in stmts {
        match st {
            TStmt::Let { name, .. } => *out.entry(name.as_str()).or_insert(0) += 1,
            TStmt::If { then_body, else_body, .. } => {
                count_bindings(then_body, out);
                if let Some(eb) = else_body {
                    count_bindings(eb, out);
                }
            }
            TStmt::While { body, .. } | TStmt::For { body, .. } => count_bindings(body, out),
            TStmt::TryCatch { body, arms, .. } => {
                count_bindings(body, out);
                for arm in arms {
                    count_bindings(&arm.body, out);
                }
            }
            _ => {}
        }
    }
}

fn stmts_contain_await(stmts: &[TStmt]) -> bool {
    stmts.iter().any(stmt_contains_await)
}

fn stmt_contains_await(st: &TStmt) -> bool {
    match st {
        TStmt::Let { value, .. } | TStmt::Assign { value, .. } => expr_contains_await(value),
        TStmt::Expr(e) => expr_contains_await(e),
        TStmt::Return { value, .. } => value.as_ref().is_some_and(expr_contains_await),
        TStmt::Yield { value, .. } => expr_contains_await(value),
        TStmt::If { condition, then_body, else_body, .. } => {
            expr_contains_await(condition)
                || stmts_contain_await(then_body)
                || else_body.as_ref().is_some_and(|e| stmts_contain_await(e))
        }
        TStmt::While { condition, body, .. } => {
            expr_contains_await(condition) || stmts_contain_await(body)
        }
        TStmt::For { iterable, body, .. } => {
            expr_contains_await(iterable) || stmts_contain_await(body)
        }
        TStmt::FieldAssign { value, .. } | TStmt::PromptDecl { body: value, .. } => {
            expr_contains_await(value)
        }
        TStmt::IndexAssign { index, value, .. } => {
            expr_contains_await(index) || expr_contains_await(value)
        }
        TStmt::Raise { value, .. } => expr_contains_await(value),
        TStmt::TryCatch { body, arms, .. } => {
            stmts_contain_await(body) || arms.iter().any(|a| stmts_contain_await(&a.body))
        }
        // A nested (async) fn definition's awaits belong to that fn, not this one.
        _ => false,
    }
}

fn expr_contains_await(e: &TExpr) -> bool {
    match &e.kind {
        TExprKind::Await { .. } => true,
        TExprKind::Call { callee, args, kwargs } => {
            expr_contains_await(callee)
                || args.iter().any(expr_contains_await)
                || kwargs.iter().any(|(_, v)| expr_contains_await(v))
        }
        TExprKind::BinOp { left, right, .. } => {
            expr_contains_await(left) || expr_contains_await(right)
        }
        TExprKind::UnaryOp { operand, .. } => expr_contains_await(operand),
        TExprKind::Index { object, index } => {
            expr_contains_await(object) || expr_contains_await(index)
        }
        TExprKind::FieldAccess { object, .. } => expr_contains_await(object),
        TExprKind::Array { elements } => elements.iter().any(expr_contains_await),
        TExprKind::StructLiteral { base, fields, .. } => {
            base.as_ref().is_some_and(|b| expr_contains_await(b))
                || fields.iter().any(|(_, v, _)| expr_contains_await(v))
        }
        TExprKind::Dict { entries } => {
            entries.iter().any(|(k, v)| expr_contains_await(k) || expr_contains_await(v))
        }
        TExprKind::FStr { parts } => parts.iter().any(|p| match p {
            TFStrPart::Expr(x) => expr_contains_await(x),
            TFStrPart::Literal(_) => false,
        }),
        TExprKind::PromptDeref { expr, grammar_expr, .. } => {
            expr_contains_await(expr)
                || grammar_expr.as_ref().is_some_and(|g| expr_contains_await(g))
        }
        TExprKind::PromptLiteral { body } => expr_contains_await(body),
        // A closure body's awaits belong to the closure.
        TExprKind::Closure { .. }
        | TExprKind::Identifier(_)
        | TExprKind::Integer(_)
        | TExprKind::Float(_)
        | TExprKind::Bool(_)
        | TExprKind::Str(_) => false,
    }
}

#[cfg(test)]
mod tests;
