use std::collections::HashMap;

use crate::interpreter::{
    ast::{BinOpKind, Expr, FStrPart, Program, StructFieldDef, Stmt, UnaryOpKind},
    error::{JadeError, Result, Span},
};
use super::tir::{JadeType, TExpr, TExprKind, TFStrPart, TProgram, TStmt};

// ── TypeContext ───────────────────────────────────────────────────────────────

/// The type environment maintained during inference.
///
/// Uses a scope stack matching the evaluator's scoping: `scopes[0]` is global,
/// `scopes.last()` is the innermost. Struct and interface definitions are stored
/// in flat maps (they are always global in the current language).
struct TypeContext {
    /// Variable name → resolved type, innermost scope last.
    scopes: Vec<HashMap<String, JadeType>>,
    /// Struct type name → field definitions (copied from AST).
    struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Interface name → required method names.
    interface_defs: HashMap<String, Vec<String>>,
    /// Extend: type_name → method_name → inferred return type.
    extend_methods: HashMap<String, HashMap<String, JadeType>>,
    /// True if this program contains any `use` statements. When true, unknown
    /// identifiers are treated as `Unknown` (resolved at VM runtime) rather
    /// than hard errors — symbols may be provided by the imports.
    has_imports: bool,
}

impl TypeContext {
    fn new() -> Self {
        let mut ctx = TypeContext {
            scopes: vec![HashMap::new()],
            struct_defs: HashMap::new(),
            interface_defs: HashMap::new(),
            extend_methods: HashMap::new(),
            has_imports: false,
        };
        // Built-in functions. `print` is variadic in practice, but we give it
        // one Unknown param so the return type (Nil) is always resolved.
        ctx.define("print".to_string(), JadeType::Fn {
            params: vec![JadeType::Unknown],
            ret: Box::new(JadeType::Nil),
        });
        ctx.define("len".to_string(), JadeType::Fn {
            params: vec![JadeType::Unknown],
            ret: Box::new(JadeType::Int),
        });
        // LLM session builtins that the evaluator always populates.
        ctx.define("__tokens__".to_string(), JadeType::Int);
        ctx.define("__model__".to_string(), JadeType::Str);
        ctx.define("__max_retries__".to_string(), JadeType::Int);
        ctx.define("__retry_log__".to_string(), JadeType::Array(Box::new(JadeType::Unknown)));
        ctx
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
    fn define(&mut self, name: String, ty: JadeType) {
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
pub fn infer(program: Program) -> Result<TProgram> {
    let mut ctx = TypeContext::new();
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
            Stmt::StructDef { name, fields, .. } => {
                ctx.struct_defs.insert(name.clone(), fields.clone());
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
            Stmt::Use { .. } => {
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

        Stmt::FnDef { name, params, body, span } => {
            ctx.push_scope();
            for param in params {
                ctx.define(param.clone(), JadeType::Unknown);
            }
            let tbody = check_stmts(body, ctx)?;
            ctx.pop_scope();

            let ret_ty = infer_return_type(&tbody);
            // Update fn entry with the now-resolved return type.
            ctx.define(name.clone(), JadeType::Fn {
                params: vec![JadeType::Unknown; params.len()],
                ret: Box::new(ret_ty.clone()),
            });

            Ok(TStmt::FnDef {
                name: name.clone(),
                params: params.clone(),
                body: tbody,
                ret_ty,
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
                    op: format!("cannot iterate over {}", jade_type_name(other)),
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

        Stmt::StructDef { name, fields, span } => {
            // Already registered in pre_pass; re-emit verbatim.
            Ok(TStmt::StructDef { name: name.clone(), fields: fields.clone(), span: *span })
        }

        Stmt::InterfaceDef { name, methods, span } => {
            Ok(TStmt::InterfaceDef { name: name.clone(), methods: methods.clone(), span: *span })
        }

        Stmt::ExtendBlock { type_name, interface_name, methods, span } => {
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

            Ok(TStmt::ExtendBlock {
                type_name: type_name.clone(),
                interface_name: interface_name.clone(),
                methods: tmethods,
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

        Stmt::Use { path, span } => {
            // Pass through unchanged; the import is resolved at VM runtime.
            Ok(TStmt::Use { path: path.clone(), span: *span })
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

        Expr::Call { callee, args, span } => {
            let tcallee = infer_expr(callee, ctx)?;
            let targs: Vec<TExpr> = args.iter().map(|a| infer_expr(a, ctx)).collect::<Result<_>>()?;
            let ret_ty = match &tcallee.ty {
                JadeType::Fn { ret, .. } => *ret.clone(),
                JadeType::Unknown        => JadeType::Unknown,
                _                        => return Err(JadeError::NotCallable { span: *span }),
            };
            Ok(TExpr {
                kind: TExprKind::Call { callee: Box::new(tcallee), args: targs },
                ty: ret_ty,
                span: *span,
            })
        }

        // ── Struct literals ───────────────────────────────────────────────────

        Expr::StructLiteral { type_name, fields, span } => {
            // Clone field defs to release the borrow on ctx before calling infer_expr.
            let def_fields = ctx.struct_defs.get(type_name)
                .ok_or_else(|| JadeError::UndefinedType { name: type_name.clone(), span: *span })?
                .clone();

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
                        tfields.push((name.clone(), infer_expr(e, ctx)?, true));
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
                    // Check field/method exists on the struct.
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
                    // Field/method value types are not tracked at Stage B — return Unknown.
                    JadeType::Unknown
                }
                JadeType::Unknown => JadeType::Unknown,
                _ => return Err(JadeError::TypeMismatch {
                    expected: "struct".to_string(),
                    got: jade_type_name(&tobj.ty),
                    span: *span,
                }),
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

            // Find the first concrete (non-Unknown) element type.
            let elem_ty = telems.iter()
                .find(|t| t.ty != JadeType::Unknown)
                .map(|t| t.ty.clone())
                .unwrap_or(JadeType::Unknown);

            // All other concrete elements must match the first.
            if elem_ty != JadeType::Unknown {
                for telem in &telems {
                    if telem.ty != JadeType::Unknown && telem.ty != elem_ty {
                        return Err(JadeError::HeterogeneousArray {
                            first: jade_type_name(&elem_ty),
                            got: jade_type_name(&telem.ty),
                            span: telem.span,
                        });
                    }
                }
            }

            Ok(TExpr {
                kind: TExprKind::Array { elements: telems },
                ty: JadeType::Array(Box::new(elem_ty)),
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

        // ── LLM prompt dereference ────────────────────────────────────────────

        Expr::PromptDeref { expr, output_type, span } => {
            let texpr = infer_expr(expr, ctx)?;
            if texpr.ty != JadeType::Prompt && texpr.ty != JadeType::Unknown {
                return Err(JadeError::TypeMismatch {
                    expected: "prompt".to_string(),
                    got: jade_type_name(&texpr.ty),
                    span: *span,
                });
            }
            let result_ty = match output_type.as_deref() {
                Some(s) => parse_type_name(s),
                None    => JadeType::Str,
            };
            Ok(TExpr {
                kind: TExprKind::PromptDeref { expr: Box::new(texpr), output_type: output_type.clone() },
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
            TStmt::FnDef { .. } => {} // nested fn def — do not recurse
            _ => {}
        }
    }
    JadeType::Nil
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
        JadeType::Array(elem)  => format!("[{}]", jade_type_name(elem)),
        JadeType::Dict         => "dict".to_string(),
        JadeType::Struct(name) => name.clone(),
        JadeType::Fn { .. }    => "fn".to_string(),
        JadeType::Unknown      => "unknown".to_string(),
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

fn op_symbol(op: &BinOpKind) -> &'static str {
    use BinOpKind::*;
    match op {
        Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%",
        BitAnd => "&", BitOr => "|", BitXor => "^", Shl => "<<", Shr => ">>",
        And => "&&", Or => "||",
        Eq => "==", Ne => "!=",
        Lt => "<", Gt => ">", Le => "<=", Ge => ">=",
    }
}

fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Integer { span, .. } | Expr::Float { span, .. } | Expr::Bool { span, .. }
        | Expr::Str { span, .. } | Expr::Identifier { span, .. } | Expr::Call { span, .. }
        | Expr::BinOp { span, .. } | Expr::UnaryOp { span, .. }
        | Expr::StructLiteral { span, .. } | Expr::FieldAccess { span, .. }
        | Expr::Index { span, .. } | Expr::Array { span, .. } | Expr::FStr { span, .. }
        | Expr::PromptDeref { span, .. } | Expr::Dict { span, .. } => *span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{lexer, parser};

    fn infer_src(src: &str) -> Result<TProgram> {
        let tokens = lexer::tokenize(src).expect("lex");
        let program = parser::parse(tokens).expect("parse");
        infer(program)
    }

    fn infer_ok(src: &str) -> TProgram {
        infer_src(src).expect("type check failed")
    }

    fn infer_err(src: &str) -> JadeError {
        infer_src(src).expect_err("expected type error")
    }

    // ── Literals ──────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_int_literal() {
        let tp = infer_ok("let x = 42");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    #[test]
    fn test_infer_float_literal() {
        let tp = infer_ok("let x = 3.14");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Float);
    }

    #[test]
    fn test_infer_bool_literal() {
        let tp = infer_ok("let x = true");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Bool);
    }

    #[test]
    fn test_infer_str_literal() {
        let tp = infer_ok(r#"let x = "hello""#);
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Str);
    }

    // ── Arithmetic ────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_int_add() {
        let tp = infer_ok("let x = 1 + 2");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    #[test]
    fn test_infer_float_add() {
        let tp = infer_ok("let x = 1.0 + 2.0");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Float);
    }

    #[test]
    fn test_infer_mixed_add_promotes_to_float() {
        let tp = infer_ok("let x = 1 + 2.0");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Float);
    }

    #[test]
    fn test_infer_str_concat() {
        let tp = infer_ok(r#"let x = "a" + "b""#);
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Str);
    }

    #[test]
    fn test_infer_int_plus_str_is_error() {
        let err = infer_err(r#"let x = 1 + "a""#);
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    // ── Logical and comparison ─────────────────────────────────────────────────

    #[test]
    fn test_infer_logical_and() {
        let tp = infer_ok("let x = true && false");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Bool);
    }

    #[test]
    fn test_infer_and_non_bool_is_error() {
        let err = infer_err("let x = 1 && 0");
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    #[test]
    fn test_infer_comparison_int() {
        let tp = infer_ok("let x = 1 < 2");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Bool);
    }

    #[test]
    fn test_infer_strict_equality_cross_type_is_error() {
        let err = infer_err("let x = 1 == 1.0");
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    // ── Unary operators ───────────────────────────────────────────────────────

    #[test]
    fn test_infer_unary_neg_int() {
        let tp = infer_ok("let x = -5");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    #[test]
    fn test_infer_unary_not_bool() {
        let tp = infer_ok("let x = !true");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Bool);
    }

    #[test]
    fn test_infer_unary_not_int_is_error() {
        let err = infer_err("let x = !1");
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    // ── Arrays ────────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_array_int() {
        let tp = infer_ok("let a = [1, 2, 3]");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Int)));
    }

    #[test]
    fn test_infer_array_empty() {
        let tp = infer_ok("let a = []");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Array(Box::new(JadeType::Unknown)));
    }

    #[test]
    fn test_infer_heterogeneous_array_is_error() {
        let err = infer_err(r#"let a = [1, "hello"]"#);
        assert!(matches!(err, JadeError::HeterogeneousArray { .. }));
    }

    // ── Control flow ──────────────────────────────────────────────────────────

    #[test]
    fn test_infer_if_bool_condition() {
        infer_ok("if true {\n let x = 1\n}");
    }

    #[test]
    fn test_infer_if_int_condition_is_error() {
        let err = infer_err("if 1 {\n let x = 2\n}");
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    #[test]
    fn test_infer_while_bool_condition() {
        infer_ok("while false {\n let x = 1\n}");
    }

    // ── Functions ─────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_fn_return_type_int() {
        let tp = infer_ok("fn add(x, y) {\n return 42\n}");
        let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(*ret_ty, JadeType::Int);
    }

    #[test]
    fn test_infer_fn_no_return_is_nil() {
        let tp = infer_ok("fn noop(x) {\n let y = 1\n}");
        let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(*ret_ty, JadeType::Nil);
    }

    #[test]
    fn test_infer_call_return_type() {
        let tp = infer_ok("fn id(x) {\n return 1\n}\nlet r = id(99)");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    #[test]
    fn test_infer_call_builtin_len() {
        let tp = infer_ok("let n = len([1, 2])");
        let TStmt::Let { value, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    // ── Structs ───────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_struct_literal_type() {
        let tp = infer_ok("struct Point {\n x,\n y\n}\nlet p = Point { x: 1, y: 2 }");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Struct("Point".to_string()));
    }

    #[test]
    fn test_infer_struct_undefined_type_is_error() {
        let err = infer_err("let p = Foo { x: 1 }");
        assert!(matches!(err, JadeError::UndefinedType { .. }));
    }

    #[test]
    fn test_infer_struct_missing_field_is_error() {
        let err = infer_err("struct P {\n x,\n y\n}\nlet p = P { x: 1 }");
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn test_infer_struct_extra_field_is_error() {
        let err = infer_err("struct P {\n x\n}\nlet p = P { x: 1, z: 2 }");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    // ── Prompts ───────────────────────────────────────────────────────────────

    #[test]
    fn test_infer_prompt_decl_type() {
        let tp = infer_ok(r#"prompt p = "hello""#);
        let TStmt::PromptDecl { .. } = &tp.stmts[0] else { panic!() };
    }

    #[test]
    fn test_infer_prompt_decl_non_str_is_error() {
        let err = infer_err("prompt p = 42");
        assert!(matches!(err, JadeError::TypeMismatch { .. }));
    }

    #[test]
    fn test_infer_prompt_deref_untyped_is_str() {
        let tp = infer_ok("prompt p = \"hi\"\nlet r = ?p");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Str);
    }

    #[test]
    fn test_infer_prompt_deref_typed_int() {
        let tp = infer_ok("prompt p = \"hi\"\nlet r = ?p |> int");
        let TStmt::Let { value, .. } = &tp.stmts[1] else { panic!() };
        assert_eq!(value.ty, JadeType::Int);
    }

    // ── Undefined variable ────────────────────────────────────────────────────

    #[test]
    fn test_infer_undefined_variable_is_error() {
        let err = infer_err("let x = y + 1");
        assert!(matches!(err, JadeError::UndefinedVariable { .. }));
    }

    // ── Unknown propagation ───────────────────────────────────────────────────

    #[test]
    fn test_infer_unknown_param_propagates() {
        // Function body with unannotated params: operator with Unknown propagates to Unknown.
        let tp = infer_ok("fn add(x, y) {\n return x + y\n}");
        let TStmt::FnDef { ret_ty, .. } = &tp.stmts[0] else { panic!() };
        assert_eq!(*ret_ty, JadeType::Unknown);
    }
}
