//! The per-file pass: the parser's tree in, the same tree rewritten, out.
//!
//! It runs once per file, before imports are merged, and does the jobs that
//! need to know what each name is but not what type each value has:
//!
//! - flattening a pipe chain, `RawPipe(RawPipe(a, f), g(x))`, into one `Pipe`
//!   with its hoisted stage arguments and the name of the piece in hand;
//! - folding `toInt(?p)`, `City(?p)`, `array(?p)`, `dict(?p)` and `g(?p)` into
//!   a typed `PromptDeref`;
//! - flattening each class's inherited fields into its own list and filling a
//!   class literal's defaults, for the classes this file defines;
//! - the errors one file can settle: undefined names, unknown classes, fields
//!   and parents, inheritance cycles and clashes, `yield` beside a returned
//!   value, duplicate and missing fields, quoted imports and import aliases,
//!   unknown package functions, and pipe stages that cannot work.
//!
//! What it keeps is a table of what each name is, [`NameKind`]: a function, a
//! class, a grammar, a prompt, a package, or some other value. The pipe and
//! dereference rewrites need that and nothing more. Every type is worked out
//! once, by the whole-program checker in `typechecker/infer/`, after imports
//! are merged, and the type errors are its to report.
//!
//! [`fill_struct_literal_defaults`] is the half that has to wait for the
//! merge: a class literal whose class, or a parent of it, came from another
//! file gets its inherited fields and defaults there.

use std::collections::{HashMap, HashSet};

use crate::{
    builtins,
    frontend::{
        ast::{CatchArm, DecoratorList, Expr, ExprKind, FStrPart, Program, Stmt, StructFieldDef},
        error::{FieldOwner, JadeError, Result, Span},
    },
};

// ── The name table ───────────────────────────────────────────────────────────

/// What a name is, as far as one file can tell without types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A `fn`, a builtin function, or a converter.
    Function,
    /// An `async fn`.
    AsyncFunction,
    /// A `fn` whose body yields.
    Generator,
    /// A class, which a value can be viewed as: `Animal(x)`.
    Class,
    /// A value made by `Grammar.new(…)`.
    Grammar,
    /// A `prompt` declaration or a `prompt …` expression.
    Prompt,
    /// A std package a `use` named, such as `math`.
    Package,
    /// Any other value: a `let`, a parameter, a loop variable, a caught error.
    Value,
}

impl NameKind {
    /// Purpose: This function says whether a name of this kind is a function
    /// a call or a pipe stage can name directly.
    fn is_function(self) -> bool {
        matches!(self, NameKind::Function | NameKind::AsyncFunction | NameKind::Generator)
    }
}

/// The per-file pass's state: what each name in scope is, and what each class
/// in this file declares.
///
/// Uses a scope stack matching the language's scoping: `scopes[0]` is global,
/// `scopes.last()` is the innermost. Classes live in flat maps, since a class
/// is always defined at the top level.
pub struct Names {
    /// Name → what it is, innermost scope last.
    scopes: Vec<HashMap<String, NameKind>>,
    /// Class name → its fields, flattened once `resolve_inheritance` has run.
    struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Class name → the names of its methods, inherited ones included once
    /// `resolve_inheritance` has run.
    extend_methods: HashMap<String, HashSet<String>>,
    /// Parents as written, per class.
    struct_parents: HashMap<String, Vec<String>>,
    /// Every class a class inherits, nearest first, flattened transitively.
    struct_ancestors: HashMap<String, Vec<String>>,
    /// Each class's member decorators and declarations, as its body wrote them.
    struct_members: HashMap<String, crate::frontend::ast::ClassMembers>,
    /// Where each class was declared, so an inheritance error points at the
    /// child rather than at wherever the clash was noticed.
    struct_spans: HashMap<String, Span>,
    /// True if this file imports names this pass cannot see. When true, an
    /// unknown name is left for the merge rather than refused, because an
    /// import may be what defines it.
    ///
    /// **Only imports that are actually opaque count.** A `use std::fs` is not:
    /// a std package contributes one name, its own, which this pass defines,
    /// so after one there is still no name this pass cannot account for.
    /// Letting it turn the check off cost more than it sounds like: every
    /// undefined name in a file with a single std import went through. `exit()`
    /// is the one people reach for; it has never existed in Jade.
    ///
    /// What stays lenient is what genuinely is: a `use` of a user module or a
    /// package, whose top-level names arrive with the merge, and every
    /// `from … use a, b`, which binds bare names this pass never sees.
    opaque_imports: bool,
    /// Std packages this file imported, by the name they bind (`math`, `fs`).
    /// A package is a fixed set of functions known now, so `math.round(2.5)`
    /// can be refused here, naming the module and the function. An entry is
    /// dropped the moment anything else binds the name, so a local that
    /// shadows a package is not checked against it.
    imported_packages: HashSet<String>,
    /// How many pipes this file has lowered, so each one's piece and hoisted
    /// arguments get names no other pipe uses: `$piece3`, `$arg3.0`.
    next_pipe: usize,
    /// The classes whose defaults are being filled in right now, innermost
    /// last, so a default that builds its own class is refused rather than
    /// filled forever.
    filling: Vec<String>,
}

impl Default for Names {
    fn default() -> Self {
        Self::new()
    }
}

impl Names {
    /// Purpose: This function makes the table every file starts with: the
    /// globals a program has without an import.
    pub fn new() -> Self {
        let mut names = Names {
            scopes: vec![HashMap::new()],
            struct_defs: HashMap::new(),
            extend_methods: HashMap::new(),
            struct_parents: HashMap::new(),
            struct_ancestors: HashMap::new(),
            struct_members: HashMap::new(),
            struct_spans: HashMap::new(),
            opaque_imports: false,
            imported_packages: HashSet::new(),
            next_pipe: 0,
            filling: Vec::new(),
        };
        builtins::register_core_names(&mut names);
        names
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Leave a block scope, carrying any binding that shadowed an outer name
    /// back out to it.
    ///
    /// A `let` inside a block rebinds the name for the rest of the function,
    /// which is what `docs/docs/control-flow.md` documents ("a `let` inside a
    /// loop body does not shadow, it overwrites"). So a name the outer scope
    /// already knows takes on what the block bound it to. A name introduced
    /// *only* inside the block stays unknown outside it.
    fn pop_block_scope(&mut self) {
        if self.scopes.len() <= 1 {
            return;
        }
        let inner = self.scopes.pop().unwrap_or_default();
        for (name, kind) in inner {
            for scope in self.scopes.iter_mut().rev() {
                if scope.contains_key(&name) {
                    scope.insert(name.clone(), kind);
                    break;
                }
            }
        }
    }

    /// Bind `name` in the innermost scope, as a `let` or a parameter does.
    pub fn define(&mut self, name: String, kind: NameKind) {
        // Anything that binds the name is now what the name means, so it is no
        // longer the package's namespace and must not be checked against it.
        self.imported_packages.remove(&name);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, kind);
        }
    }

    /// Rebind `name` in the nearest scope that already holds it, or introduce
    /// it in the global scope if none does, which is what a bare assignment
    /// means.
    fn assign(&mut self, name: &str, kind: NameKind) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), kind);
                return;
            }
        }
        if let Some(global) = self.scopes.first_mut() {
            global.insert(name.to_string(), kind);
        }
    }

    /// Look `name` up from the innermost scope outward.
    fn get(&self, name: &str) -> Option<NameKind> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).copied())
    }

    /// Purpose: This function says what a name bound to a value is, from the
    /// value's syntax alone.
    ///
    /// Inputs:
    /// - `value`: the rewritten value.
    ///
    /// Outputs: `Grammar` for `Grammar.new(…)`, `Prompt` for `prompt …`, what
    /// another name is for a bare name of a function, class, grammar or
    /// prompt, and `Value` for anything else.
    fn kind_of(&self, value: &Expr) -> NameKind {
        match &value.kind {
            ExprKind::Call { callee, .. } if is_grammar_new(callee) => NameKind::Grammar,
            ExprKind::PromptLiteral { .. } => NameKind::Prompt,
            ExprKind::Identifier(n) => match self.get(n) {
                Some(NameKind::Package | NameKind::Value) | None => NameKind::Value,
                Some(k) => k,
            },
            _ => NameKind::Value,
        }
    }
}

/// Purpose: This function says whether a callee is `Grammar.new`.
fn is_grammar_new(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::FieldAccess { object, field }
        if field == "new" && matches!(&object.kind, ExprKind::Identifier(n) if n == "Grammar"))
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Purpose: This function runs the per-file pass over one file.
///
/// Two passes: `pre_pass` registers every top-level function and class so a
/// forward reference resolves, then each statement is rewritten in order. The
/// first error stops it.
///
/// Inputs:
/// - `program`: the file as the parser built it.
///
/// Outputs: the rewritten file, which holds no `RawPipe`, or the first error.
pub fn resolve(program: Program) -> Result<Program> {
    let mut ctx = Names::new();
    pre_pass(&program.stmts, &mut ctx);
    resolve_inheritance(&mut ctx)?;
    let stmts = resolve_stmts(program.stmts, &mut ctx)?;
    Ok(Program { stmts })
}

// ── Cross-file struct-literal default fill ────────────────────────────────────
//
// The per-file pass cannot see an imported class, so a literal like
// `messages.Session { system: …, tools: … }` leaves its default-bearing fields
// (`let _history = []`) out. This fills them in over the merged program,
// before the checker, so MIR finds every field of a literal with no base.

/// Purpose: This function finishes, over the merged program, the class work
/// that crossed a file boundary: a child of an imported class gets its
/// parent's fields, and a literal of an imported class gets its defaults.
///
/// Inputs:
/// - `program`: the merged program, imports inlined and renamed.
///
/// Outputs: `Ok(())`, or an error for a default that builds its own class.
pub fn fill_struct_literal_defaults(program: &mut Program) -> Result<()> {
    // Collect every StructDef (from this file and from inlined imports).
    let mut defs: HashMap<String, Vec<StructFieldDef>> = HashMap::new();
    collect_struct_defs_into(&program.stmts, &mut defs);

    // Then finish any inheritance that crossed a file boundary.
    //
    // `resolve_inheritance` ran while each file was resolved on its own, where
    // an imported parent is deliberately out of reach, so a child of one still
    // carries only its own fields. This is the first point where every module
    // is in one stream. The result is written back into the nodes as well as
    // into `defs`, because a literal's missing defaults are filled from `defs`
    // just below while the checker and MIR read the nodes.
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    collect_struct_parents_into(&program.stmts, &mut parents);
    fold_inherited_fields(&mut defs, &parents);
    write_back_struct_fields(&mut program.stmts, &defs);

    let mut filling = Vec::new();
    fill_in_stmts(&mut program.stmts, &defs, &mut filling)
}

fn collect_struct_parents_into(stmts: &[Stmt], out: &mut HashMap<String, Vec<String>>) {
    for s in stmts {
        match s {
            Stmt::StructDef { name, parents, .. } => {
                out.entry(name.clone()).or_insert_with(|| parents.clone());
            }
            Stmt::FnDef { body, .. } | Stmt::AsyncFnDef { body, .. } => {
                collect_struct_parents_into(body, out)
            }
            Stmt::ExtendBlock { methods, .. } => collect_struct_parents_into(methods, out),
            _ => {}
        }
    }
}

/// Fold each parent's fields into a child that is still missing them.
///
/// Transitive, and idempotent by name: a field the child already has is either
/// its own or one the per-file pass folded in when the parent was local. A
/// genuine clash was refused before this ran, so nothing here has to decide
/// anything.
///
/// Only *direct* parents are read, and every parent is completed before any of
/// its children, so a parent's list is already whole when a child copies it and
/// a grandparent arrives through the parent. Walking the full ancestor set with
/// a linear membership scan instead — which is what this did first — cost a
/// factor of the depth twice over: `jade check` on a chain of 400 structs spent
/// 26 of its 27 seconds here.
fn fold_inherited_fields(
    defs: &mut HashMap<String, Vec<StructFieldDef>>,
    parents: &HashMap<String, Vec<String>>,
) {
    fn complete(
        name: &str,
        defs: &mut HashMap<String, Vec<StructFieldDef>>,
        parents: &HashMap<String, Vec<String>>,
        done: &mut HashSet<String>,
    ) {
        if !done.insert(name.to_string()) {
            return;
        }
        let ps = parents.get(name).cloned().unwrap_or_default();
        for p in &ps {
            complete(p, defs, parents, done);
        }
        let Some(own) = defs.get(name).cloned() else {
            return;
        };
        let mut known: HashSet<String> = own.iter().map(|f| f.name().to_string()).collect();
        let mut inherited: Vec<StructFieldDef> = Vec::new();
        for p in &ps {
            for f in defs.get(p).into_iter().flatten() {
                if known.insert(f.name().to_string()) {
                    inherited.push(f.clone());
                }
            }
        }
        if !inherited.is_empty() {
            inherited.extend(own);
            defs.insert(name.to_string(), inherited);
        }
    }

    let mut done: HashSet<String> = HashSet::new();
    for name in parents.keys() {
        complete(name, defs, parents, &mut done);
    }
}

fn write_back_struct_fields(stmts: &mut [Stmt], defs: &HashMap<String, Vec<StructFieldDef>>) {
    for s in stmts.iter_mut() {
        match s {
            Stmt::StructDef { name, fields, .. } => {
                if let Some(full) = defs.get(name) {
                    *fields = full.clone();
                }
            }
            Stmt::FnDef { body, .. } | Stmt::AsyncFnDef { body, .. } => {
                write_back_struct_fields(body, defs)
            }
            Stmt::ExtendBlock { methods, .. } => write_back_struct_fields(methods, defs),
            _ => {}
        }
    }
}

fn collect_struct_defs_into(stmts: &[Stmt], out: &mut HashMap<String, Vec<StructFieldDef>>) {
    for s in stmts {
        match s {
            Stmt::StructDef { name, fields, .. } => {
                out.entry(name.clone()).or_insert_with(|| fields.clone());
            }
            Stmt::FnDef { body, .. } | Stmt::AsyncFnDef { body, .. } => {
                collect_struct_defs_into(body, out);
            }
            Stmt::ExtendBlock { methods, .. } => collect_struct_defs_into(methods, out),
            _ => {}
        }
    }
}

fn fill_in_stmts(
    stmts: &mut [Stmt],
    defs: &HashMap<String, Vec<StructFieldDef>>,
    filling: &mut Vec<String>,
) -> Result<()> {
    for s in stmts.iter_mut() {
        fill_in_stmt(s, defs, filling)?;
    }
    Ok(())
}

fn fill_in_stmt(
    s: &mut Stmt,
    defs: &HashMap<String, Vec<StructFieldDef>>,
    filling: &mut Vec<String>,
) -> Result<()> {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::Expr(value)
        | Stmt::PromptDecl { body: value, .. }
        | Stmt::Raise { value, .. } => fill_in_expr(value, defs, filling),
        Stmt::Return { value: Some(value), .. } => fill_in_expr(value, defs, filling),
        Stmt::FnDef { body, .. } | Stmt::AsyncFnDef { body, .. } => {
            fill_in_stmts(body, defs, filling)
        }
        Stmt::If { condition, then_body, else_body, .. } => {
            fill_in_expr(condition, defs, filling)?;
            fill_in_stmts(then_body, defs, filling)?;
            if let Some(eb) = else_body {
                fill_in_stmts(eb, defs, filling)?;
            }
            Ok(())
        }
        Stmt::While { condition, body, .. } => {
            fill_in_expr(condition, defs, filling)?;
            fill_in_stmts(body, defs, filling)
        }
        Stmt::For { iterable, body, .. } => {
            fill_in_expr(iterable, defs, filling)?;
            fill_in_stmts(body, defs, filling)
        }
        Stmt::FieldAssign { value, .. } | Stmt::IndexAssign { value, .. } => {
            fill_in_expr(value, defs, filling)
        }
        Stmt::ExtendBlock { methods, .. } => fill_in_stmts(methods, defs, filling),
        Stmt::TryCatch { body, arms, .. } => {
            fill_in_stmts(body, defs, filling)?;
            for arm in arms.iter_mut() {
                fill_in_stmts(&mut arm.body, defs, filling)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn fill_in_expr(
    e: &mut Expr,
    defs: &HashMap<String, Vec<StructFieldDef>>,
    filling: &mut Vec<String>,
) -> Result<()> {
    let span = e.span;
    // Recurse first so nested literals get filled before we patch this one.
    match &mut e.kind {
        ExprKind::Await { expr } | ExprKind::Is { value: expr, .. } => {
            fill_in_expr(expr, defs, filling)?
        }
        ExprKind::Call { callee, args, kwargs } => {
            fill_in_expr(callee, defs, filling)?;
            for a in args.iter_mut() {
                fill_in_expr(a, defs, filling)?;
            }
            for (_, a) in kwargs.iter_mut() {
                fill_in_expr(a, defs, filling)?;
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            fill_in_expr(left, defs, filling)?;
            fill_in_expr(right, defs, filling)?;
        }
        ExprKind::UnaryOp { operand, .. } => fill_in_expr(operand, defs, filling)?,
        ExprKind::FieldAccess { object, .. } => fill_in_expr(object, defs, filling)?,
        ExprKind::Index { object, index } => {
            fill_in_expr(object, defs, filling)?;
            fill_in_expr(index, defs, filling)?;
        }
        ExprKind::Array { elements } => {
            for el in elements.iter_mut() {
                fill_in_expr(el, defs, filling)?;
            }
        }
        ExprKind::FStr { parts } => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(ex) = p {
                    fill_in_expr(ex, defs, filling)?;
                }
            }
        }
        ExprKind::Dict { entries } => {
            for (k, v) in entries.iter_mut() {
                fill_in_expr(k, defs, filling)?;
                fill_in_expr(v, defs, filling)?;
            }
        }
        ExprKind::PromptDeref { expr, grammar_expr, .. } => {
            fill_in_expr(expr, defs, filling)?;
            if let Some(g) = grammar_expr {
                fill_in_expr(g, defs, filling)?;
            }
        }
        ExprKind::PromptLiteral { body } => fill_in_expr(body, defs, filling)?,
        ExprKind::Pipe { source, hoisted, body, .. } => {
            fill_in_expr(source, defs, filling)?;
            for (_, v) in hoisted.iter_mut() {
                fill_in_expr(v, defs, filling)?;
            }
            fill_in_expr(body, defs, filling)?;
        }
        ExprKind::RawPipe { value, stage } => {
            fill_in_expr(value, defs, filling)?;
            fill_in_expr(stage, defs, filling)?;
        }
        ExprKind::StructLiteral { type_name, base, fields, .. } => {
            if let Some(b) = base.as_mut() {
                fill_in_expr(b, defs, filling)?;
            }
            for (_, fe, _) in fields.iter_mut() {
                fill_in_expr(fe, defs, filling)?;
            }
            // Patch missing fields with their declared defaults — unless a
            // `...base` is there, in which case the checker and MIR read each
            // missing field from the base, or take its default when the base's
            // class does not have it.
            if let Some(def_fields) = defs.get(type_name.as_str()).filter(|_| base.is_none()) {
                for def_field in def_fields {
                    if fields.iter().any(|(n, _, _)| n == def_field.name()) {
                        continue;
                    }
                    let (name, default, is_prompt) = match def_field {
                        // Left out: the class member rules have already
                        // refused a literal that omits a required field.
                        StructFieldDef::Required(_) => continue,
                        StructFieldDef::Let { name, default } => (name, default, false),
                        StructFieldDef::Prompt { name, default } => (name, default, true),
                    };
                    // The class's own default, which the per-file pass of the
                    // file that defines it already rewrote and the import pass
                    // renamed. A literal inside it may need defaults of its own.
                    enter_fill(filling, type_name, name, span)?;
                    let mut te = default.clone();
                    let filled = fill_in_expr(&mut te, defs, filling);
                    filling.pop();
                    filled?;
                    fields.push((name.clone(), te, is_prompt));
                }
            }
        }
        ExprKind::Integer(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Identifier(_) => {}
    }
    Ok(())
}

/// Purpose: This function notes that a class's default is about to be filled
/// in, refusing a default that would build its own class again without end.
///
/// Inputs:
/// - `filling`: the classes whose defaults are being filled, innermost last.
/// - `class`: the class.
/// - `field`: the field whose default it is.
/// - `span`: the literal, for the message.
///
/// Outputs: `Ok(())` with `class` pushed, which the caller pops, or the error.
fn enter_fill(filling: &mut Vec<String>, class: &str, field: &str, span: Span) -> Result<()> {
    if filling.iter().any(|c| c == class) {
        let c = crate::typechecker::infer::display_type_name(class).to_string();
        return Err(JadeError::TypeError {
            message: format!(
                "the default of {c}'s field '{field}' builds another {c}, whose default builds \
                 another, without end; give '{field}' a default that does not build {c}, such \
                 as nil, or set it in every literal"
            ),
            span,
        });
    }
    filling.push(class.to_string());
    Ok(())
}

// ── Classes ───────────────────────────────────────────────────────────────────

/// Fold every parent's fields into its children, and work out each class's
/// full ancestry.
///
/// Runs once, after `pre_pass` has seen every class in the file and before any
/// body is rewritten. Flattening here is what keeps inheritance out of the rest
/// of the toolchain: by the time MIR runs, a child's field list already holds
/// everything it inherited, so building a value and finding a field treat a
/// class as a flat list of fields and never walk a chain.
///
/// Four things are refused, and the first three are the user's rules:
///
///  * a parent that is not a class in scope,
///  * a field name that appears twice across a class and all it inherits,
///  * two *parents* supplying one method name,
///  * a class that inherits itself, however long the way round.
///
/// A *child's* method overriding a parent's is not a clash: the child is
/// nearer, so there is something to decide between them. Two parents are the
/// same distance away, so there is not. Fields have no equivalent notion of
/// nearer, since two fields with one name means two storage slots and no way to
/// say which a literal meant, which is why a child redeclaring an inherited
/// field is refused where a child redeclaring a method is not.
///
/// A parent that cannot be found here is only an error when the file has no
/// opaque imports. With `opaque_imports` set, an imported class is deliberately
/// absent from `struct_defs`, so the name is left for the merge to resolve.
fn resolve_inheritance(ctx: &mut Names) -> Result<()> {
    let names: Vec<String> = ctx.struct_parents.keys().cloned().collect();
    let mut resolved: HashMap<String, Vec<StructFieldDef>> = HashMap::new();
    let mut ancestors: HashMap<String, Vec<String>> = HashMap::new();
    for name in &names {
        let mut seen = Vec::new();
        flatten_struct(name, ctx, &mut resolved, &mut ancestors, &mut seen)?;
    }
    for (name, fields) in resolved {
        ctx.struct_defs.insert(name, fields);
    }
    // Methods fold in the same way, except that a child's own entry stands:
    // overriding an inherited method is the point of inheriting one.
    for (name, chain) in &ancestors {
        let mut merged = ctx.extend_methods.get(name).cloned().unwrap_or_default();
        for ancestor in chain {
            for method in ctx.extend_methods.get(ancestor).into_iter().flatten() {
                merged.insert(method.clone());
            }
        }
        if !merged.is_empty() {
            ctx.extend_methods.insert(name.clone(), merged);
        }
    }
    ctx.struct_ancestors = ancestors;
    Ok(())
}

/// Flatten one class, recursing into its parents first.
///
/// `chain` is the path taken to get here, which is both the cycle check and the
/// text of the error when one is found: a reader needs the route, not just the
/// fact that there was one.
fn flatten_struct(
    name: &str,
    ctx: &Names,
    resolved: &mut HashMap<String, Vec<StructFieldDef>>,
    ancestors: &mut HashMap<String, Vec<String>>,
    chain: &mut Vec<String>,
) -> Result<()> {
    if resolved.contains_key(name) {
        return Ok(());
    }
    let span = ctx.struct_spans.get(name).copied().unwrap_or(Span { line: 0, col: 0 });
    if let Some(at) = chain.iter().position(|c| c == name) {
        let mut cycle: Vec<String> = chain[at..].to_vec();
        cycle.push(name.to_string());
        return Err(JadeError::InheritanceCycle { chain: cycle, span });
    }
    let Some(own) = ctx.struct_defs.get(name).cloned() else {
        return Ok(());
    };
    let parents = ctx.struct_parents.get(name).cloned().unwrap_or_default();

    chain.push(name.to_string());
    for parent in &parents {
        if !ctx.struct_defs.contains_key(parent) {
            // An imported parent is not in this file's map by design; leave it
            // for the merge. Anything else is a real mistake.
            if ctx.opaque_imports {
                continue;
            }
            chain.pop();
            return Err(JadeError::UnknownParent {
                child: name.to_string(),
                parent: parent.clone(),
                span,
            });
        }
        flatten_struct(parent, ctx, resolved, ancestors, chain)?;
    }
    chain.pop();

    // Fields, parents first and in written order, then the class's own. Every
    // name is checked against where it already came from, so the message can
    // name both sides.
    let mut fields: Vec<StructFieldDef> = Vec::new();
    // Each field's name, with the class that declared it and the parent it
    // came through. A class reached by two paths, the shared ancestor of a
    // diamond, declares its fields once, so the second path adds nothing.
    let mut owner: HashMap<String, (String, String)> = HashMap::new();
    // The class that declared a field a parent passes on: the nearest of the
    // parent and its ancestors whose own body names it.
    let declarer = |field: &str, parent: &str| -> String {
        std::iter::once(parent.to_string())
            .chain(ancestors.get(parent).cloned().unwrap_or_default())
            .find(|c| {
                ctx.struct_defs.get(c).is_some_and(|own| own.iter().any(|f| f.name() == field))
            })
            .unwrap_or_else(|| parent.to_string())
    };
    let push = |f: &StructFieldDef,
                decl: String,
                from: &str,
                fields: &mut Vec<StructFieldDef>,
                owner: &mut HashMap<String, (String, String)>|
     -> Result<()> {
        if let Some((first_decl, first_from)) = owner.get(f.name()) {
            if *first_decl == decl {
                return Ok(());
            }
            return Err(JadeError::InheritedFieldClash {
                field: f.name().to_string(),
                owner: first_from.clone(),
                other: from.to_string(),
                span,
            });
        }
        owner.insert(f.name().to_string(), (decl, from.to_string()));
        fields.push(f.clone());
        Ok(())
    };
    for parent in &parents {
        let inherited = resolved.get(parent).cloned().unwrap_or_default();
        for f in &inherited {
            push(f, declarer(f.name(), parent), parent, &mut fields, &mut owner)?;
        }
    }
    for f in &own {
        push(f, name.to_string(), name, &mut fields, &mut owner)?;
    }

    // Two parents supplying one method name have nothing to decide between
    // them, unless both supply the same method, inherited from an ancestor
    // they share. The class settles it by defining the method itself, as in
    // C++; then neither parent's version is the one it runs.
    let defines_itself =
        |method: &str| ctx.extend_methods.get(name).is_some_and(|m| m.contains(method));
    // Each method a parent supplies, with the class whose version it is.
    let mut from_parent: HashMap<String, (String, String)> = HashMap::new();
    for parent in &parents {
        let line: Vec<String> = std::iter::once(parent.clone())
            .chain(ancestors.get(parent).cloned().unwrap_or_default())
            .collect();
        let mut supplied: Vec<(String, String)> = Vec::new();
        for class in &line {
            for method in ctx
                .extend_methods
                .get(class)
                .map(|m| m.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if !supplied.iter().any(|(m, _)| *m == method) {
                    supplied.push((method, class.clone()));
                }
            }
        }
        supplied.sort();
        for (method, version) in supplied {
            if let Some((first_parent, first_version)) = from_parent.get(&method)
                && first_parent != parent
                && *first_version != version
                && !defines_itself(&method)
            {
                return Err(JadeError::InheritedMethodClash {
                    method,
                    first: first_parent.clone(),
                    second: parent.clone(),
                    span,
                });
            }
            from_parent.entry(method).or_insert((parent.clone(), version));
        }
    }

    // Nearest first: each parent, then what that parent inherited.
    //
    // Membership goes through a set rather than a scan of the list being built.
    // Each parent's own list is already finished, so the work here is linear in
    // what is being copied; scanning instead made it quadratic in the depth, and
    // that squared again over a whole file. A chain of 800 structs took six
    // minutes to check.
    let mut chainlist: Vec<String> = Vec::new();
    let mut in_chain: HashSet<String> = HashSet::new();
    for parent in &parents {
        if in_chain.insert(parent.clone()) {
            chainlist.push(parent.clone());
        }
        for a in ancestors.get(parent).into_iter().flatten() {
            if in_chain.insert(a.clone()) {
                chainlist.push(a.clone());
            }
        }
    }

    resolved.insert(name.to_string(), fields);
    ancestors.insert(name.to_string(), chainlist);
    Ok(())
}

/// Register top-level names without descending into bodies, so a function
/// called before its definition, or a class used before its own, resolves.
fn pre_pass(stmts: &[Stmt], ctx: &mut Names) {
    for stmt in stmts {
        match stmt {
            Stmt::FnDef { name, body, .. } => {
                let kind = if body_yields(body) { NameKind::Generator } else { NameKind::Function };
                ctx.define(name.clone(), kind);
            }
            Stmt::AsyncFnDef { name, .. } => {
                ctx.define(name.clone(), NameKind::AsyncFunction);
            }
            Stmt::StructDef { name, fields, parents, members, span } => {
                // A declared method is a method here even before anything
                // defines it; the whole-program checker reports a missing
                // definition where it is called.
                for d in members.declared.iter().filter(|d| *d != "new") {
                    ctx.extend_methods.entry(name.clone()).or_default().insert(d.clone());
                }
                ctx.struct_members.insert(name.clone(), members.clone());
                ctx.struct_defs.insert(name.clone(), fields.clone());
                ctx.struct_parents.insert(name.clone(), parents.clone());
                ctx.struct_spans.insert(name.clone(), *span);
                ctx.define(name.clone(), NameKind::Class);
            }
            Stmt::ExtendBlock { type_name, methods, .. } => {
                for method in methods {
                    if let Stmt::FnDef { name: mname, .. } = method {
                        ctx.extend_methods
                            .entry(type_name.clone())
                            .or_default()
                            .insert(mname.clone());
                    }
                }
            }
            Stmt::Use { path, .. } => {
                ctx.opaque_imports |= import_is_opaque(path);
            }
            Stmt::FromUse { .. } => {
                ctx.opaque_imports = true;
            }
            _ => {}
        }
    }
}

/// Does `use <path>` bring in names this pass cannot see?
///
/// Only a non-std path does. A std package contributes its own name, which
/// this pass defines when it meets the `use`, so `use std::fs` leaves nothing
/// unaccounted for. Anything else — a sibling `.jde`, a subdir module, a
/// `[lib]` or a dependency — contributes top-level names that only exist once
/// the importer merges the modules, well after this runs.
fn import_is_opaque(path: &str) -> bool {
    builtins::find_package(path).is_none()
}

// ── Statements ────────────────────────────────────────────────────────────────

/// A function's parameters: each name, with its default when it has one.
type Params = Vec<(String, Option<Expr>)>;

fn resolve_stmts(stmts: Vec<Stmt>, ctx: &mut Names) -> Result<Vec<Stmt>> {
    stmts.into_iter().map(|s| resolve_stmt(s, ctx)).collect()
}

/// Purpose: This function rewrites one function's parameters, body and
/// decorators, binding the parameters inside the body.
///
/// Outputs: the parameters, body and decorators, rewritten.
fn resolve_function(
    params: Params,
    body: Vec<Stmt>,
    decorators: DecoratorList,
    ctx: &mut Names,
) -> Result<(Params, Vec<Stmt>, DecoratorList)> {
    ctx.push_scope();
    for (pname, _) in &params {
        ctx.define(pname.clone(), NameKind::Value);
    }
    let body = resolve_stmts(body, ctx);
    ctx.pop_scope();
    let body = body?;
    if body_yields(&body)
        && let Some(rspan) = body_returns_a_value(&body)
    {
        return Err(JadeError::YieldAndReturn { span: rspan });
    }
    let params = params
        .into_iter()
        .map(|(pname, default)| Ok((pname, default.map(|e| resolve_expr(e, ctx)).transpose()?)))
        .collect::<Result<Vec<_>>>()?;
    let decorators = resolve_decorators(decorators, ctx)?;
    Ok((params, body, decorators))
}

fn resolve_stmt(stmt: Stmt, ctx: &mut Names) -> Result<Stmt> {
    Ok(match stmt {
        // ── Variable bindings ─────────────────────────────────────────────────
        Stmt::Let { name, value, span } => {
            let value = resolve_expr(value, ctx)?;
            ctx.define(name.clone(), ctx.kind_of(&value));
            Stmt::Let { name, value, span }
        }

        Stmt::Assign { name, value, span } => {
            let value = resolve_expr(value, ctx)?;
            ctx.assign(&name, ctx.kind_of(&value));
            Stmt::Assign { name, value, span }
        }

        // ── Functions ─────────────────────────────────────────────────────────
        Stmt::FnDef { name, params, body, decorators, span } => {
            let (params, body, decorators) = resolve_function(params, body, decorators, ctx)?;
            let kind = if body_yields(&body) { NameKind::Generator } else { NameKind::Function };
            ctx.define(name.clone(), kind);
            Stmt::FnDef { name, params, body, decorators, span }
        }

        Stmt::AsyncFnDef { name, params, body, decorators, span } => {
            let (params, body, decorators) = resolve_function(params, body, decorators, ctx)?;
            ctx.define(name.clone(), NameKind::AsyncFunction);
            Stmt::AsyncFnDef { name, params, body, decorators, span }
        }

        Stmt::Yield { value, span } => Stmt::Yield { value: resolve_expr(value, ctx)?, span },

        Stmt::Return { value, span } => {
            Stmt::Return { value: value.map(|e| resolve_expr(e, ctx)).transpose()?, span }
        }

        // ── Control flow ──────────────────────────────────────────────────────
        Stmt::If { condition, then_body, else_body, span } => {
            let condition = resolve_expr(condition, ctx)?;
            let then_body = resolve_block(then_body, ctx)?;
            let else_body = else_body.map(|b| resolve_block(b, ctx)).transpose()?;
            Stmt::If { condition, then_body, else_body, span }
        }

        Stmt::While { condition, body, span } => {
            let condition = resolve_expr(condition, ctx)?;
            Stmt::While { condition, body: resolve_block(body, ctx)?, span }
        }

        Stmt::For { var, iterable, body, span } => {
            let iterable = resolve_expr(iterable, ctx)?;
            ctx.push_scope();
            ctx.define(var.clone(), NameKind::Value);
            let body = resolve_stmts(body, ctx);
            ctx.pop_block_scope();
            Stmt::For { var, iterable, body: body?, span }
        }

        // Control flow only: nothing to rewrite, and the parser has already
        // refused either one that has no loop to act on.
        Stmt::Break { span } => Stmt::Break { span },
        Stmt::Continue { span } => Stmt::Continue { span },

        // ── Type definitions ──────────────────────────────────────────────────
        // The *flattened* field list, not the written one: `resolve_inheritance`
        // folded every parent's fields in already. Each default is rewritten
        // here as well, so no later stage meets a `RawPipe` in one, with a name
        // it cannot see yet left for the checker: a default may name a global
        // defined further down, and is only read where a literal leaves its
        // field out.
        Stmt::StructDef { name, fields, parents, members, span } => {
            let fields = ctx.struct_defs.get(&name).cloned().unwrap_or(fields);
            let saved = ctx.opaque_imports;
            ctx.opaque_imports = true;
            ctx.filling.push(name.clone());
            let fields = fields
                .into_iter()
                .map(|f| {
                    Ok(match f {
                        StructFieldDef::Required(n) => StructFieldDef::Required(n),
                        StructFieldDef::Let { name, default } => {
                            StructFieldDef::Let { name, default: resolve_expr(default, ctx)? }
                        }
                        StructFieldDef::Prompt { name, default } => {
                            StructFieldDef::Prompt { name, default: resolve_expr(default, ctx)? }
                        }
                    })
                })
                .collect::<Result<Vec<_>>>();
            ctx.filling.pop();
            ctx.opaque_imports = saved;
            Stmt::StructDef { name, fields: fields?, parents, members, span }
        }

        Stmt::ExtendBlock { type_name, methods, decorators, outside, span } => {
            // Each method is a FnDef, which binds its own parameters, `self`
            // first, in a scope of its own.
            let methods = resolve_stmts(methods, ctx)?;
            let decorators = resolve_decorators(decorators, ctx)?;
            Stmt::ExtendBlock { type_name, methods, decorators, outside, span }
        }

        // ── Mutations ─────────────────────────────────────────────────────────
        Stmt::FieldAssign { object, field, value, span } => {
            Stmt::FieldAssign { object, field, value: resolve_expr(value, ctx)?, span }
        }

        Stmt::IndexAssign { name, index, value, span } => {
            let index = resolve_expr(index, ctx)?;
            let value = resolve_expr(value, ctx)?;
            Stmt::IndexAssign { name, index, value, span }
        }

        // ── Prompts ───────────────────────────────────────────────────────────
        Stmt::PromptDecl { name, body, span } => {
            let body = resolve_expr(body, ctx)?;
            ctx.define(name.clone(), NameKind::Prompt);
            Stmt::PromptDecl { name, body, span }
        }

        // ── Imports ───────────────────────────────────────────────────────────
        Stmt::Use { path, as_name, path_is_string, span } => {
            ctx.opaque_imports |= import_is_opaque(&path);
            // One unified import form: `::` notation (or a bare name) that names a
            // module — a std package, a sibling `.jde` file, a subdir module
            // (`sub::name`), or a registered `[lib]`/dependency. Quoted file paths
            // and the `as` alias were removed.
            if path_is_string {
                return Err(JadeError::QuotedImport { path, span });
            }
            if as_name.is_some() {
                return Err(JadeError::ImportAlias { span });
            }
            if let Some(pkg) = builtins::find_package(&path) {
                // After `define`, which would otherwise clear the association.
                ctx.define(pkg.global_name.to_string(), NameKind::Package);
                ctx.imported_packages.insert(pkg.global_name.to_string());
            }
            Stmt::Use { path, as_name: None, path_is_string: false, span }
        }

        Stmt::FromUse { path, names, path_is_string, span } => {
            ctx.opaque_imports = true;
            if path_is_string {
                return Err(JadeError::QuotedImport { path, span });
            }
            if let Some(pkg) = builtins::find_package(&path) {
                ctx.define(pkg.global_name.to_string(), NameKind::Package);
            }
            Stmt::FromUse { path, names, path_is_string: false, span }
        }

        // ── Exception handling ────────────────────────────────────────────────
        Stmt::Raise { value, span } => Stmt::Raise { value: resolve_expr(value, ctx)?, span },

        Stmt::TryCatch { body, arms, span } => {
            let body = resolve_block(body, ctx)?;
            let arms = arms
                .into_iter()
                .map(|arm| {
                    ctx.push_scope();
                    ctx.define(arm.binding.clone(), NameKind::Value);
                    let body = resolve_stmts(arm.body, ctx);
                    ctx.pop_block_scope();
                    Ok(CatchArm { catch_type: arm.catch_type, binding: arm.binding, body: body? })
                })
                .collect::<Result<Vec<_>>>()?;
            Stmt::TryCatch { body, arms, span }
        }

        // ── Bare expression ───────────────────────────────────────────────────
        Stmt::Expr(expr) => Stmt::Expr(resolve_expr(expr, ctx)?),
    })
}

/// Purpose: This function rewrites a block in a scope of its own, carrying a
/// rebinding of an outer name back out.
fn resolve_block(body: Vec<Stmt>, ctx: &mut Names) -> Result<Vec<Stmt>> {
    ctx.push_scope();
    let out = resolve_stmts(body, ctx);
    ctx.pop_block_scope();
    out
}

/// Purpose: This function rewrites a definition's decorator arguments.
fn resolve_decorators(decorators: DecoratorList, ctx: &mut Names) -> Result<DecoratorList> {
    decorators
        .into_iter()
        .map(|(name, args)| {
            let args = args
                .into_iter()
                .map(|(kw, a)| Ok((kw, resolve_expr(a, ctx)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok((name, args))
        })
        .collect()
}

/// Whether a body yields, at any depth in this function. A nested definition
/// is the parser's error, so none is looked into.
fn body_yields(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::Yield { .. } => true,
        Stmt::If { then_body, else_body, .. } => {
            body_yields(then_body) || else_body.as_deref().is_some_and(body_yields)
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => body_yields(body),
        Stmt::TryCatch { body, arms, .. } => {
            body_yields(body) || arms.iter().any(|a| body_yields(&a.body))
        }
        _ => false,
    })
}

/// Whether a body has a `return` carrying a value, at any depth in this
/// function. A bare `return` is fine in a generator — it stops early — but a
/// value-returning one asks the function to be two things at once.
fn body_returns_a_value(stmts: &[Stmt]) -> Option<Span> {
    for stmt in stmts {
        let found = match stmt {
            Stmt::Return { value: Some(_), span } => Some(*span),
            Stmt::If { then_body, else_body, .. } => body_returns_a_value(then_body)
                .or_else(|| else_body.as_deref().and_then(body_returns_a_value)),
            Stmt::While { body, .. } | Stmt::For { body, .. } => body_returns_a_value(body),
            Stmt::TryCatch { body, arms, .. } => body_returns_a_value(body)
                .or_else(|| arms.iter().find_map(|a| body_returns_a_value(&a.body))),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

// ── Expressions ───────────────────────────────────────────────────────────────

fn boxed(e: Expr, ctx: &mut Names) -> Result<Box<Expr>> {
    Ok(Box::new(resolve_expr(e, ctx)?))
}

fn resolve_expr(expr: Expr, ctx: &mut Names) -> Result<Expr> {
    let span = expr.span;
    let kind = match expr.kind {
        // ── Class test ────────────────────────────────────────────────────────
        // Whether the class is related to the value's is the whole-program
        // checker's question; a class this file does not know may be imported.
        ExprKind::Is { value, class } => {
            if !class.contains('.') && !ctx.struct_defs.contains_key(&class) && !ctx.opaque_imports
            {
                return Err(JadeError::UndefinedType { name: class, span });
            }
            let value = boxed(*value, ctx)?;
            let class = if ctx.struct_defs.contains_key(&class) {
                class
            } else {
                class.rsplit_once('.').map(|(_, b)| b.to_string()).unwrap_or(class)
            };
            ExprKind::Is { value, class }
        }

        // ── Literals ──────────────────────────────────────────────────────────
        k @ (ExprKind::Integer(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_)) => k,

        // ── Names ─────────────────────────────────────────────────────────────
        ExprKind::Identifier(name) => {
            // `nil`, `None`, and `null` are literal values; they need no definition.
            if !matches!(name.as_str(), "nil" | "None" | "null")
                && ctx.get(&name).is_none()
                && !ctx.opaque_imports
            {
                return Err(JadeError::UndefinedVariable { name, span });
            }
            ExprKind::Identifier(name)
        }

        // ── Interpolated strings ──────────────────────────────────────────────
        ExprKind::FStr { parts } => ExprKind::FStr {
            parts: parts
                .into_iter()
                .map(|p| {
                    Ok(match p {
                        FStrPart::Literal(s) => FStrPart::Literal(s),
                        FStrPart::Expr(e) => FStrPart::Expr(resolve_expr(e, ctx)?),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        },

        // ── Operators ─────────────────────────────────────────────────────────
        // Which operand types an operator takes is the whole-program checker's
        // to say, once every type is known.
        ExprKind::BinOp { op, left, right } => {
            ExprKind::BinOp { op, left: boxed(*left, ctx)?, right: boxed(*right, ctx)? }
        }
        ExprKind::UnaryOp { op, operand } => {
            ExprKind::UnaryOp { op, operand: boxed(*operand, ctx)? }
        }

        // ── Calls ─────────────────────────────────────────────────────────────
        ExprKind::Call { callee, args, kwargs } => {
            return resolve_call(*callee, args, kwargs, span, ctx);
        }

        // ── Class literals ────────────────────────────────────────────────────
        ExprKind::StructLiteral { type_name, base, fields, .. } => {
            return resolve_struct_literal(type_name, base, fields, span, ctx);
        }

        // ── Field access ──────────────────────────────────────────────────────
        ExprKind::FieldAccess { object, field } => {
            // `math.round` — a std package's contents are fixed and known now,
            // so a name it does not have is settled here.
            if let ExprKind::Identifier(name) = &object.kind
                && ctx.imported_packages.contains(name)
                && let Some(pkg) = builtins::package_by_global_name(name)
                && !pkg.has_fn(&field)
            {
                return Err(JadeError::UnknownPackageFn {
                    package: pkg.import_name.replace('/', "::"),
                    name: field,
                    available: pkg.fns.iter().map(|f| f.to_string()).collect(),
                    span,
                });
            }
            ExprKind::FieldAccess { object: boxed(*object, ctx)?, field }
        }

        ExprKind::Index { object, index } => {
            ExprKind::Index { object: boxed(*object, ctx)?, index: boxed(*index, ctx)? }
        }

        ExprKind::Array { elements } => ExprKind::Array {
            elements: elements.into_iter().map(|e| resolve_expr(e, ctx)).collect::<Result<_>>()?,
        },

        ExprKind::Dict { entries } => ExprKind::Dict {
            entries: entries
                .into_iter()
                .map(|(k, v)| Ok((resolve_expr(k, ctx)?, resolve_expr(v, ctx)?)))
                .collect::<Result<_>>()?,
        },

        ExprKind::Await { expr } => ExprKind::Await { expr: boxed(*expr, ctx)? },

        ExprKind::PromptLiteral { body } => ExprKind::PromptLiteral { body: boxed(*body, ctx)? },

        // ── Prompt dereference ────────────────────────────────────────────────
        ExprKind::PromptDeref { expr, output_type, grammar_expr, style } => ExprKind::PromptDeref {
            expr: boxed(*expr, ctx)?,
            output_type,
            grammar_expr: grammar_expr.map(|g| boxed(*g, ctx)).transpose()?,
            style,
        },

        // ── Pipes ─────────────────────────────────────────────────────────────
        ExprKind::RawPipe { value, stage } => return resolve_pipe(*value, *stage, span, ctx),

        // A pipe already flattened, which a tree this pass has rewritten once
        // holds; its piece and hoisted names are bound inside its stages.
        ExprKind::Pipe { source, hoisted, piece, body } => {
            let source = boxed(*source, ctx)?;
            let hoisted = hoisted
                .into_iter()
                .map(|(n, v)| Ok((n, resolve_expr(v, ctx)?)))
                .collect::<Result<Vec<_>>>()?;
            ctx.push_scope();
            for (n, _) in &hoisted {
                ctx.define(n.clone(), NameKind::Value);
            }
            ctx.define(piece.clone(), NameKind::Value);
            let body = boxed(*body, ctx);
            ctx.pop_scope();
            ExprKind::Pipe { source, hoisted, piece, body: body? }
        }
    };
    Ok(Expr::new(kind, span))
}

/// Purpose: This function rewrites a call: a converter, class or grammar
/// around a whole dereference folds into it, a class applied to one value is
/// a view, and `Parent::m(…)` inside a method gets `self` as its first
/// argument.
///
/// Inputs:
/// - `callee`, `args`, `kwargs`: the call's parts.
/// - `span`: the call.
/// - `ctx`: the name table.
///
/// Outputs: the rewritten call, or a typed dereference.
fn resolve_call(
    callee: Expr,
    mut args: Vec<Expr>,
    kwargs: Vec<(String, Expr)>,
    span: Span,
    ctx: &mut Names,
) -> Result<Expr> {
    // A converter, class or grammar called on a whole dereference asks the
    // model for that shape: `toInt(?p)` asks for an int, and `City(?p)` for a
    // City. Piped, `?p |> f` runs per piece instead.
    if kwargs.is_empty()
        && args.len() == 1
        && matches!(args[0].kind, ExprKind::PromptDeref { .. })
        && let Some(c) = deref_constraint(&callee, ctx)?
        && let Some(arg) = args.pop()
        && let ExprKind::PromptDeref { expr: inner, style, .. } = arg.kind
    {
        let (output_type, grammar_expr) = match c {
            DerefStage::Type(name) => (Some(name), None),
            DerefStage::Grammar(g) => (None, Some(g)),
        };
        return Ok(Expr::new(
            ExprKind::PromptDeref { expr: boxed(*inner, ctx)?, output_type, grammar_expr, style },
            span,
        ));
    }

    // A class applied to one value views the value as that class:
    // `Animal(d)`, which is also what `@Animal let a = d` means. The
    // whole-program checker requires the value to be the class or one of its
    // descendants. Called any other way, a class is refused here so `jade
    // check`, `jade run` and `jade build` all agree.
    if let ExprKind::Identifier(name) = &callee.kind
        && ctx.struct_defs.contains_key(name)
    {
        if args.len() == 1 && kwargs.is_empty() {
            let arg = args.into_iter().map(|a| resolve_expr(a, ctx)).collect::<Result<_>>()?;
            return Ok(Expr::new(
                ExprKind::Call { callee: Box::new(callee), args: arg, kwargs: Vec::new() },
                span,
            ));
        }
        return Err(JadeError::TypeError {
            message: format!(
                "'{name}' is a class, not a function; build one with {name} {{ … }} or \
                 {name}::new(…), or view a value as the class with {name}(x)"
            ),
            span,
        });
    }

    // `Animal::speak(…)` inside a method calls Animal's version on the same
    // object, so the object goes first, as it does for any method.
    let named = match &callee.kind {
        ExprKind::Identifier(name) | ExprKind::FieldAccess { field: name, .. } => Some(name),
        _ => None,
    };
    let parent_call = named.and_then(|n| n.split_once("::")).is_some_and(|(_, m)| m != "new");
    if parent_call && ctx.get("self").is_none() {
        let named = named.cloned().unwrap_or_default();
        let (class, method) = named.split_once("::").unwrap_or((named.as_str(), ""));
        return Err(JadeError::TypeError {
            message: format!(
                "{class}::{method}(…) calls {class}'s version of a method on the same object, \
                 so it works only inside a method of {class} or of a class that inherits from it"
            ),
            span,
        });
    }
    if parent_call {
        args.insert(0, Expr::new(ExprKind::Identifier("self".to_string()), span));
    }

    // A parent call's `Animal::speak` names a method, not a variable, so it is
    // not looked up here; the whole-program checker resolves it.
    let callee = match callee.kind {
        ExprKind::Identifier(_) if parent_call => callee,
        _ => resolve_expr(callee, ctx)?,
    };
    let args = args.into_iter().map(|a| resolve_expr(a, ctx)).collect::<Result<_>>()?;
    let kwargs =
        kwargs.into_iter().map(|(k, v)| Ok((k, resolve_expr(v, ctx)?))).collect::<Result<_>>()?;
    Ok(Expr::new(ExprKind::Call { callee: Box::new(callee), args, kwargs }, span))
}

/// Purpose: This function rewrites a class literal: for a class this file
/// defines, it checks the fields named, puts them in definition order, marks
/// prompt fields and, with no base, fills in each default left out.
///
/// Inputs:
/// - `type_name`, `base`, `fields`: the literal as the parser built it.
/// - `span`: the literal.
/// - `ctx`: the name table.
///
/// Outputs: the rewritten literal, or the first error.
fn resolve_struct_literal(
    type_name: String,
    base: Option<Box<Expr>>,
    fields: Vec<(String, Expr, bool)>,
    span: Span,
    ctx: &mut Names,
) -> Result<Expr> {
    let given: Vec<String> = fields.iter().map(|(n, _, _)| n.clone()).collect();
    // The base is rewritten once, here, and carried through. It is never
    // duplicated per field.
    let base = base.map(|b| boxed(*b, ctx)).transpose()?;
    let literal = |type_name: String, base, fields, given| {
        Expr::new(ExprKind::StructLiteral { type_name, base, fields, given }, span)
    };

    let Some(def_fields) = ctx.struct_defs.get(&type_name).cloned() else {
        if !ctx.opaque_imports {
            return Err(JadeError::UndefinedType { name: type_name, span });
        }
        // The class may be defined in an imported file; its fields are checked
        // and filled in once the merge brings it in. The name is made bare
        // ("messages.Session" → "Session") so the literal agrees with the
        // class the merge registers, and with the method bodies that name it.
        let bare =
            type_name.rsplit_once('.').map(|(_, b)| b.to_string()).unwrap_or(type_name.clone());
        let fields = fields
            .into_iter()
            .map(|(n, e, _)| Ok((n, resolve_expr(e, ctx)?, false)))
            .collect::<Result<Vec<_>>>()?;
        return Ok(literal(bare, base, fields, given));
    };

    // A class whose parent came from another file is not fully known here:
    // `resolve_inheritance` had nothing to fold in. Its literal is checked and
    // filled at the merge instead, the same treatment an imported class gets.
    let awaits_a_parent = ctx
        .struct_parents
        .get(&type_name)
        .is_some_and(|ps| ps.iter().any(|p| !ctx.struct_defs.contains_key(p)));
    if awaits_a_parent {
        let fields = fields
            .into_iter()
            .map(|(n, e, _)| Ok((n, resolve_expr(e, ctx)?, false)))
            .collect::<Result<Vec<_>>>()?;
        return Ok(literal(type_name, base, fields, given));
    }

    // Extra fields.
    for (fname, fexpr, _) in &fields {
        if !def_fields.iter().any(|f| f.name() == fname) {
            return Err(JadeError::UndefinedField {
                type_name,
                field: fname.clone(),
                owner: FieldOwner::Struct,
                span: fexpr.span,
            });
        }
    }

    // Required fields. With a base, a field the literal leaves out is read
    // from it, and whether the base's class has it is the whole-program
    // checker's question. A private field is left to
    // `typechecker::infer::members`, which knows whether the literal is inside
    // the class and can name a constructor in its message.
    for def_field in &def_fields {
        if let StructFieldDef::Required(req) = def_field
            && !given.iter().any(|n| n == req)
            && base.is_none()
            && !field_is_private(ctx, &type_name, req)
        {
            return Err(JadeError::MissingField { field: req.clone(), span });
        }
    }

    // Duplicate fields.
    let mut seen = HashSet::new();
    for (fname, fexpr, _) in &fields {
        if !seen.insert(fname.as_str()) {
            return Err(JadeError::DuplicateField { field: fname.clone(), span: fexpr.span });
        }
    }

    // Walk definition order and produce the field list, defaults filled in
    // where there is no base. The `bool` marks a prompt field, whose value is
    // the prompt's text.
    let mut provided: HashMap<String, Expr> = fields.into_iter().map(|(n, e, _)| (n, e)).collect();
    let mut out: Vec<(String, Expr, bool)> = Vec::with_capacity(def_fields.len());
    for def_field in &def_fields {
        let name = def_field.name();
        let is_prompt = matches!(def_field, StructFieldDef::Prompt { .. });
        if let Some(e) = provided.remove(name) {
            out.push((name.to_string(), resolve_expr(e, ctx)?, is_prompt));
            continue;
        }
        // A field the literal leaves out comes from the base when there is
        // one: from the base itself when its class has the field, from the
        // field's default when it does not, decided where the base's class is
        // known. A default is evaluated only when it is needed, which is the
        // rule with no base too: `S { name: "c", id: 99 }` never runs the `id`
        // default.
        if base.is_some() {
            continue;
        }
        let default = match def_field {
            StructFieldDef::Required(_) => continue,
            StructFieldDef::Let { default, .. } | StructFieldDef::Prompt { default, .. } => default,
        };
        enter_fill(&mut ctx.filling, &type_name, name, span)?;
        let filled = resolve_expr(default.clone(), ctx);
        ctx.filling.pop();
        out.push((name.to_string(), filled?, is_prompt));
    }

    Ok(literal(type_name, base, out, given))
}

/// What a call around a whole prompt dereference asks the model for.
enum DerefStage {
    /// A type name: ask for that type and read the reply as one.
    Type(String),
    /// A Grammar value: constrain sampling with it directly.
    Grammar(Box<Expr>),
}

/// Purpose: This function decides whether a call around a whole prompt
/// dereference shapes the reply, as `toInt(?p)` does, or is an ordinary call,
/// as `print(?p)` is.
///
/// The order of these checks is the language rule:
///
/// 1. *A converter is its type.* `toInt(?p)` asks the model for an int. The
///    type functions are not converters: `int(?p)` checks the reply, a str,
///    and the checker names `toInt`.
/// 2. *`array`, `dict` and a declared class are types,* the class checked
///    before the function test since it is a name in scope too.
/// 3. *A function is an ordinary call.*
/// 4. *A Grammar value constrains sampling.*
/// 5. *An uppercase name is a class,* even one this file does not declare,
///    since an imported class is not in `struct_defs` here.
///
/// Inputs:
/// - `callee`: what is called.
/// - `ctx`: the name table.
///
/// Outputs: the constraint, or `None` for an ordinary call.
fn deref_constraint(callee: &Expr, ctx: &mut Names) -> Result<Option<DerefStage>> {
    let Some(name) = extract_type_name(callee) else {
        return Ok(None);
    };
    if let Some(ty) = converter_type(&name) {
        return Ok(Some(DerefStage::Type(ty.to_string())));
    }
    if is_collection_type(&name) || ctx.struct_defs.contains_key(&name) {
        return Ok(Some(DerefStage::Type(name)));
    }
    match ctx.get(&name) {
        Some(k) if k.is_function() => return Ok(None),
        Some(NameKind::Grammar) => {
            return Ok(Some(DerefStage::Grammar(boxed(callee.clone(), ctx)?)));
        }
        _ => {}
    }
    if last_segment_is_upper(&name) {
        return Ok(Some(DerefStage::Type(name)));
    }
    Ok(None)
}

/// Purpose: This function says whether a name is a collection type a reply
/// can be decoded into as JSON: `array(?p)`, `dict(?p)`.
fn is_collection_type(name: &str) -> bool {
    matches!(name, "array" | "Array" | "dict" | "Dict")
}

/// Purpose: This function says whether the last part of a name, `City` in
/// `shapes.City`, starts with a capital, as a class name does.
fn last_segment_is_upper(name: &str) -> bool {
    name.rsplit('.').next().and_then(|n| n.chars().next()).is_some_and(char::is_uppercase)
}

/// Purpose: This function gives the type a converter makes.
///
/// Inputs:
/// - `name`: a name a call names.
///
/// Outputs: the type's name for `toInt`, `toFloat`, `toString`, `toBool` and
/// `toChar`, and `None` for anything else.
fn converter_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "toInt" => "int",
        "toFloat" => "float",
        "toString" => "str",
        "toBool" => "bool",
        "toChar" => "char",
        _ => return None,
    })
}

/// Purpose: This function finds a pipe stage, right after a prompt
/// dereference, that only makes sense on the whole reply: a converter, a type
/// function, a class or a grammar. Each was how a typed dereference was
/// written before pipes ran once per piece, and calling one on a single
/// token's text is almost never what was meant.
///
/// Inputs:
/// - `stage`: the pipe's first stage.
/// - `ctx`: the name table.
///
/// Outputs: how the stage was written and the call that applies it to the
/// whole reply, such as `toInt(?p)`, or `None` for an ordinary function.
fn whole_reply_stage(stage: &Expr, ctx: &Names) -> Option<(String, String)> {
    use super::infer::SizedNum;
    let name = extract_type_name(stage)?;
    let canon = super::infer::canonical(&name);
    let type_fn_converter = match canon {
        "int" => Some("toInt"),
        "float" => Some("toFloat"),
        "str" => Some("toString"),
        "bool" => Some("toBool"),
        "char" => Some("toChar"),
        other => SizedNum::from_name(other).map(SizedNum::converter),
    };
    let fix = if let Some(conv) = type_fn_converter {
        conv.to_string()
    } else if converter_type(canon).is_some()
        || SizedNum::from_converter(canon).is_some()
        || is_collection_type(&name)
        || ctx.struct_defs.contains_key(&name)
        || ctx.get(&name) == Some(NameKind::Grammar)
    {
        name.clone()
    } else if ctx.get(&name).is_some_and(NameKind::is_function) {
        return None;
    } else if last_segment_is_upper(&name) {
        name.clone()
    } else {
        return None;
    };
    Some((name, format!("{fix}(?p)")))
}

/// Lower a pipe chain, `source |> f |> g(a)`.
///
/// The parser nests a chain as `RawPipe(RawPipe(source, f), g(a))`; this
/// flattens it into one `Pipe`, so each piece goes through every stage before
/// the next piece is made. The stages become one expression over a name for
/// the piece in hand, `g(f(piece), a)`. A stage argument that is not a literal
/// or a name gets a name of its own, so it is worked out once, not once per
/// piece.
fn resolve_pipe(value: Expr, stage: Expr, span: Span, ctx: &mut Names) -> Result<Expr> {
    let mut stages = vec![stage];
    let mut source = value;
    loop {
        match source.kind {
            ExprKind::RawPipe { value, stage } => {
                stages.push(*stage);
                source = *value;
            }
            kind => {
                source = Expr::new(kind, source.span);
                break;
            }
        }
    }
    stages.reverse();

    let from_prompt = matches!(source.kind, ExprKind::PromptDeref { .. });
    if from_prompt && let Some((stage, fix)) = whole_reply_stage(&stages[0], ctx) {
        return Err(JadeError::WholeReplyStage { stage, fix, span: stages[0].span });
    }
    // Reject a stage that plainly cannot be called, so the message names what
    // it found rather than letting the call path report a mismatched callee.
    for s in &stages {
        if let Some(got) = uncallable_stage(s, ctx) {
            return Err(JadeError::InvalidPipeStage { got, span: s.span });
        }
    }

    let source = boxed(source, ctx)?;
    let id = ctx.next_pipe;
    ctx.next_pipe += 1;
    let piece = format!("$piece{id}");
    let mut hoisted_src: Vec<(String, Expr)> = Vec::new();
    let mut body = Expr::new(ExprKind::Identifier(piece.clone()), span);
    for s in stages {
        let sspan = s.span;
        body = match s.kind {
            // A call stage keeps its own span, so `5 |> add(3)` reports errors
            // against `add(3)`, not against the pipe.
            ExprKind::Call { callee, args, kwargs } => {
                let mut with_piece = Vec::with_capacity(args.len() + 1);
                with_piece.push(body);
                for a in args {
                    with_piece.push(hoist_stage_arg(a, id, &mut hoisted_src));
                }
                let kwargs = kwargs
                    .into_iter()
                    .map(|(k, v)| (k, hoist_stage_arg(v, id, &mut hoisted_src)))
                    .collect();
                Expr::new(ExprKind::Call { callee, args: with_piece, kwargs }, sspan)
            }
            other => Expr::new(
                ExprKind::Call {
                    callee: Box::new(Expr::new(other, sspan)),
                    args: vec![body],
                    kwargs: vec![],
                },
                sspan,
            ),
        };
    }

    let mut hoisted = Vec::with_capacity(hoisted_src.len());
    for (name, e) in hoisted_src {
        hoisted.push((name, resolve_expr(e, ctx)?));
    }
    ctx.push_scope();
    for (name, _) in &hoisted {
        ctx.define(name.clone(), NameKind::Value);
    }
    ctx.define(piece.clone(), NameKind::Value);
    let body = boxed(body, ctx);
    ctx.pop_scope();
    Ok(Expr::new(ExprKind::Pipe { source, hoisted, piece, body: body? }, span))
}

/// Purpose: This function leaves a pipe stage's argument in place when
/// working it out once per piece would change nothing, a literal or a name,
/// and otherwise binds it to a name of its own, worked out once before the
/// first piece.
///
/// A literal stays where it is so it still takes the type it meets: in
/// `x |> f(5)` with `f` taking a byte, the 5 is a byte.
///
/// Inputs:
/// - `a`: the argument.
/// - `pipe`: the pipe's number, for a name no other pipe uses.
/// - `out`: the hoisted arguments so far.
///
/// Outputs: the argument, or the name standing for it.
fn hoist_stage_arg(a: Expr, pipe: usize, out: &mut Vec<(String, Expr)>) -> Expr {
    let stays = match &a.kind {
        ExprKind::Integer(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Identifier(_) => true,
        ExprKind::UnaryOp { op: crate::frontend::ast::UnaryOpKind::Neg, operand } => {
            matches!(operand.kind, ExprKind::Integer(_) | ExprKind::Float(_))
        }
        _ => false,
    };
    if stays {
        return a;
    }
    let name = format!("$arg{pipe}.{}", out.len());
    let span = a.span;
    out.push((name.clone(), a));
    Expr::new(ExprKind::Identifier(name), span)
}

/// Type keywords, which a pipe stage may name without being a function in
/// scope.
fn is_builtin_type_name(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "float"
            | "bool"
            | "char"
            | "str"
            | "nil"
            | "null"
            | "array"
            | "Array"
            | "dict"
            | "Dict"
    )
}

/// Describe a pipe stage that is certainly not a function, or `None` if it
/// might be. A literal is never one; a name is refused only when this file
/// knows it holds a prompt or a grammar. Any other value may hold a function,
/// so the whole-program checker decides, once it knows the value's type.
fn uncallable_stage(stage: &Expr, ctx: &Names) -> Option<String> {
    match &stage.kind {
        ExprKind::Integer(_) => Some("int".to_string()),
        ExprKind::Float(_) => Some("float".to_string()),
        ExprKind::Bool(_) => Some("bool".to_string()),
        ExprKind::Str(_) | ExprKind::FStr { .. } => Some("str".to_string()),
        ExprKind::Array { .. } => Some("array".to_string()),
        ExprKind::Dict { .. } => Some("dict".to_string()),
        ExprKind::Identifier(name) if !is_builtin_type_name(name) => match ctx.get(name) {
            Some(NameKind::Prompt) => Some("prompt".to_string()),
            Some(NameKind::Grammar) => Some("grammar".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Purpose: This function says whether a class's field is private in the class
/// that declares it, as far as this file can tell.
///
/// Inputs:
/// - `ctx`: the file's name table.
/// - `class`: the class the literal builds.
/// - `field`: the field.
///
/// Outputs: true when the declaring class is in this file and does not mark
/// the field `@public`.
fn field_is_private(ctx: &Names, class: &str, field: &str) -> bool {
    let has =
        |c: &str| ctx.struct_defs.get(c).is_some_and(|fs| fs.iter().any(|f| f.name() == field));
    let owner = ctx
        .struct_ancestors
        .get(class)
        .and_then(|a| a.iter().rev().find(|x| has(x)).cloned())
        .unwrap_or_else(|| class.to_string());
    ctx.struct_members.get(&owner).is_some_and(|m| !m.is_public(field))
}

/// Extract a dotted name from an expression a call or pipe stage names.
/// Returns `None` if the expression is not a plain identifier or dotted path.
fn extract_type_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::FieldAccess { object, field } => {
            let base = extract_type_name(object)?;
            Some(format!("{}.{}", base, field))
        }
        _ => None,
    }
}

// Tests for this module live in `src/typechecker/tests.rs` (`mod resolve`).
