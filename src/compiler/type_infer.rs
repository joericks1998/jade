use std::collections::{HashMap, HashSet};

use crate::{
    compiler::builtins,
    frontend::{
        ast::{BinOpKind, Expr, FStrPart, Program, StructFieldDef, Stmt, UnaryOpKind},
        error::{JadeError, Result, Span},
    },
};
use super::tir::{JadeType, TExpr, TExprKind, TFStrPart, TProgram, TStmt};

// ── TypeContext ───────────────────────────────────────────────────────────────

/// The type environment maintained during inference.
///
/// Uses a scope stack matching the evaluator's scoping: `scopes[0]` is global,
/// `scopes.last()` is the innermost. Struct and interface definitions are stored
/// in flat maps (they are always global in the current language).
pub struct TypeContext {
    /// Variable name → resolved type, innermost scope last.
    scopes: Vec<HashMap<String, JadeType>>,
    /// Struct type name → field definitions (copied from AST).
    struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Interface name → required method names.
    interface_defs: HashMap<String, Vec<String>>,
    /// Extend: type_name → method_name → inferred return type.
    extend_methods: HashMap<String, HashMap<String, JadeType>>,
    /// Primitive type methods: type_name → method_name → JadeType.
    /// E.g. "str" → {"upper" → Fn { .. }}, "array" → {"push" → Fn { .. }}.
    pub primitive_methods: HashMap<String, HashMap<String, JadeType>>,
    /// True if this program contains any `use` statements. When true, unknown
    /// identifiers are treated as `Unknown` (resolved at VM runtime) rather
    /// than hard errors — symbols may be provided by the imports.
    has_imports: bool,
}

impl TypeContext {
    pub fn new() -> Self {
        let mut ctx = TypeContext {
            scopes: vec![HashMap::new()],
            struct_defs: HashMap::new(),
            interface_defs: HashMap::new(),
            extend_methods: HashMap::new(),
            primitive_methods: HashMap::new(),
            has_imports: false,
        };
        builtins::register_core_types(&mut ctx);
        builtins::register_primitive_method_types(&mut ctx);
        ctx
    }

    /// Register a primitive method type for type inference of `receiver.method(...)`.
    pub fn define_primitive_method(&mut self, type_name: &str, method_name: &str, ty: JadeType) {
        self.primitive_methods
            .entry(type_name.to_string())
            .or_default()
            .insert(method_name.to_string(), ty);
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Bind `name` in the innermost (current) scope — used for `let` and fn params.
    pub fn define(&mut self, name: String, ty: JadeType) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    /// Reassign `name` in the nearest scope that already holds it, or introduce
    /// it in the global scope if not found (matches evaluator's bare-assign
    /// semantics). Never fails — conservative by design.
    fn assign(&mut self, name: &str, ty: JadeType) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), ty);
                return;
            }
        }
        if let Some(global) = self.scopes.first_mut() {
            global.insert(name.to_string(), ty);
        }
    }

    /// Returns `true` if `name` is defined in the global (outermost) scope only.
    /// Used by the capture analysis to skip builtins and top-level functions.
    fn is_in_global_scope(&self, name: &str) -> bool {
        self.scopes.first().map(|s| s.contains_key(name)).unwrap_or(false)
    }

    /// Look up `name` from innermost scope outward. Returns `None` if undefined.
    fn get(&self, name: &str) -> Option<JadeType> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty.clone());
            }
        }
        None
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Infer types for an entire program, producing a typed IR (`TProgram`).
///
/// Two-pass algorithm:
/// 1. `pre_pass`: register all top-level struct, fn, and interface names so
///    forward references resolve correctly.
/// 2. `check_stmts`: walk every statement and expression, attaching types.
///
/// Errors are fatal (first error stops inference). The `Unknown` type is used
/// conservatively whenever a type cannot be determined — no false positives.
/// Static return type for package calls of the form `mod.method(...)`. Mirrors
/// the runtime signatures so codegen sees `JadeType::Str` (not `Unknown`)
/// for string-producing builtins — required for taint-flow gates.
fn pkg_call_return_type(module: &str, method: &str) -> Option<JadeType> {
    match (module, method) {
        ("sh",   "exec")    => Some(JadeType::Str),
        ("fs",   "read")    => Some(JadeType::Str),
        ("fs",   "exists")  => Some(JadeType::Bool),
        ("http", "get")     => Some(JadeType::Dict),
        ("llm",  "count_tokens") => Some(JadeType::Int),
        ("json", "stringify") => Some(JadeType::Str),
        // json.parse intentionally stays Unknown — it can fail and return nil,
        // which would conflict with strict Dict typing on `if x != nil { … }`.
        _ => None,
    }
}

pub fn infer(program: Program) -> Result<TProgram> {
    let mut ctx = TypeContext::new();
    pre_pass(&program.stmts, &mut ctx);
    let stmts = check_stmts(&program.stmts, &mut ctx)?;
    Ok(TProgram { stmts })
}

/// Like `infer` but pre-seeds the type context with a set of known global names
/// as `JadeType::Unknown`. Used by the REPL so that variables defined in earlier
/// snippets are visible to the type checker in subsequent snippets — without
/// requiring the type checker to be fully stateful.
pub fn infer_with_globals(program: Program, known_globals: &[String]) -> Result<TProgram> {
    let mut ctx = TypeContext::new();
    for name in known_globals {
        ctx.define(name.clone(), JadeType::Unknown);
    }
    pre_pass(&program.stmts, &mut ctx);
    let stmts = check_stmts(&program.stmts, &mut ctx)?;
    Ok(TProgram { stmts })
}

// ── Pass 1: pre_pass ──────────────────────────────────────────────────────────

/// Register top-level names without descending into bodies.
/// This allows forward references: a fn call before the definition still resolves.
fn pre_pass(stmts: &[Stmt], ctx: &mut TypeContext) {
    for stmt in stmts {
        match stmt {
            Stmt::FnDef { name, params, .. } => {
                ctx.define(name.clone(), JadeType::Fn {
                    params: vec![JadeType::Unknown; params.len()],
                    ret: Box::new(JadeType::Unknown),
                });
            }
            Stmt::AsyncFnDef { name, params, .. } => {
                ctx.define(name.clone(), JadeType::AsyncFn {
                    params: vec![JadeType::Unknown; params.len()],
                    ret: Box::new(JadeType::Unknown),
                });
            }

            Stmt::StructDef { name, fields, .. } => {
                ctx.struct_defs.insert(name.clone(), fields.clone());
                // Struct names are callable constructors: `City(dict)`.
                ctx.define(name.clone(), JadeType::Fn {
                    params: vec![JadeType::Unknown],
                    ret: Box::new(JadeType::Struct(name.clone())),
                });
            }
            Stmt::InterfaceDef { name, methods, .. } => {
                let method_names = methods.iter().map(|m| m.name.clone()).collect();
                ctx.interface_defs.insert(name.clone(), method_names);
            }
            Stmt::ExtendBlock { type_name, methods, .. } => {
                for method in methods {
                    if let Stmt::FnDef { name: mname, .. } = method {
                        ctx.extend_methods
                            .entry(type_name.clone())
                            .or_default()
                            .insert(mname.clone(), JadeType::Unknown);
                    }
                }
            }
            Stmt::Use { .. } | Stmt::FromUse { .. } => {
                ctx.has_imports = true;
            }
            _ => {}
        }
    }
}

// ── Pass 2: check_stmts / check_stmt ─────────────────────────────────────────

fn check_stmts(stmts: &[Stmt], ctx: &mut TypeContext) -> Result<Vec<TStmt>> {
    stmts.iter().map(|s| check_stmt(s, ctx)).collect()
}

fn check_stmt(stmt: &Stmt, ctx: &mut TypeContext) -> Result<TStmt> {
    match stmt {
        // ── Variable bindings ─────────────────────────────────────────────────

        Stmt::Let { name, value, span } => {
            let tval = infer_expr(value, ctx)?;
            ctx.define(name.clone(), tval.ty.clone());
            Ok(TStmt::Let { name: name.clone(), value: tval, span: *span })
        }

        Stmt::Assign { name, value, span } => {
            let tval = infer_expr(value, ctx)?;
            ctx.assign(name, tval.ty.clone());
            Ok(TStmt::Assign { name: name.clone(), value: tval, span: *span })
        }

        // ── Functions ─────────────────────────────────────────────────────────

        Stmt::FnDef { name, params, body, decorators, span } => {
            ctx.push_scope();
            for (pname, _) in params {
                ctx.define(pname.clone(), JadeType::Unknown);
            }
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();

            let ret_ty = infer_return_type(&tbody);
            ctx.define(name.clone(), JadeType::Fn {
                params: vec![JadeType::Unknown; params.len()],
                ret: Box::new(ret_ty.clone()),
            });

            let tparams = params.iter()
                .map(|(pname, default)| {
                    let tdefault = default.as_ref().map(|e| infer_expr(e, ctx)).transpose()?;
                    Ok((pname.clone(), tdefault))
                })
                .collect::<Result<Vec<_>>>()?;

            let tdecorators = infer_decorators(decorators, ctx)?;
            Ok(TStmt::FnDef {
                name: name.clone(),
                params: tparams,
                body: tbody,
                ret_ty,
                decorators: tdecorators,
                span: *span,
            })
        }

        Stmt::AsyncFnDef { name, params, body, decorators, span } => {
            ctx.push_scope();
            for (pname, _) in params {
                ctx.define(pname.clone(), JadeType::Unknown);
            }
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();

            let ret_ty = infer_return_type(&tbody);
            ctx.define(name.clone(), JadeType::AsyncFn {
                params: vec![JadeType::Unknown; params.len()],
                ret: Box::new(ret_ty.clone()),
            });

            let tparams = params.iter()
                .map(|(pname, default)| {
                    let tdefault = default.as_ref().map(|e| infer_expr(e, ctx)).transpose()?;
                    Ok((pname.clone(), tdefault))
                })
                .collect::<Result<Vec<_>>>()?;

            let tdecorators = infer_decorators(decorators, ctx)?;
            Ok(TStmt::AsyncFnDef {
                name: name.clone(),
                params: tparams,
                body: tbody,
                ret_ty,
                decorators: tdecorators,
                span: *span,
            })
        }

        Stmt::Return { value, span } => {
            let tval = value.as_ref().map(|e| infer_expr(e, ctx)).transpose()?;
            Ok(TStmt::Return { value: tval, span: *span })
        }

        // ── Control flow ──────────────────────────────────────────────────────

        Stmt::If { condition, then_body, else_body, span } => {
            let tcond = infer_expr(condition, ctx)?;
            require_bool_or_unknown(&tcond.ty, *span)?;

            ctx.push_scope();
            let tthen = check_stmts(then_body, ctx)?;
            ctx.pop_scope();

            let telse = match else_body {
                Some(body) => {
                    ctx.push_scope();
                    let r = check_stmts(body, ctx)?;
                    ctx.pop_scope();
                    Some(r)
                }
                None => None,
            };

            Ok(TStmt::If { condition: tcond, then_body: tthen, else_body: telse, span: *span })
        }

        Stmt::While { condition, body, span } => {
            let tcond = infer_expr(condition, ctx)?;
            require_bool_or_unknown(&tcond.ty, *span)?;

            ctx.push_scope();
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();

            Ok(TStmt::While { condition: tcond, body: tbody, span: *span })
        }

        Stmt::For { var, iterable, body, span } => {
            let titerable = infer_expr(iterable, ctx)?;
            let elem_ty = match &titerable.ty {
                JadeType::Array(elem) => *elem.clone(),
                JadeType::Unknown     => JadeType::Unknown,
                other => return Err(JadeError::TypeError {
                    message: format!("cannot iterate over {}", jade_type_name(other)),
                    span: *span,
                }),
            };
            ctx.push_scope();
            ctx.define(var.clone(), elem_ty);
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();
            Ok(TStmt::For { var: var.clone(), iterable: titerable, body: tbody, span: *span })
        }

        // ── Type definitions ──────────────────────────────────────────────────

        Stmt::StructDef { name, fields, decorators, span } => {
            let tdecorators = infer_decorators(decorators, ctx)?;
            Ok(TStmt::StructDef { name: name.clone(), fields: fields.clone(), decorators: tdecorators, span: *span })
        }

        Stmt::InterfaceDef { name, methods, span } => {
            Ok(TStmt::InterfaceDef { name: name.clone(), methods: methods.clone(), span: *span })
        }

        Stmt::ExtendBlock { type_name, interface_name, methods, decorators, span } => {
            // Verify interface compliance if an interface is named.
            if let Some(iface_name) = interface_name {
                let required = ctx.interface_defs.get(iface_name).cloned();
                match required {
                    Some(required_methods) => {
                        for req in &required_methods {
                            let provided = methods.iter().any(|m| {
                                matches!(m, Stmt::FnDef { name, .. } if name == req)
                            });
                            if !provided {
                                return Err(JadeError::MissingInterfaceMethod {
                                    type_name: type_name.clone(),
                                    interface_name: iface_name.clone(),
                                    method: req.clone(),
                                    span: *span,
                                });
                            }
                        }
                    }
                    None => return Err(JadeError::UndefinedInterface {
                        name: iface_name.clone(),
                        span: *span,
                    }),
                }
            }

            // Type-check method bodies. Each FnDef pushes/pops its own scope.
            // The first param (`self`) is bound as Unknown inside the fn scope —
            // field accesses on self return Unknown (conservative for Stage B).
            let tmethods = check_stmts(methods, ctx)?;

            let tdecorators = infer_decorators(decorators, ctx)?;
            Ok(TStmt::ExtendBlock {
                type_name: type_name.clone(),
                interface_name: interface_name.clone(),
                methods: tmethods,
                decorators: tdecorators,
                span: *span,
            })
        }

        // ── Mutations ─────────────────────────────────────────────────────────

        Stmt::FieldAssign { object, field, value, span } => {
            let tval = infer_expr(value, ctx)?;
            // Check the target object is a known struct with this field.
            match ctx.get(object) {
                Some(JadeType::Struct(tn)) => {
                    if let Some(defs) = ctx.struct_defs.get(&tn).cloned() {
                        if !defs.iter().any(|f| f.name() == field) {
                            return Err(JadeError::UndefinedField {
                                type_name: tn,
                                field: field.clone(),
                                span: *span,
                            });
                        }
                    }
                }
                // Unknown or undefined → conservative, skip check.
                _ => {}
            }
            Ok(TStmt::FieldAssign {
                object: object.clone(),
                field: field.clone(),
                value: tval,
                span: *span,
            })
        }

        Stmt::IndexAssign { name, index, value, span } => {
            let tidx = infer_expr(index, ctx)?;
            let tval = infer_expr(value, ctx)?;
            Ok(TStmt::IndexAssign {
                name: name.clone(),
                index: tidx,
                value: tval,
                span: *span,
            })
        }

        // ── LLM integration ───────────────────────────────────────────────────

        Stmt::PromptDecl { name, body, span } => {
            let tbody = infer_expr(body, ctx)?;
            // Prompt body must be a string (or unknown).
            if tbody.ty != JadeType::Str && tbody.ty != JadeType::Unknown {
                return Err(JadeError::TypeMismatch {
                    expected: "str".to_string(),
                    got: jade_type_name(&tbody.ty),
                    span: *span,
                });
            }
            ctx.define(name.clone(), JadeType::Prompt);
            Ok(TStmt::PromptDecl { name: name.clone(), body: tbody, span: *span })
        }

        // ── Imports ───────────────────────────────────────────────────────────

        Stmt::Use { path, as_name, path_is_string, span } => {
            ctx.has_imports = true;
            if let Some(pkg) = builtins::find_package(path) {
                if *path_is_string {
                    return Err(JadeError::StdlibStringImport { path: path.clone(), span: *span });
                }
                (pkg.register_types)(ctx);
            } else if as_name.is_none() && !path.starts_with("native/") {
                return Err(JadeError::MissingImportAlias { path: path.clone(), span: *span });
            }
            Ok(TStmt::Use { path: path.clone(), as_name: as_name.clone(), path_is_string: *path_is_string, span: *span })
        }

        Stmt::FromUse { path, names, path_is_string, span } => {
            ctx.has_imports = true;
            if let Some(pkg) = builtins::find_package(path) {
                if *path_is_string {
                    return Err(JadeError::StdlibStringImport { path: path.clone(), span: *span });
                }
                (pkg.register_types)(ctx);
            }
            Ok(TStmt::FromUse { path: path.clone(), names: names.clone(), path_is_string: *path_is_string, span: *span })
        }

        // ── Exception handling ────────────────────────────────────────────────

        Stmt::Raise { value, span } => {
            let tval = infer_expr(value, ctx)?;
            Ok(TStmt::Raise { value: tval, span: *span })
        }

        Stmt::TryCatch { body, arms, span } => {
            ctx.push_scope();
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();

            let tarms = arms.iter().map(|arm| {
                ctx.push_scope();
                // The binding variable holds the caught value; type is unknown statically.
                ctx.define(arm.binding.clone(), JadeType::Unknown);
                let tbody = check_stmts(&arm.body, ctx)?;
                ctx.pop_scope();
                Ok(crate::compiler::tir::TCatchArm {
                    catch_type: arm.catch_type.clone(),
                    binding: arm.binding.clone(),
                    body: tbody,
                })
            }).collect::<Result<Vec<_>>>()?;

            Ok(TStmt::TryCatch { body: tbody, arms: tarms, span: *span })
        }

        // ── Bare expression ───────────────────────────────────────────────────

        Stmt::Expr(expr) => {
            let texpr = infer_expr(expr, ctx)?;
            Ok(TStmt::Expr(texpr))
        }
    }
}

// ── Expression type inference ─────────────────────────────────────────────────

fn infer_expr(expr: &Expr, ctx: &mut TypeContext) -> Result<TExpr> {
    match expr {
        // ── Literals ──────────────────────────────────────────────────────────

        Expr::Integer { value, span } =>
            Ok(TExpr { kind: TExprKind::Integer(*value), ty: JadeType::Int, span: *span }),

        Expr::Float { value, span } =>
            Ok(TExpr { kind: TExprKind::Float(*value), ty: JadeType::Float, span: *span }),

        Expr::Bool { value, span } =>
            Ok(TExpr { kind: TExprKind::Bool(*value), ty: JadeType::Bool, span: *span }),

        Expr::Str { value, span } =>
            Ok(TExpr { kind: TExprKind::Str(value.clone()), ty: JadeType::Str, span: *span }),

        // ── Variables ─────────────────────────────────────────────────────────

        Expr::Identifier { name, span } => {
            // `nil` and `None` are built-in literal values; don't require a prior definition.
            if name == "nil" || name == "None" {
                return Ok(TExpr { kind: TExprKind::Identifier(name.clone()), ty: JadeType::Nil, span: *span });
            }
            let ty = match ctx.get(name) {
                Some(t) => t,
                None if ctx.has_imports => JadeType::Unknown,
                None => return Err(JadeError::UndefinedVariable { name: name.clone(), span: *span }),
            };
            Ok(TExpr { kind: TExprKind::Identifier(name.clone()), ty, span: *span })
        }

        // ── Interpolated strings ──────────────────────────────────────────────

        Expr::FStr { parts, span } => {
            let mut tparts = Vec::new();
            for part in parts {
                tparts.push(match part {
                    FStrPart::Literal(s) => TFStrPart::Literal(s.clone()),
                    FStrPart::Expr(e)   => TFStrPart::Expr(infer_expr(e, ctx)?),
                });
            }
            Ok(TExpr { kind: TExprKind::FStr { parts: tparts }, ty: JadeType::Str, span: *span })
        }

        // ── Binary operations ─────────────────────────────────────────────────

        Expr::BinOp { op, left, right, span } => {
            let tleft  = infer_expr(left, ctx)?;
            let tright = infer_expr(right, ctx)?;
            let ty = infer_binop(op, &tleft.ty, &tright.ty, *span)?;
            Ok(TExpr {
                kind: TExprKind::BinOp { op: op.clone(), left: Box::new(tleft), right: Box::new(tright) },
                ty,
                span: *span,
            })
        }

        // ── Unary operations ──────────────────────────────────────────────────

        Expr::UnaryOp { op, operand, span } => {
            let top = infer_expr(operand, ctx)?;
            let ty = infer_unaryop(op, &top.ty, *span)?;
            Ok(TExpr {
                kind: TExprKind::UnaryOp { op: op.clone(), operand: Box::new(top) },
                ty,
                span: *span,
            })
        }

        // ── Function calls ────────────────────────────────────────────────────

        Expr::Call { callee, args, kwargs, span } => {
            // Grammar.new(pattern) → JadeType::Grammar (special-cased for static typing).
            let is_grammar_new = matches!(callee.as_ref(),
                Expr::FieldAccess { object, field, .. }
                if field == "new" && matches!(object.as_ref(), Expr::Identifier { name, .. } if name == "Grammar")
            );

            // Recognize package calls (sh.exec, fs.read, http.get, …) so their
            // return types are known statically — codegen needs Str (not
            // Unknown→i64) for tagged-string flow analysis.
            let pkg_call_ret: Option<JadeType> = match callee.as_ref() {
                Expr::FieldAccess { object, field, .. } => match object.as_ref() {
                    Expr::Identifier { name, .. } => pkg_call_return_type(name, field),
                    _ => None,
                },
                _ => None,
            };

            let tcallee = infer_expr(callee, ctx)?;
            let targs: Vec<TExpr> = args.iter().map(|a| infer_expr(a, ctx)).collect::<Result<_>>()?;
            let tkwargs: Vec<(String, TExpr)> = kwargs.iter()
                .map(|(k, v)| infer_expr(v, ctx).map(|tv| (k.clone(), tv)))
                .collect::<Result<_>>()?;
            // Builtin primitive methods have a fixed return type regardless of
            // the receiver type — so even `unknown.len()` reliably yields Int.
            // Without this, methods called on Unknown receivers (struct fields
            // whose types we don't track, untyped fn params) propagate Unknown
            // into subsequent operations, where prefer_known in emit_binop can
            // wrongly promote a small integer to a string pointer.
            let builtin_ret: Option<JadeType> = match callee.as_ref() {
                Expr::FieldAccess { field, .. } => match field.as_str() {
                    "len" => Some(JadeType::Int),
                    "has" | "contains" => Some(JadeType::Bool),
                    _ => None,
                },
                _ => None,
            };

            let ret_ty = if is_grammar_new {
                JadeType::Grammar
            } else if let Some(t) = pkg_call_ret {
                t
            } else if let Some(t) = builtin_ret {
                t
            } else {
                match &tcallee.ty {
                    JadeType::Fn { ret, .. }      => *ret.clone(),
                    JadeType::AsyncFn { ret, .. } => JadeType::Future(ret.clone()),
                    JadeType::Unknown             => JadeType::Unknown,
                    _                             => return Err(JadeError::NotCallable { span: *span }),
                }
            };
            Ok(TExpr {
                kind: TExprKind::Call { callee: Box::new(tcallee), args: targs, kwargs: tkwargs },
                ty: ret_ty,
                span: *span,
            })
        }

        // ── Struct literals ───────────────────────────────────────────────────

        Expr::StructLiteral { type_name, fields, span } => {
            // Clone field defs to release the borrow on ctx before calling infer_expr.
            // If the type is unknown but imports are present, fall back gracefully.
            let def_fields_opt = ctx.struct_defs.get(type_name).cloned();
            if def_fields_opt.is_none() && ctx.has_imports {
                // Type may be defined in an imported file; skip static checks.
                // Normalize the type name to its bare form (strip any module
                // prefix like "messages.Session" → "Session") so the literal
                // agrees with the StructDef registered after import flattening
                // and with method-body callsites that see the bare name.
                let bare = type_name.rsplit_once('.')
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_else(|| type_name.clone());
                let tfields = fields.iter()
                    .map(|(n, e)| infer_expr(e, ctx).map(|te| (n.clone(), te, false)))
                    .collect::<Result<Vec<_>>>()?;
                return Ok(TExpr {
                    kind: TExprKind::StructLiteral { type_name: bare.clone(), fields: tfields },
                    ty: JadeType::Struct(bare),
                    span: *span,
                });
            }
            let def_fields = def_fields_opt
                .ok_or_else(|| JadeError::UndefinedType { name: type_name.clone(), span: *span })?;

            // Extra fields check.
            for (fname, fexpr) in fields {
                let fspan = expr_span(fexpr);
                if !def_fields.iter().any(|f| f.name() == fname) {
                    return Err(JadeError::UndefinedField {
                        type_name: type_name.clone(),
                        field: fname.clone(),
                        span: fspan,
                    });
                }
            }

            // Required fields check.
            for def_field in &def_fields {
                if let StructFieldDef::Required(req) = def_field {
                    if !fields.iter().any(|(n, _)| n == req) {
                        return Err(JadeError::MissingField {
                            field: req.clone(),
                            span: *span,
                        });
                    }
                }
            }

            // Duplicate fields check.
            let mut seen = std::collections::HashSet::new();
            for (fname, fexpr) in fields {
                if !seen.insert(fname.as_str()) {
                    return Err(JadeError::DuplicateField {
                        field: fname.clone(),
                        span: expr_span(fexpr),
                    });
                }
            }

            // Build a quick lookup of caller-provided field expressions.
            let provided: HashMap<&str, &Expr> = fields
                .iter()
                .map(|(n, e)| (n.as_str(), e))
                .collect();

            // Walk definition order and produce a complete field list with
            // defaults resolved.  The `bool` flag marks prompt fields so
            // the bytecode emitter knows to wrap the value in Prompt(…).
            let mut tfields: Vec<(String, TExpr, bool)> = Vec::with_capacity(def_fields.len());
            for def_field in &def_fields {
                match def_field {
                    StructFieldDef::Required(name) => {
                        // Already validated above that it is provided.
                        let e = provided[name.as_str()];
                        tfields.push((name.clone(), infer_expr(e, ctx)?, false));
                    }
                    StructFieldDef::Let { name, default } => {
                        let e = provided.get(name.as_str()).copied().unwrap_or(default);
                        tfields.push((name.clone(), infer_expr(e, ctx)?, false));
                    }
                    StructFieldDef::Prompt { name, default } => {
                        let e = provided.get(name.as_str()).copied().unwrap_or(default);
                        let te = infer_expr(e, ctx)?;
                        if te.ty != JadeType::Str && te.ty != JadeType::Unknown {
                            return Err(JadeError::PromptFieldNotStr {
                                field: name.clone(),
                                span: te.span,
                            });
                        }
                        tfields.push((name.clone(), te, true));
                    }
                }
            }

            Ok(TExpr {
                kind: TExprKind::StructLiteral { type_name: type_name.clone(), fields: tfields },
                ty: JadeType::Struct(type_name.clone()),
                span: *span,
            })
        }

        // ── Field access ──────────────────────────────────────────────────────

        Expr::FieldAccess { object, field, span } => {
            let tobj = infer_expr(object, ctx)?;
            let ty = match &tobj.ty {
                JadeType::Struct(tn) => {
                    let tn = tn.clone();
                    // Skip validation for imported structs whose definitions are
                    // resolved in a separate file (and therefore aren't in this
                    // file's ctx.struct_defs).  Fields/methods can't be checked
                    // until the importer merges TIRs at codegen time.
                    let is_imported = ctx.has_imports && !ctx.struct_defs.contains_key(&tn);
                    if !is_imported {
                        let has_field = ctx.struct_defs.get(&tn)
                            .map(|defs| defs.iter().any(|f| f.name() == field))
                            .unwrap_or(false);
                        let has_method = ctx.extend_methods.get(&tn)
                            .map(|m| m.contains_key(field.as_str()))
                            .unwrap_or(false);
                        if !has_field && !has_method {
                            return Err(JadeError::UndefinedField {
                                type_name: tn,
                                field: field.clone(),
                                span: *span,
                            });
                        }
                    }
                    // Field/method value types are not tracked at Stage B — return Unknown.
                    JadeType::Unknown
                }
                // Primitive types: check registered primitive methods
                JadeType::Str => {
                    ctx.primitive_methods.get("str")
                        .and_then(|m| m.get(field.as_str()))
                        .cloned()
                        .unwrap_or(JadeType::Unknown)
                }
                JadeType::Array(_) => {
                    ctx.primitive_methods.get("array")
                        .and_then(|m| m.get(field.as_str()))
                        .cloned()
                        .unwrap_or(JadeType::Unknown)
                }
                JadeType::Dict => {
                    ctx.primitive_methods.get("dict")
                        .and_then(|m| m.get(field.as_str()))
                        .cloned()
                        .unwrap_or(JadeType::Unknown)
                }
                JadeType::Unknown => JadeType::Unknown,
                _ => JadeType::Unknown,
            };
            Ok(TExpr {
                kind: TExprKind::FieldAccess { object: Box::new(tobj), field: field.clone() },
                ty,
                span: *span,
            })
        }

        // ── Indexing ──────────────────────────────────────────────────────────

        Expr::Index { object, index, span } => {
            let tobj = infer_expr(object, ctx)?;
            let tidx = infer_expr(index, ctx)?;
            let ty = match &tobj.ty {
                JadeType::Array(elem_ty) => *elem_ty.clone(),
                JadeType::Dict           => JadeType::Unknown,
                JadeType::Str            => JadeType::Str,
                JadeType::Unknown        => JadeType::Unknown,
                other => return Err(JadeError::TypeMismatch {
                    expected: "array, dict, or str".to_string(),
                    got: jade_type_name(other),
                    span: *span,
                }),
            };
            Ok(TExpr {
                kind: TExprKind::Index { object: Box::new(tobj), index: Box::new(tidx) },
                ty,
                span: *span,
            })
        }

        // ── Array literals ────────────────────────────────────────────────────

        Expr::Array { elements, span } => {
            if elements.is_empty() {
                return Ok(TExpr {
                    kind: TExprKind::Array { elements: vec![] },
                    ty: JadeType::Array(Box::new(JadeType::Unknown)),
                    span: *span,
                });
            }

            let telems: Vec<TExpr> = elements.iter().map(|e| infer_expr(e, ctx)).collect::<Result<_>>()?;

            // Use the first concrete element type; fall back to Unknown for
            // heterogeneous arrays (arrays are untyped at runtime).
            let elem_ty = telems.iter()
                .find(|t| t.ty != JadeType::Unknown)
                .map(|t| t.ty.clone())
                .unwrap_or(JadeType::Unknown);

            // If elements have mixed concrete types, emit an error.
            let array_elem_ty = if elem_ty != JadeType::Unknown {
                if let Some(mismatch) = telems.iter().find(|t| t.ty != JadeType::Unknown && t.ty != elem_ty) {
                    return Err(JadeError::HeterogeneousArray {
                        first: jade_type_name(&elem_ty).to_string(),
                        got: jade_type_name(&mismatch.ty).to_string(),
                        span: *span,
                    });
                }
                elem_ty
            } else {
                elem_ty
            };

            Ok(TExpr {
                kind: TExprKind::Array { elements: telems },
                ty: JadeType::Array(Box::new(array_elem_ty)),
                span: *span,
            })
        }

        // ── Dict literals ─────────────────────────────────────────────────────

        Expr::Dict { entries, span } => {
            let tentries: Vec<(TExpr, TExpr)> = entries
                .iter()
                .map(|(k, v)| Ok((infer_expr(k, ctx)?, infer_expr(v, ctx)?)))
                .collect::<Result<_>>()?;
            Ok(TExpr {
                kind: TExprKind::Dict { entries: tentries },
                ty: JadeType::Dict,
                span: *span,
            })
        }

        // ── Closures ──────────────────────────────────────────────────────────

        Expr::Closure { params, body, span } => {
            ctx.push_scope();
            for param in params {
                ctx.define(param.clone(), JadeType::Unknown);
            }
            let tbody = check_stmts(body, ctx)?;
            // Collect free variables BEFORE popping scope so ctx.get() still resolves them.
            let captures = collect_captures(&tbody, params, ctx);
            ctx.pop_scope();
            Ok(TExpr {
                kind: TExprKind::Closure { params: params.clone(), body: tbody, captures },
                ty: JadeType::Fn {
                    params: params.iter().map(|_| JadeType::Unknown).collect(),
                    ret: Box::new(JadeType::Unknown),
                },
                span: *span,
            })
        }

        // ── Async await ───────────────────────────────────────────────────────

        Expr::Await { expr, span } => {
            let texpr = infer_expr(expr, ctx)?;
            let ty = match &texpr.ty {
                JadeType::Future(inner) => *inner.clone(),
                JadeType::Unknown       => JadeType::Unknown,
                other => return Err(JadeError::TypeMismatch {
                    expected: "future".to_string(),
                    got: jade_type_name(other),
                    span: *span,
                }),
            };
            Ok(TExpr {
                kind: TExprKind::Await { expr: Box::new(texpr) },
                ty,
                span: *span,
            })
        }

        // ── Prompt literal expression: `prompt <expr>` ───────────────────────

        Expr::PromptLiteral { body, span } => {
            let tbody = infer_expr(body, ctx)?;
            Ok(TExpr {
                kind: TExprKind::PromptLiteral { body: Box::new(tbody) },
                ty: JadeType::Prompt,
                span: *span,
            })
        }

        // ── LLM prompt dereference ────────────────────────────────────────────

        Expr::PromptDeref { expr, constraint, span } => {
            let texpr = infer_expr(expr, ctx)?;
            if texpr.ty != JadeType::Prompt && texpr.ty != JadeType::Unknown {
                return Err(JadeError::TypeMismatch {
                    expected: "prompt".to_string(),
                    got: jade_type_name(&texpr.ty),
                    span: *span,
                });
            }

            // Resolve the |> constraint: Grammar value → grammar_expr; type name string → output_type.
            //
            // Resolution order:
            // 1. Try `extract_type_name(c)` first.  If it yields a known built-in type keyword
            //    ("int", "float", "bool", "str", "array", "dict") or an uppercase struct name,
            //    use it directly as `output_type` — do NOT call `infer_expr`, which would fail
            //    with "undefined variable" for bare identifiers like `array` and `dict` that are
            //    valid type targets but are not in-scope variables.
            // 2. Otherwise call `infer_expr(c)`:
            //    a. If the result type is Grammar → grammar_expr path.
            //    b. If the result type is Unknown (e.g. `self.grammar` field on a struct whose
            //       field type is not statically tracked) → treat as a runtime Grammar value
            //       and emit it as grammar_expr.  This avoids the retry-loop that firing
            //       `output_type = Some("self.grammar")` would cause at runtime.
            //    c. Otherwise → error.
            let (output_type, grammar_texpr, result_ty) = match constraint {
                None => (None, None, JadeType::Str),
                Some(c) => {
                    if let Some(name) = extract_type_name(c) {
                        let is_builtin = matches!(
                            name.as_str(),
                            "int" | "float" | "bool" | "str" | "nil"
                            | "array" | "Array"
                            | "dict"  | "Dict"
                        );
                        let is_struct_name = name.chars().next()
                            .map(|ch| ch.is_uppercase())
                            .unwrap_or(false);
                        if is_builtin || is_struct_name {
                            // Known type keyword or struct name — use directly.
                            let ty = parse_type_name(&name);
                            (Some(name), None, ty)
                        } else {
                            // Might be a Grammar variable (e.g. `gram`) or a field path
                            // (e.g. `self.grammar`).  Infer it to determine which.
                            let tc = infer_expr(c, ctx)?;
                            if tc.ty == JadeType::Grammar {
                                // Statically known Grammar value.
                                (None, Some(Box::new(tc)), JadeType::Str)
                            } else {
                                // Unknown type (common for struct fields) — treat as a
                                // runtime Grammar expression rather than a coerce type name.
                                // This is the correct path for `?p |> self.grammar`.
                                (None, Some(Box::new(tc)), JadeType::Str)
                            }
                        }
                    } else {
                        // Not an identifier / dotted path (e.g. a call expression).
                        // Infer normally.
                        let tc = infer_expr(c, ctx)?;
                        if tc.ty == JadeType::Grammar || tc.ty == JadeType::Unknown {
                            (None, Some(Box::new(tc)), JadeType::Str)
                        } else {
                            return Err(JadeError::TypeMismatch {
                                expected: "type name or Grammar value after |>".to_string(),
                                got: jade_type_name(&tc.ty),
                                span: *span,
                            });
                        }
                    }
                }
            };

            Ok(TExpr {
                kind: TExprKind::PromptDeref {
                    expr: Box::new(texpr),
                    output_type,
                    grammar_expr: grammar_texpr,
                },
                ty: result_ty,
                span: *span,
            })
        }
    }
}

// ── Operator type rules ───────────────────────────────────────────────────────

fn infer_binop(op: &BinOpKind, lty: &JadeType, rty: &JadeType, span: Span) -> Result<JadeType> {
    use BinOpKind::*;
    use JadeType::*;

    // Unknown on either side propagates without error.
    if *lty == Unknown || *rty == Unknown {
        return Ok(Unknown);
    }

    match op {
        Add => match (lty, rty) {
            (Int,   Int)               => Ok(Int),
            (Float, Float)             => Ok(Float),
            (Int,   Float) | (Float, Int) => Ok(Float),
            (Str,   Str)               => Ok(Str),
            _ => Err(JadeError::TypeMismatch {
                expected: "int, float, or str on both sides of +".to_string(),
                got: format!("{} + {}", jade_type_name(lty), jade_type_name(rty)),
                span,
            }),
        },

        Sub | Mul | Div | Mod => match (lty, rty) {
            (Int,   Int)               => Ok(Int),
            (Float, Float)             => Ok(Float),
            (Int,   Float) | (Float, Int) => Ok(Float),
            _ => Err(JadeError::TypeMismatch {
                expected: "int or float".to_string(),
                got: format!("{} {} {}", jade_type_name(lty), op_symbol(op), jade_type_name(rty)),
                span,
            }),
        },

        BitAnd | BitOr | BitXor | Shl | Shr => match (lty, rty) {
            (Int, Int) => Ok(Int),
            _ => Err(JadeError::TypeMismatch {
                expected: "int".to_string(),
                got: format!("{} {} {}", jade_type_name(lty), op_symbol(op), jade_type_name(rty)),
                span,
            }),
        },

        And | Or => match (lty, rty) {
            (Bool, Bool) => Ok(Bool),
            _ => Err(JadeError::TypeMismatch {
                expected: "bool".to_string(),
                got: format!("{} {} {}", jade_type_name(lty), op_symbol(op), jade_type_name(rty)),
                span,
            }),
        },

        // Strict equality: both sides must be the same type (1 == 1.0 is a type error in Jade).
        Eq | Ne => {
            if lty == rty {
                Ok(Bool)
            } else {
                Err(JadeError::TypeMismatch {
                    expected: jade_type_name(lty),
                    got: jade_type_name(rty),
                    span,
                })
            }
        }

        // Ordering: Int/Float/Bool mixing is allowed (matches evaluator promotion rules).
        Lt | Gt | Le | Ge => match (lty, rty) {
            (Int, Int) | (Float, Float) | (Bool, Bool) | (Str, Str) => Ok(Bool),
            (Int, Float) | (Float, Int) => Ok(Bool),
            _ => Err(JadeError::TypeMismatch {
                expected: "comparable types".to_string(),
                got: format!("{} {} {}", jade_type_name(lty), op_symbol(op), jade_type_name(rty)),
                span,
            }),
        },

        // Membership: always returns bool; right-hand side type not statically checked.
        In | NotIn => Ok(Bool),
    }
}

fn infer_unaryop(op: &UnaryOpKind, ty: &JadeType, span: Span) -> Result<JadeType> {
    use UnaryOpKind::*;
    use JadeType::*;

    if *ty == Unknown {
        return Ok(Unknown);
    }

    match (op, ty) {
        (BitNot, Int)   => Ok(Int),
        (Not,    Bool)  => Ok(Bool),
        (Neg,    Int)   => Ok(Int),
        (Neg,    Float) => Ok(Float),
        _ => Err(JadeError::TypeMismatch {
            expected: match op {
                BitNot => "int".to_string(),
                Not    => "bool".to_string(),
                Neg    => "int or float".to_string(),
            },
            got: jade_type_name(ty),
            span,
        }),
    }
}

// ── Return type inference ─────────────────────────────────────────────────────

/// Infer types for decorator argument lists, preserving `(name, args)` pairs.
fn infer_decorators(
    decorators: &[(String, Vec<(Option<String>, crate::frontend::ast::Expr)>)],
    ctx: &mut TypeContext,
) -> Result<Vec<(String, Vec<(Option<String>, TExpr)>)>> {
    decorators.iter()
        .map(|(name, args)| {
            let targs = args.iter()
                .map(|(kw, a)| Ok((kw.clone(), infer_expr(a, ctx)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok((name.clone(), targs))
        })
        .collect()
}

/// Scan `stmts` for `Return` nodes to infer the function's return type.
/// Does not recurse into nested `FnDef` bodies (those are separate functions).
fn infer_return_type(stmts: &[TStmt]) -> JadeType {
    for stmt in stmts {
        match stmt {
            TStmt::Return { value: Some(texpr), .. } => return texpr.ty.clone(),
            TStmt::Return { value: None, .. }        => return JadeType::Nil,
            TStmt::If { then_body, else_body, .. } => {
                let t = infer_return_type(then_body);
                if t != JadeType::Unknown && t != JadeType::Nil { return t; }
                if let Some(eb) = else_body {
                    let t2 = infer_return_type(eb);
                    if t2 != JadeType::Unknown && t2 != JadeType::Nil { return t2; }
                }
            }
            TStmt::While { body, .. } | TStmt::For { body, .. } => {
                let t = infer_return_type(body);
                if t != JadeType::Unknown && t != JadeType::Nil { return t; }
            }
            TStmt::FnDef { .. } | TStmt::AsyncFnDef { .. } => {} // nested fn def — do not recurse
            _ => {}
        }
    }
    JadeType::Nil
}

// ── Capture analysis ─────────────────────────────────────────────────────────

/// Collect variables captured from enclosing scopes by the closure whose typed
/// body is `tbody`.  A variable is captured when it is:
///   1. referenced inside the body (as `TExprKind::Identifier`), AND
///   2. not a parameter of this closure, AND
///   3. not bound by a `let` or `for` inside the closure body, AND
///   4. not in the global scope (builtins, top-level functions).
fn collect_captures(tbody: &[TStmt], params: &[String], ctx: &TypeContext) -> Vec<(String, JadeType)> {
    let mut locals: HashSet<String> = params.iter().cloned().collect();
    let mut captures: Vec<(String, JadeType)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    collect_captures_in_stmts(tbody, &mut locals, &mut captures, &mut seen, ctx);
    captures
}

fn collect_captures_in_stmts(
    stmts: &[TStmt],
    locals: &mut HashSet<String>,
    captures: &mut Vec<(String, JadeType)>,
    seen: &mut HashSet<String>,
    ctx: &TypeContext,
) {
    for stmt in stmts {
        match stmt {
            TStmt::Let { name, value, .. } => {
                collect_captures_in_expr(value, locals, captures, seen, ctx);
                locals.insert(name.clone());
            }
            TStmt::Assign { value, .. } => {
                collect_captures_in_expr(value, locals, captures, seen, ctx);
            }
            TStmt::Return { value, .. } => {
                if let Some(e) = value { collect_captures_in_expr(e, locals, captures, seen, ctx); }
            }
            TStmt::Expr(e) => collect_captures_in_expr(e, locals, captures, seen, ctx),
            TStmt::If { condition, then_body, else_body, .. } => {
                collect_captures_in_expr(condition, locals, captures, seen, ctx);
                let mut tl = locals.clone();
                collect_captures_in_stmts(then_body, &mut tl, captures, seen, ctx);
                if let Some(eb) = else_body {
                    let mut el = locals.clone();
                    collect_captures_in_stmts(eb, &mut el, captures, seen, ctx);
                }
            }
            TStmt::While { condition, body, .. } => {
                collect_captures_in_expr(condition, locals, captures, seen, ctx);
                let mut bl = locals.clone();
                collect_captures_in_stmts(body, &mut bl, captures, seen, ctx);
            }
            TStmt::For { var, iterable, body, .. } => {
                collect_captures_in_expr(iterable, locals, captures, seen, ctx);
                let mut bl = locals.clone();
                bl.insert(var.clone());
                collect_captures_in_stmts(body, &mut bl, captures, seen, ctx);
            }
            TStmt::FnDef { name, .. } | TStmt::AsyncFnDef { name, .. } => {
                // Nested fn defs are separate scopes; just mark the name as locally bound.
                locals.insert(name.clone());
            }
            TStmt::FieldAssign { object, value, .. } => {
                maybe_capture(object, locals, captures, seen, ctx);
                collect_captures_in_expr(value, locals, captures, seen, ctx);
            }
            TStmt::IndexAssign { name, index, value, .. } => {
                maybe_capture(name, locals, captures, seen, ctx);
                collect_captures_in_expr(index, locals, captures, seen, ctx);
                collect_captures_in_expr(value, locals, captures, seen, ctx);
            }
            TStmt::Raise { value, .. } => {
                collect_captures_in_expr(value, locals, captures, seen, ctx);
            }
            TStmt::TryCatch { body, arms, .. } => {
                let mut bl = locals.clone();
                collect_captures_in_stmts(body, &mut bl, captures, seen, ctx);
                for arm in arms {
                    let mut al = locals.clone();
                    al.insert(arm.binding.clone());
                    collect_captures_in_stmts(&arm.body, &mut al, captures, seen, ctx);
                }
            }
            TStmt::PromptDecl { name, body, .. } => {
                collect_captures_in_expr(body, locals, captures, seen, ctx);
                locals.insert(name.clone());
            }
            TStmt::StructDef { .. } | TStmt::InterfaceDef { .. }
            | TStmt::ExtendBlock { .. } | TStmt::Use { .. } | TStmt::FromUse { .. } => {}
        }
    }
}

fn collect_captures_in_expr(
    expr: &TExpr,
    locals: &mut HashSet<String>,
    captures: &mut Vec<(String, JadeType)>,
    seen: &mut HashSet<String>,
    ctx: &TypeContext,
) {
    match &expr.kind {
        TExprKind::Identifier(name) => maybe_capture(name, locals, captures, seen, ctx),
        TExprKind::BinOp { left, right, .. } => {
            collect_captures_in_expr(left, locals, captures, seen, ctx);
            collect_captures_in_expr(right, locals, captures, seen, ctx);
        }
        TExprKind::UnaryOp { operand, .. } => collect_captures_in_expr(operand, locals, captures, seen, ctx),
        TExprKind::Call { callee, args, kwargs } => {
            collect_captures_in_expr(callee, locals, captures, seen, ctx);
            for a in args { collect_captures_in_expr(a, locals, captures, seen, ctx); }
            for (_, v) in kwargs { collect_captures_in_expr(v, locals, captures, seen, ctx); }
        }
        TExprKind::Array { elements } => {
            for e in elements { collect_captures_in_expr(e, locals, captures, seen, ctx); }
        }
        TExprKind::Index { object, index } => {
            collect_captures_in_expr(object, locals, captures, seen, ctx);
            collect_captures_in_expr(index, locals, captures, seen, ctx);
        }
        TExprKind::FieldAccess { object, .. } => collect_captures_in_expr(object, locals, captures, seen, ctx),
        TExprKind::StructLiteral { fields, .. } => {
            for (_, e, _) in fields { collect_captures_in_expr(e, locals, captures, seen, ctx); }
        }
        TExprKind::FStr { parts } => {
            for p in parts {
                if let TFStrPart::Expr(e) = p { collect_captures_in_expr(e, locals, captures, seen, ctx); }
            }
        }
        TExprKind::Dict { entries } => {
            for (k, v) in entries {
                collect_captures_in_expr(k, locals, captures, seen, ctx);
                collect_captures_in_expr(v, locals, captures, seen, ctx);
            }
        }
        TExprKind::Await { expr } => collect_captures_in_expr(expr, locals, captures, seen, ctx),
        TExprKind::PromptDeref { expr, grammar_expr, .. } => {
            collect_captures_in_expr(expr, locals, captures, seen, ctx);
            if let Some(g) = grammar_expr { collect_captures_in_expr(g, locals, captures, seen, ctx); }
        }
        TExprKind::PromptLiteral { body } => collect_captures_in_expr(body, locals, captures, seen, ctx),
        TExprKind::Closure { params: inner_params, captures: inner_caps, .. } => {
            // The inner closure already resolved its own captures.
            // Propagate any that come from OUR enclosing scope.
            for (name, ty) in inner_caps {
                if !inner_params.contains(name) {
                    maybe_capture_typed(name, ty, locals, captures, seen, ctx);
                }
            }
        }
        // Literals carry no identifiers.
        TExprKind::Integer(_) | TExprKind::Float(_) | TExprKind::Bool(_) | TExprKind::Str(_) => {}
    }
}

/// Add `name` to captures if it is a free variable in this closure scope.
fn maybe_capture(
    name: &str,
    locals: &HashSet<String>,
    captures: &mut Vec<(String, JadeType)>,
    seen: &mut HashSet<String>,
    ctx: &TypeContext,
) {
    if locals.contains(name) || seen.contains(name) || ctx.is_in_global_scope(name) {
        return;
    }
    if let Some(ty) = ctx.get(name) {
        captures.push((name.to_string(), ty));
        seen.insert(name.to_string());
    }
}

/// Same as `maybe_capture` but uses a known type (from an already-resolved inner capture).
fn maybe_capture_typed(
    name: &str,
    ty: &JadeType,
    locals: &HashSet<String>,
    captures: &mut Vec<(String, JadeType)>,
    seen: &mut HashSet<String>,
    ctx: &TypeContext,
) {
    if locals.contains(name) || seen.contains(name) || ctx.is_in_global_scope(name) {
        return;
    }
    captures.push((name.to_string(), ty.clone()));
    seen.insert(name.to_string());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn require_bool_or_unknown(ty: &JadeType, span: Span) -> Result<()> {
    if *ty != JadeType::Bool && *ty != JadeType::Unknown {
        Err(JadeError::TypeMismatch {
            expected: "bool".to_string(),
            got: jade_type_name(ty),
            span,
        })
    } else {
        Ok(())
    }
}

pub fn jade_type_name(ty: &JadeType) -> String {
    match ty {
        JadeType::Int          => "int".to_string(),
        JadeType::Float        => "float".to_string(),
        JadeType::Bool         => "bool".to_string(),
        JadeType::Str          => "str".to_string(),
        JadeType::Nil          => "nil".to_string(),
        JadeType::Prompt       => "prompt".to_string(),
        JadeType::Grammar      => "grammar".to_string(),
        JadeType::Array(elem)  => format!("[{}]", jade_type_name(elem)),
        JadeType::Dict         => "dict".to_string(),
        JadeType::Struct(name) => name.clone(),
        JadeType::Fn { .. }       => "fn".to_string(),
        JadeType::AsyncFn { .. }  => "async fn".to_string(),
        JadeType::Future(inner)   => format!("future<{}>", jade_type_name(inner)),
        JadeType::Unknown         => "unknown".to_string(),
    }
}

fn parse_type_name(s: &str) -> JadeType {
    match s {
        "int"   => JadeType::Int,
        "float" => JadeType::Float,
        "bool"  => JadeType::Bool,
        "str"   => JadeType::Str,
        "nil"   => JadeType::Nil,
        _       => JadeType::Unknown,
    }
}

/// Extract a dotted type-name string from an AST expression used as a `|>` constraint.
/// Returns `None` if the expression is not a plain identifier or dotted field path.
fn extract_type_name(expr: &crate::frontend::ast::Expr) -> Option<String> {
    use crate::frontend::ast::Expr;
    match expr {
        Expr::Identifier { name, .. } => Some(name.clone()),
        Expr::FieldAccess { object, field, .. } => {
            let base = extract_type_name(object)?;
            Some(format!("{}.{}", base, field))
        }
        _ => None,
    }
}

fn op_symbol(op: &BinOpKind) -> &'static str {
    use BinOpKind::*;
    match op {
        Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%",
        BitAnd => "&", BitOr => "|", BitXor => "^", Shl => "<<", Shr => ">>",
        And => "&&", Or => "||",
        Eq => "==", Ne => "!=",
        Lt => "<", Gt => ">", Le => "<=", Ge => ">=",
        In => "in", NotIn => "not in",
    }
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Integer { span, .. } | Expr::Float { span, .. } | Expr::Bool { span, .. }
        | Expr::Str { span, .. } | Expr::Identifier { span, .. } | Expr::Call { span, .. }
        | Expr::BinOp { span, .. } | Expr::UnaryOp { span, .. }
        | Expr::StructLiteral { span, .. } | Expr::FieldAccess { span, .. }
        | Expr::Index { span, .. } | Expr::Array { span, .. } | Expr::FStr { span, .. }
        | Expr::PromptLiteral { span, .. } | Expr::PromptDeref { span, .. }
        | Expr::Dict { span, .. } | Expr::Closure { span, .. } | Expr::Await { span, .. } => *span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "type_infer_tests.rs"]
mod tests;
