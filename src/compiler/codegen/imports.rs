//! Import resolution + module namespacing for the LLVM backend.
//!
//! ## Why this exists
//!
//! The bytecode VM gives every imported file its **own namespace**: `use "a.jde"
//! as a` runs `a.jde` in an isolated sub-state and binds its symbols under `a`
//! (`a.greet`, struct defs as `a.Foo`, methods as `a.Foo`). Two different modules
//! can each define `greet` and stay distinct (see `vm::run` ImportFile handling).
//! **The VM is the source of truth.**
//!
//! The AOT backend has no runtime namespaces — everything collapses into one LLVM
//! module. The previous pass flattened every imported file into one global stream
//! and rewrote `a.greet` → bare `greet`, which **fused all namespaces together**:
//! two modules each defining `greet` collided (last definition won, or LLVM
//! rejected the duplicate symbol). That diverged from the VM.
//!
//! ## What this pass does instead
//!
//! It inlines imported files (AOT still needs one module) but **mangles every
//! imported module's globals by a per-module id**, so distinct modules never
//! collide. `main` keeps bare names — it is the root namespace, matching the VM.
//!
//! Mangling scheme: `name$<id>`. `$` is valid in LLVM symbol names but is **never
//! produced by the Jade lexer** (identifiers are `[A-Za-z0-9_]`), so a mangled name
//! can never collide with a user-written one. It is also untouched by codegen's
//! existing name-splitting conventions:
//!   * `rsplit_once('.')` — type / decorator namespace stripping
//!   * `split_once("__")` — `Type__method` extend-method keys
//! so `Foo$2` stays intact and methods key as `Foo$2__method`.
//!
//! ## Coverage
//!
//! The mangle is scope-aware and crosses every name-bearing position:
//!   * **values** — `let` / `fn` / `async fn` / `prompt` names, identifier refs,
//!     `alias.foo` accesses, decorator targets, `from "x" use a` bindings.
//!   * **types**  — `struct` / `interface` names, `StructLiteral.type_name`,
//!     `extend` `type_name` / `interface_name`, `JadeType::Struct(_)` embedded in
//!     every `TExpr.ty` / `ret_ty`, `?p |> Type` output types, `catch Type`.
//!   * **AST defaults** — `StructFieldDef::Let/Prompt` defaults are raw AST `Expr`
//!     (inlined into literals later by `fill_struct_literal_defaults`), so they get
//!     their own walker.
//!
//! Scope-awareness: a bare identifier is only mangled when it resolves to a
//! *module-global* and is **not shadowed** by a function param, `let`, loop var,
//! closure param, or catch binding. Get this wrong and you clobber locals.
//!
//! Known gap (alpha, tune later): closures appearing *inside* a struct-field
//! default expression have their `Vec<Stmt>` body left un-mangled — vanishingly
//! rare and not worth an entire AST-statement walker yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::compiler::tir::{JadeType, TExpr, TExprKind, TFStrPart, TStmt};
use crate::frontend::ast::{Expr, FStrPart, StructFieldDef};

/// Mangle a module-global `name` into module `id`'s namespace.
fn mangle(name: &str, id: u32) -> String {
    format!("{name}${id}")
}

/// stdlib packages and native packages are handled by separate codegen dispatch
/// paths — they never participate in source-file flattening, and their `ns.foo`
/// accesses stay qualified for the stdlib path to resolve.
fn is_stdlib_or_native(path: &str) -> bool {
    path.starts_with("native/") || crate::compiler::stdlib::find_package(path).is_some()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve every `use "file.jde"` reachable from `source_path`, inline the
/// imported files (mangled into per-module namespaces), and return one flat,
/// fully-resolved statement stream for the LLVM backend. `main`'s own globals
/// keep bare names; imported modules' globals are mangled `name$<id>`.
pub fn resolve_and_namespace(
    stmts: Vec<TStmt>,
    source_path: &Path,
) -> Result<Vec<TStmt>, String> {
    let main_canon = source_path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", source_path.display()))?;
    let main_dir = main_canon
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut reg = Registry::new();
    // Mark main as loaded so a self-import is treated as a cycle (skipped), not
    // re-inlined under a fresh id.
    reg.loaded.insert(main_canon);

    // main carries self_id = None → its own names are never mangled (root namespace).
    let main_stmts = process_file(&mut reg, stmts, &main_dir, None, HashSet::new(), HashSet::new())?;

    // Imported modules were appended to `reg.out` in dependency order (deps before
    // dependents); main's statements come last.
    let mut out = reg.out;
    out.extend(main_stmts);
    Ok(out)
}

// ── Module registry ───────────────────────────────────────────────────────────

/// Per-module bookkeeping, keyed by canonical file path. `main` is intentionally
/// absent (it has no id and is never mangled).
struct Registry {
    next_id: u32,
    id: HashMap<PathBuf, u32>,
    values: HashMap<PathBuf, HashSet<String>>,
    types: HashMap<PathBuf, HashSet<String>>,
    loaded: HashSet<PathBuf>,
    /// Inlined, mangled statements from imported files, in dependency order.
    out: Vec<TStmt>,
}

impl Registry {
    fn new() -> Self {
        Registry {
            next_id: 0,
            id: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            loaded: HashSet::new(),
            out: Vec::new(),
        }
    }

    fn fresh_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Parse + type-infer an imported file, returning its TIR statements.
fn parse_and_infer(canon: &Path) -> Result<Vec<TStmt>, String> {
    let src = std::fs::read_to_string(canon)
        .map_err(|e| format!("import '{}': {e}", canon.display()))?;
    let tokens = crate::frontend::lexer::tokenize(&src).map_err(|e| e.to_string())?;
    let ast = crate::frontend::parser::parse(tokens).map_err(|e| e.to_string())?;
    let tp = crate::compiler::type_infer::infer(ast).map_err(|e| e.to_string())?;
    Ok(tp.stmts)
}

/// Collect a file's top-level global names, split into the value namespace
/// (let / fn / async fn / prompt) and the type namespace (struct / interface).
fn collect_globals(stmts: &[TStmt]) -> (HashSet<String>, HashSet<String>) {
    let mut values = HashSet::new();
    let mut types = HashSet::new();
    for s in stmts {
        match s {
            TStmt::Let { name, .. }
            | TStmt::FnDef { name, .. }
            | TStmt::AsyncFnDef { name, .. }
            | TStmt::PromptDecl { name, .. } => {
                values.insert(name.clone());
            }
            TStmt::StructDef { name, .. } | TStmt::InterfaceDef { name, .. } => {
                types.insert(name.clone());
            }
            _ => {}
        }
    }
    (values, types)
}

/// Ensure `dep_canon` is loaded: parse, assign an id, record its globals, mangle
/// its body, and append it to `reg.out`. Idempotent (diamond imports inline once).
fn load_dep(reg: &mut Registry, dep_canon: &Path) -> Result<(), String> {
    if reg.loaded.contains(dep_canon) {
        return Ok(());
    }
    reg.loaded.insert(dep_canon.to_path_buf());

    let stmts = parse_and_infer(dep_canon)?;
    let id = reg.fresh_id();
    reg.id.insert(dep_canon.to_path_buf(), id);

    let (values, types) = collect_globals(&stmts);
    reg.values.insert(dep_canon.to_path_buf(), values.clone());
    reg.types.insert(dep_canon.to_path_buf(), types.clone());

    let dep_dir = dep_canon
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    // Recurse first (post-order): this module's own deps land in `reg.out` before
    // it does, so forward references resolve.
    let mangled = process_file(reg, stmts, &dep_dir, Some(id), values, types)?;
    reg.out.extend(mangled);
    Ok(())
}

/// Resolve the imports of a single file and mangle its body. Returns the mangled,
/// import-free statements (the caller decides where to splice them).
fn process_file(
    reg: &mut Registry,
    stmts: Vec<TStmt>,
    dir: &Path,
    self_id: Option<u32>,
    self_values: HashSet<String>,
    self_types: HashSet<String>,
) -> Result<Vec<TStmt>, String> {
    let mut aliases: HashMap<String, (u32, HashSet<String>, HashSet<String>)> = HashMap::new();
    let mut from_value: HashMap<String, String> = HashMap::new();
    let mut from_type: HashMap<String, String> = HashMap::new();
    let mut kept: Vec<TStmt> = Vec::new();

    for s in stmts {
        match s {
            TStmt::Use { path, as_name, .. } => {
                // stdlib / native: drop the `use` (matches prior behavior); the
                // `ns.foo` access stays qualified for stdlib dispatch.
                if is_stdlib_or_native(&path) {
                    continue;
                }
                let dep = canon_join(dir, &path)?;
                load_dep(reg, &dep)?;
                let alias = as_name.unwrap_or_else(|| stem(&path));
                let id = reg.id[&dep];
                aliases.insert(alias, (id, reg.values[&dep].clone(), reg.types[&dep].clone()));
                // `use` itself is dropped — the body is inlined+mangled already.
            }
            TStmt::FromUse { path, names, path_is_string, span } => {
                // stdlib / native from-imports are preserved verbatim for the
                // stdlib dispatch path (parity with the prior pass).
                if is_stdlib_or_native(&path) {
                    kept.push(TStmt::FromUse { path, names, path_is_string, span });
                    continue;
                }
                let dep = canon_join(dir, &path)?;
                load_dep(reg, &dep)?;
                let id = reg.id[&dep];
                for n in &names {
                    if reg.values[&dep].contains(n) {
                        from_value.insert(n.clone(), mangle(n, id));
                    }
                    if reg.types[&dep].contains(n) {
                        from_type.insert(n.clone(), mangle(n, id));
                    }
                }
                // `from ... use` is dropped — selected names map to mangled targets.
            }
            other => kept.push(other),
        }
    }

    let mut renamer = Renamer {
        self_id,
        self_values,
        self_types,
        aliases,
        from_value,
        from_type,
        locals: Vec::new(),
    };
    for s in kept.iter_mut() {
        renamer.rename_stmt(s, true);
    }
    Ok(kept)
}

fn canon_join(dir: &Path, path: &str) -> Result<PathBuf, String> {
    dir.join(path)
        .canonicalize()
        .map_err(|e| format!("import '{}': {e}", path))
}

fn stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

// ── Renamer ───────────────────────────────────────────────────────────────────

/// Walks one module's statements, mangling references to module-globals (its own,
/// via `self_*`; imported, via `aliases` / `from_*`) while leaving locals bare.
struct Renamer {
    /// `None` for `main` (root namespace — never mangle own names).
    self_id: Option<u32>,
    self_values: HashSet<String>,
    self_types: HashSet<String>,
    /// import alias → (module id, value names, type names) of the target module.
    aliases: HashMap<String, (u32, HashSet<String>, HashSet<String>)>,
    /// `from "x" use n` → mangled target name, by namespace.
    from_value: HashMap<String, String>,
    from_type: HashMap<String, String>,
    /// Lexical local-binding stack; an identifier shadowed here is never mangled.
    locals: Vec<HashSet<String>>,
}

impl Renamer {
    fn push_scope(&mut self) {
        self.locals.push(HashSet::new());
    }
    fn pop_scope(&mut self) {
        self.locals.pop();
    }
    fn bind_local(&mut self, name: &str) {
        if let Some(top) = self.locals.last_mut() {
            top.insert(name.to_string());
        }
    }
    fn is_shadowed(&self, name: &str) -> bool {
        self.locals.iter().any(|s| s.contains(name))
    }

    /// Resolve a bare value reference (scope-aware).
    fn ref_value(&self, name: &str) -> Option<String> {
        if self.is_shadowed(name) {
            return None;
        }
        if let Some(m) = self.from_value.get(name) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id {
            if self.self_values.contains(name) {
                return Some(mangle(name, id));
            }
        }
        None
    }

    /// Resolve `alias.field` to a mangled value, unless `alias` is a shadowing local.
    fn ref_value_qual(&self, alias: &str, field: &str) -> Option<String> {
        if self.is_shadowed(alias) {
            return None;
        }
        let (id, values, _) = self.aliases.get(alias)?;
        if values.contains(field) {
            Some(mangle(field, *id))
        } else {
            None
        }
    }

    /// Resolve a type name (possibly `alias.Type`). Types share no namespace with
    /// values and are not shadowed by locals.
    fn ref_type(&self, tn: &str) -> Option<String> {
        if let Some((alias, field)) = tn.split_once('.') {
            let (id, _, types) = self.aliases.get(alias)?;
            return types.contains(field).then(|| mangle(field, *id));
        }
        if let Some(m) = self.from_type.get(tn) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id {
            if self.self_types.contains(tn) {
                return Some(mangle(tn, id));
            }
        }
        None
    }

    /// Resolve a decorator target (`alias.dec` or bare `dec`) — a value, applied at
    /// the (top-level) definition site so no local shadowing applies.
    fn ref_decorator(&self, name: &str) -> Option<String> {
        if let Some((alias, field)) = name.split_once('.') {
            let (id, values, _) = self.aliases.get(alias)?;
            return values.contains(field).then(|| mangle(field, *id));
        }
        if let Some(m) = self.from_value.get(name) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id {
            if self.self_values.contains(name) {
                return Some(mangle(name, id));
            }
        }
        None
    }

    /// Mangle a top-level value *definition* name (no-op for `main`).
    fn def_value(&self, name: &str) -> Option<String> {
        self.self_id
            .filter(|_| self.self_values.contains(name))
            .map(|id| mangle(name, id))
    }
    /// Mangle a top-level type *definition* name (no-op for `main`).
    fn def_type(&self, name: &str) -> Option<String> {
        self.self_id
            .filter(|_| self.self_types.contains(name))
            .map(|id| mangle(name, id))
    }

    // ── Statements ────────────────────────────────────────────────────────────

    /// `top` marks module-top-level statements, whose definition names are
    /// globals (mangled). Inside bodies, definitions are locals (bound, not mangled).
    fn rename_stmt(&mut self, s: &mut TStmt, top: bool) {
        match s {
            TStmt::Let { name, value, .. } => {
                self.rename_expr(value);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
            }
            TStmt::Assign { name, value, .. } => {
                self.rename_expr(value);
                if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            TStmt::FnDef { name, params, body, ret_ty, decorators, .. }
            | TStmt::AsyncFnDef { name, params, body, ret_ty, decorators, .. } => {
                self.rename_decorators(decorators);
                self.rename_type(ret_ty);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
                self.push_scope();
                for (pname, default) in params.iter_mut() {
                    if let Some(d) = default {
                        self.rename_expr(d);
                    }
                    self.bind_local(pname);
                }
                for st in body.iter_mut() {
                    self.rename_stmt(st, false);
                }
                self.pop_scope();
            }
            TStmt::StructDef { name, fields, decorators, .. } => {
                self.rename_decorators(decorators);
                for f in fields.iter_mut() {
                    match f {
                        StructFieldDef::Let { default, .. }
                        | StructFieldDef::Prompt { default, .. } => self.rename_ast_expr(default),
                        StructFieldDef::Required(_) => {}
                    }
                }
                if top {
                    if let Some(m) = self.def_type(name) {
                        *name = m;
                    }
                }
            }
            TStmt::InterfaceDef { name, methods, .. } => {
                for m in methods.iter_mut() {
                    if let Some(rt) = m.return_type.as_mut() {
                        if let Some(x) = self.ref_type(rt) {
                            *rt = x;
                        }
                    }
                }
                if top {
                    if let Some(m) = self.def_type(name) {
                        *name = m;
                    }
                }
            }
            TStmt::ExtendBlock { type_name, interface_name, methods, decorators, .. } => {
                self.rename_decorators(decorators);
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                if let Some(iname) = interface_name.as_mut() {
                    if let Some(m) = self.ref_type(iname) {
                        *iname = m;
                    }
                }
                // Methods are FnDefs; their names are NOT globals (they key as
                // `Type$id__method` off the already-mangled type_name).
                for meth in methods.iter_mut() {
                    if let TStmt::FnDef { params, body, ret_ty, decorators, .. }
                    | TStmt::AsyncFnDef { params, body, ret_ty, decorators, .. } = meth
                    {
                        self.rename_decorators(decorators);
                        self.rename_type(ret_ty);
                        self.push_scope();
                        for (pname, default) in params.iter_mut() {
                            if let Some(d) = default {
                                self.rename_expr(d);
                            }
                            self.bind_local(pname);
                        }
                        for st in body.iter_mut() {
                            self.rename_stmt(st, false);
                        }
                        self.pop_scope();
                    } else {
                        self.rename_stmt(meth, false);
                    }
                }
            }
            TStmt::FieldAssign { object, value, .. } => {
                self.rename_expr(value);
                if let Some(m) = self.ref_value(object) {
                    *object = m;
                }
            }
            TStmt::IndexAssign { name, index, value, .. } => {
                self.rename_expr(index);
                self.rename_expr(value);
                if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            TStmt::PromptDecl { name, body, .. } => {
                self.rename_expr(body);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
            }
            TStmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.rename_expr(e);
                }
            }
            TStmt::If { condition, then_body, else_body, .. } => {
                self.rename_expr(condition);
                self.block(then_body);
                if let Some(eb) = else_body {
                    self.block(eb);
                }
            }
            TStmt::While { condition, body, .. } => {
                self.rename_expr(condition);
                self.block(body);
            }
            TStmt::For { var, iterable, body, .. } => {
                self.rename_expr(iterable);
                self.push_scope();
                self.bind_local(var);
                for st in body.iter_mut() {
                    self.rename_stmt(st, false);
                }
                self.pop_scope();
            }
            TStmt::TryCatch { body, arms, .. } => {
                self.block(body);
                for arm in arms.iter_mut() {
                    if let Some(ct) = arm.catch_type.as_mut() {
                        if let Some(m) = self.ref_type(ct) {
                            *ct = m;
                        }
                    }
                    self.push_scope();
                    self.bind_local(&arm.binding);
                    for st in arm.body.iter_mut() {
                        self.rename_stmt(st, false);
                    }
                    self.pop_scope();
                }
            }
            TStmt::Raise { value, .. } => self.rename_expr(value),
            TStmt::Expr(e) => self.rename_expr(e),
            // Imports were stripped in process_file (except preserved stdlib
            // from-uses, which carry nothing to rename).
            TStmt::Use { .. } | TStmt::FromUse { .. } => {}
        }
    }

    fn block(&mut self, stmts: &mut Vec<TStmt>) {
        self.push_scope();
        for s in stmts.iter_mut() {
            self.rename_stmt(s, false);
        }
        self.pop_scope();
    }

    fn rename_decorators(
        &mut self,
        decorators: &mut [(String, Vec<(Option<String>, TExpr)>)],
    ) {
        for (dname, dargs) in decorators.iter_mut() {
            if let Some(m) = self.ref_decorator(dname) {
                *dname = m;
            }
            for (_, e) in dargs.iter_mut() {
                self.rename_expr(e);
            }
        }
    }

    // ── TIR expressions ───────────────────────────────────────────────────────

    fn rename_expr(&mut self, e: &mut TExpr) {
        self.rename_type(&mut e.ty);

        // `alias.foo` → mangled `foo$<id>` (before walking children, so the bare
        // alias identifier isn't independently rewritten).
        let collapse = if let TExprKind::FieldAccess { object, field } = &e.kind {
            if let TExprKind::Identifier(alias) = &object.kind {
                self.ref_value_qual(alias, field)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(m) = collapse {
            e.kind = TExprKind::Identifier(m);
            return;
        }

        match &mut e.kind {
            TExprKind::Identifier(n) => {
                if let Some(m) = self.ref_value(n) {
                    *n = m;
                }
            }
            TExprKind::Call { callee, args, kwargs } => {
                self.rename_expr(callee);
                for a in args.iter_mut() {
                    self.rename_expr(a);
                }
                for (_, v) in kwargs.iter_mut() {
                    self.rename_expr(v);
                }
            }
            TExprKind::BinOp { left, right, .. } => {
                self.rename_expr(left);
                self.rename_expr(right);
            }
            TExprKind::UnaryOp { operand, .. } => self.rename_expr(operand),
            TExprKind::StructLiteral { type_name, fields } => {
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                for (_, v, _) in fields.iter_mut() {
                    self.rename_expr(v);
                }
            }
            TExprKind::FieldAccess { object, .. } => self.rename_expr(object),
            TExprKind::Index { object, index } => {
                self.rename_expr(object);
                self.rename_expr(index);
            }
            TExprKind::Array { elements } => {
                for el in elements.iter_mut() {
                    self.rename_expr(el);
                }
            }
            TExprKind::FStr { parts } => {
                for p in parts.iter_mut() {
                    if let TFStrPart::Expr(inner) = p {
                        self.rename_expr(inner);
                    }
                }
            }
            TExprKind::PromptDeref { expr, output_type, grammar_expr } => {
                self.rename_expr(expr);
                if let Some(ot) = output_type.as_mut() {
                    if let Some(m) = self.ref_type(ot) {
                        *ot = m;
                    }
                }
                if let Some(g) = grammar_expr {
                    self.rename_expr(g);
                }
            }
            TExprKind::Dict { entries } => {
                for (k, v) in entries.iter_mut() {
                    self.rename_expr(k);
                    self.rename_expr(v);
                }
            }
            TExprKind::Closure { params, body, captures } => {
                // Captures bind enclosing locals (kept bare); only their types may
                // reference mangled structs.
                for (_, cty) in captures.iter_mut() {
                    self.rename_type(cty);
                }
                self.push_scope();
                for p in params.iter() {
                    self.bind_local(p);
                }
                for (cn, _) in captures.iter() {
                    self.bind_local(cn);
                }
                for st in body.iter_mut() {
                    self.rename_stmt(st, false);
                }
                self.pop_scope();
            }
            TExprKind::Await { expr } => self.rename_expr(expr),
            TExprKind::PromptLiteral { body } => self.rename_expr(body),
            TExprKind::Integer(_)
            | TExprKind::Float(_)
            | TExprKind::Bool(_)
            | TExprKind::Str(_) => {}
        }
    }

    fn rename_type(&mut self, t: &mut JadeType) {
        match t {
            JadeType::Struct(n) => {
                if let Some(m) = self.ref_type(n) {
                    *n = m;
                }
            }
            JadeType::Array(inner) | JadeType::Future(inner) => self.rename_type(inner),
            JadeType::Fn { params, ret } | JadeType::AsyncFn { params, ret } => {
                for p in params.iter_mut() {
                    self.rename_type(p);
                }
                self.rename_type(ret);
            }
            _ => {}
        }
    }

    // ── AST expressions (struct-field defaults) ───────────────────────────────

    fn rename_ast_expr(&mut self, e: &mut Expr) {
        // `alias.foo` collapse, same as the TIR path.
        let repl = if let Expr::FieldAccess { object, field, span } = &*e {
            if let Expr::Identifier { name: alias, .. } = object.as_ref() {
                self.ref_value_qual(alias, field).map(|m| (m, *span))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((m, span)) = repl {
            *e = Expr::Identifier { name: m, span };
            return;
        }

        match e {
            Expr::Identifier { name, .. } => {
                if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            Expr::Call { callee, args, kwargs, .. } => {
                self.rename_ast_expr(callee);
                for a in args.iter_mut() {
                    self.rename_ast_expr(a);
                }
                for (_, v) in kwargs.iter_mut() {
                    self.rename_ast_expr(v);
                }
            }
            Expr::BinOp { left, right, .. } => {
                self.rename_ast_expr(left);
                self.rename_ast_expr(right);
            }
            Expr::UnaryOp { operand, .. } => self.rename_ast_expr(operand),
            Expr::StructLiteral { type_name, fields, .. } => {
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                for (_, v) in fields.iter_mut() {
                    self.rename_ast_expr(v);
                }
            }
            Expr::FieldAccess { object, .. } => self.rename_ast_expr(object),
            Expr::Index { object, index, .. } => {
                self.rename_ast_expr(object);
                self.rename_ast_expr(index);
            }
            Expr::Array { elements, .. } => {
                for el in elements.iter_mut() {
                    self.rename_ast_expr(el);
                }
            }
            Expr::FStr { parts, .. } => {
                for p in parts.iter_mut() {
                    if let FStrPart::Expr(inner) = p {
                        self.rename_ast_expr(inner);
                    }
                }
            }
            Expr::PromptLiteral { body, .. } => self.rename_ast_expr(body),
            Expr::PromptDeref { expr, constraint, .. } => {
                self.rename_ast_expr(expr);
                if let Some(c) = constraint {
                    self.rename_ast_expr(c);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries.iter_mut() {
                    self.rename_ast_expr(k);
                    self.rename_ast_expr(v);
                }
            }
            Expr::Await { expr, .. } => self.rename_ast_expr(expr),
            // Known gap: closure bodies inside a default are not walked (see module doc).
            Expr::Closure { .. } => {}
            Expr::Integer { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Str { .. } => {}
        }
    }
}
