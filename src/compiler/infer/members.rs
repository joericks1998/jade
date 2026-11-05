//! Class member rules that need the program before struct defaults are filled.
//!
//! Two rules from `design/v2-classes.md` look at what a program *wrote*, not
//! at what the compiler later filled in, so they run on the merged program
//! before `fill_struct_literal_defaults` adds every omitted field to every
//! literal:
//!
//! - *A literal outside a class sets only its public fields.* A private field
//!   it leaves out must have a default; otherwise the class needs a
//!   constructor, `fn new(…)`, which builds it from inside.
//! - *A method defined outside its class is declared in the body.* `fn
//!   Point::area()` needs `fn area()` in `class Point`, and no method is
//!   defined twice. A declared method with no definition is an error only
//!   where something calls it, as in C++, so a file can declare methods its
//!   importers define; the checker reports that at the call.
//!
//! "Inside a class" means the methods of that class or of a class descending
//! from it, and its constructor, which is the top-level function the parser
//! names `Class::new`.

use std::collections::{HashMap, HashSet};

use crate::compiler::tir::{TExpr, TExprKind, TFStrPart, TProgram, TStmt};
use crate::frontend::ast::{ClassMembers, StructFieldDef};
use crate::frontend::error::{JadeError, Span};

use super::ty::display_name;

/// Purpose: This function finds the class a constructor belongs to, from the
/// name the parser gave it.
///
/// Inputs:
/// - `fn_name`: a function's global name, such as `Account::new`, or
///   `Account::new$3` after the import pass has namespaced it.
///
/// Outputs: the class's name as the program now spells it, such as `Account`
/// or `Account$3`, or `None` for a function that is not a constructor.
pub(super) fn constructor_class(fn_name: &str) -> Option<String> {
    let (base, suffix) = match fn_name.split_once('$') {
        Some((b, s)) => (b, Some(s)),
        None => (fn_name, None),
    };
    let class = base.strip_suffix("::new")?;
    Some(match suffix {
        Some(s) => format!("{class}${s}"),
        None => class.to_string(),
    })
}

/// Purpose: This struct holds what the pass knows about one class.
struct Class {
    /// Every field, inherited ones first, and whether it has a default.
    fields: Vec<(String, bool)>,
    members: ClassMembers,
    parents: Vec<String>,
    span: Span,
}

/// Purpose: This struct holds the pass's view of the whole program.
struct Pass<'a> {
    classes: HashMap<&'a str, Class>,
    ancestors: HashMap<String, Vec<String>>,
    /// Every top-level function name, to find constructors.
    fns: HashSet<&'a str>,
}

/// Purpose: This function checks the member rules over a merged program.
///
/// Inputs:
/// - `program`: the program after imports are merged and before struct
///   defaults are filled.
///
/// Outputs: `Ok(())`, or the first rule a program breaks.
pub fn check_members(program: &TProgram) -> Result<(), JadeError> {
    let mut pass = Pass { classes: HashMap::new(), ancestors: HashMap::new(), fns: HashSet::new() };
    for s in &program.stmts {
        match s {
            TStmt::StructDef { name, fields, parents, members, span } => {
                let fields = fields
                    .iter()
                    .map(|f| (f.name().to_string(), !matches!(f, StructFieldDef::Required(_))))
                    .collect();
                pass.classes.insert(
                    name,
                    Class {
                        fields,
                        members: members.clone(),
                        parents: parents.clone(),
                        span: *span,
                    },
                );
            }
            TStmt::FnDef { name, .. } | TStmt::AsyncFnDef { name, .. } => {
                pass.fns.insert(name);
            }
            _ => {}
        }
    }
    let names: Vec<String> = pass.classes.keys().map(|n| n.to_string()).collect();
    for n in &names {
        let line = pass.ancestry(n);
        pass.ancestors.insert(n.clone(), line);
    }
    pass.check_definitions(&program.stmts)?;
    for s in &program.stmts {
        pass.stmt(s, None)?;
    }
    Ok(())
}

impl<'a> Pass<'a> {
    /// Purpose: This function lists a class's ancestors, nearest first.
    fn ancestry(&self, n: &str) -> Vec<String> {
        let mut line: Vec<String> = Vec::new();
        let mut stack: Vec<String> =
            self.classes.get(n).map(|c| c.parents.clone()).unwrap_or_default();
        while !stack.is_empty() {
            let p = stack.remove(0);
            if line.contains(&p) || p == n {
                continue;
            }
            if let Some(c) = self.classes.get(p.as_str()) {
                stack.extend(c.parents.clone());
            }
            line.push(p);
        }
        line
    }

    /// Purpose: This function says whether class `k` is class `owner` or
    /// descends from it.
    fn is_a(&self, k: &str, owner: &str) -> bool {
        k == owner || self.ancestors.get(k).is_some_and(|a| a.iter().any(|x| x == owner))
    }

    /// Purpose: This function finds the class that declares a field: the
    /// furthest ancestor that has it, as the checker decides it.
    fn field_owner(&self, class: &str, field: &str) -> String {
        let has =
            |c: &str| self.classes.get(c).is_some_and(|c| c.fields.iter().any(|(f, _)| f == field));
        self.ancestors
            .get(class)
            .and_then(|a| a.iter().rev().find(|x| has(x)).cloned())
            .unwrap_or_else(|| class.to_string())
    }

    /// Purpose: This function says whether a field is public in the class that
    /// owns it. A class this pass does not know, such as a built-in one, has
    /// only public fields.
    fn is_public(&self, owner: &str, field: &str) -> bool {
        self.classes.get(owner).is_none_or(|c| c.members.is_public(field))
    }

    /// Purpose: This function checks that every method defined outside its
    /// class is declared there, and that no method is defined twice.
    fn check_definitions(&self, stmts: &[TStmt]) -> Result<(), JadeError> {
        let mut defined: HashMap<(String, String), usize> = HashMap::new();
        for s in stmts {
            let TStmt::ExtendBlock { type_name, methods, outside, span, .. } = s else { continue };
            for m in methods {
                let (TStmt::FnDef { name, .. } | TStmt::AsyncFnDef { name, .. }) = m else {
                    continue;
                };
                if *outside {
                    let declared = self
                        .classes
                        .get(type_name.as_str())
                        .is_some_and(|c| c.members.declared.iter().any(|d| d == name));
                    if !declared {
                        let class = display_name(type_name);
                        return Err(type_error(
                            format!(
                                "fn {class}::{name} defines a method class {class} does not declare; \
                                 add `fn {name}(…)` to the class body"
                            ),
                            *span,
                        ));
                    }
                }
                *defined.entry((type_name.clone(), name.clone())).or_insert(0) += 1;
            }
        }
        for ((class, name), n) in &defined {
            if *n > 1 {
                let c = display_name(class);
                return Err(type_error(
                    format!("{c}::{name} is defined more than once"),
                    self.classes
                        .get(class.as_str())
                        .map(|c| c.span)
                        .unwrap_or(Span { line: 0, col: 0 }),
                ));
            }
        }
        Ok(())
    }

    /// Purpose: This function walks a statement, checking every class literal
    /// in it.
    ///
    /// Inputs:
    /// - `s`: the statement.
    /// - `inside`: the class whose code this is, if any.
    fn stmt(&self, s: &TStmt, inside: Option<&str>) -> Result<(), JadeError> {
        match s {
            TStmt::Let { value, .. } | TStmt::Assign { value, .. } => self.expr(value, inside),
            TStmt::FnDef { name, params, body, .. }
            | TStmt::AsyncFnDef { name, params, body, .. } => {
                let ctor = constructor_class(name);
                let inside = match &ctor {
                    Some(c) if self.classes.contains_key(c.as_str()) => Some(c.as_str()),
                    _ => inside,
                };
                for (_, d) in params {
                    if let Some(d) = d {
                        self.expr(d, inside)?;
                    }
                }
                self.stmts(body, inside)
            }
            TStmt::ExtendBlock { type_name, methods, .. } => self.stmts(methods, Some(type_name)),
            TStmt::Return { value, .. } => value.as_ref().map_or(Ok(()), |v| self.expr(v, inside)),
            TStmt::Yield { value, .. } | TStmt::Raise { value, .. } => self.expr(value, inside),
            TStmt::If { condition, then_body, else_body, .. } => {
                self.expr(condition, inside)?;
                self.stmts(then_body, inside)?;
                else_body.as_ref().map_or(Ok(()), |e| self.stmts(e, inside))
            }
            TStmt::While { condition, body, .. } => {
                self.expr(condition, inside)?;
                self.stmts(body, inside)
            }
            TStmt::For { iterable, body, .. } => {
                self.expr(iterable, inside)?;
                self.stmts(body, inside)
            }
            TStmt::FieldAssign { value, .. } | TStmt::PromptDecl { body: value, .. } => {
                self.expr(value, inside)
            }
            TStmt::IndexAssign { index, value, .. } => {
                self.expr(index, inside)?;
                self.expr(value, inside)
            }
            TStmt::TryCatch { body, arms, .. } => {
                self.stmts(body, inside)?;
                for a in arms {
                    self.stmts(&a.body, inside)?;
                }
                Ok(())
            }
            TStmt::Expr(e) => self.expr(e, inside),
            _ => Ok(()),
        }
    }

    fn stmts(&self, body: &[TStmt], inside: Option<&str>) -> Result<(), JadeError> {
        for s in body {
            self.stmt(s, inside)?;
        }
        Ok(())
    }

    /// Purpose: This function walks an expression, checking class literals.
    fn expr(&self, e: &TExpr, inside: Option<&str>) -> Result<(), JadeError> {
        match &e.kind {
            TExprKind::StructLiteral { type_name, base, fields, given } => {
                for (_, v, _) in fields {
                    self.expr(v, inside)?;
                }
                if let Some(b) = base {
                    self.expr(b, inside)?;
                }
                self.literal(type_name, base.is_some(), given, inside, e.span)
            }
            TExprKind::Call { callee, args, kwargs } => {
                self.expr(callee, inside)?;
                for a in args {
                    self.expr(a, inside)?;
                }
                for (_, v) in kwargs {
                    self.expr(v, inside)?;
                }
                Ok(())
            }
            TExprKind::BinOp { left, right, .. } => {
                self.expr(left, inside)?;
                self.expr(right, inside)
            }
            TExprKind::UnaryOp { operand, .. } => self.expr(operand, inside),
            TExprKind::FieldAccess { object, .. } => self.expr(object, inside),
            TExprKind::Index { object, index } => {
                self.expr(object, inside)?;
                self.expr(index, inside)
            }
            TExprKind::Array { elements } => {
                for x in elements {
                    self.expr(x, inside)?;
                }
                Ok(())
            }
            TExprKind::Dict { entries } => {
                for (k, v) in entries {
                    self.expr(k, inside)?;
                    self.expr(v, inside)?;
                }
                Ok(())
            }
            TExprKind::FStr { parts } => {
                for p in parts {
                    if let TFStrPart::Expr(x) = p {
                        self.expr(x, inside)?;
                    }
                }
                Ok(())
            }
            TExprKind::PromptDeref { expr, grammar_expr, .. } => {
                self.expr(expr, inside)?;
                grammar_expr.as_ref().map_or(Ok(()), |g| self.expr(g, inside))
            }
            TExprKind::Await { expr } => self.expr(expr, inside),
            TExprKind::PromptLiteral { body } => self.expr(body, inside),
            _ => Ok(()),
        }
    }

    /// Purpose: This function checks one class literal: outside the class, it
    /// may set only public fields, and the private fields it leaves out need
    /// defaults.
    fn literal(
        &self,
        class: &str,
        has_base: bool,
        given: &[String],
        inside: Option<&str>,
        span: Span,
    ) -> Result<(), JadeError> {
        let Some(info) = self.classes.get(class) else { return Ok(()) };
        let sees = |owner: &str| inside.is_some_and(|k| self.is_a(k, owner));
        let c = display_name(class);
        let how = if info.members.declared.iter().any(|d| d == "new")
            || self.fns.contains(constructor_name(class).as_str())
        {
            format!("build it with {c}::new(…)")
        } else {
            format!("give {c} a constructor, `fn new(…)`, to build it from inside the class")
        };
        for name in given {
            let owner = self.field_owner(class, name);
            if !self.is_public(&owner, name) && !sees(&owner) {
                return Err(type_error(
                    format!(
                        "field '{name}' is private to {}, so a literal outside the class cannot set it; {how}",
                        display_name(&owner)
                    ),
                    span,
                ));
            }
        }
        if has_base {
            return Ok(());
        }
        for (name, has_default) in &info.fields {
            if *has_default || given.iter().any(|n| n == name) {
                continue;
            }
            let owner = self.field_owner(class, name);
            if self.is_public(&owner, name) {
                // A public field's absence is the per-file pass's to report.
                continue;
            }
            if !sees(&owner) {
                return Err(type_error(
                    format!(
                        "{c}'s private field '{name}' has no default, so a literal outside the class cannot build it; {how}"
                    ),
                    span,
                ));
            }
            return Err(JadeError::MissingField { field: name.clone(), span });
        }
        Ok(())
    }
}

/// Purpose: This function names a class's constructor the way the parser and
/// the import pass name it.
///
/// Inputs:
/// - `class`: the class, such as `Account` or `Account$3`.
///
/// Outputs: `Account::new`, or `Account::new$3`.
fn constructor_name(class: &str) -> String {
    match class.split_once('$') {
        Some((base, suffix)) => format!("{base}::new${suffix}"),
        None => format!("{class}::new"),
    }
}

/// Purpose: This function makes a type error.
fn type_error(message: String, span: Span) -> JadeError {
    JadeError::TypeError { message, span }
}
