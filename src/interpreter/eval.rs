use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    ast::{BinOpKind, Expr, FStrPart, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result, Span},
};

// ── Value ────────────────────────────────────────────────────────────────────

/// A runtime value in Jade.
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Fn(Rc<FnValue>),
    /// A struct instance. Wrapped in `Rc<RefCell<…>>` so that all references
    /// to the same instance (including `self` inside methods) share mutable state.
    Struct(Rc<RefCell<StructInstance>>),
    /// A method bound to a specific receiver instance.
    BoundMethod(Rc<BoundMethod>),
    /// A built-in function (e.g. `print`).
    Builtin(BuiltinFn),
}

/// Identifies a built-in function by name.
#[derive(Clone, Debug)]
pub enum BuiltinFn {
    Print,
}

/// Heap-allocated function body shared via `Rc`.
pub struct FnValue {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

/// A struct instance at runtime.
pub struct StructInstance {
    pub type_name: String,
    pub fields: HashMap<String, Value>,
}

/// A method together with the instance it was accessed through.
pub struct BoundMethod {
    pub receiver: Rc<RefCell<StructInstance>>,
    pub method: Rc<FnValue>,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(i)   => write!(f, "Int({})", i),
            Value::Float(v) => write!(f, "Float({})", v),
            Value::Bool(b)  => write!(f, "Bool({})", b),
            Value::Str(s)   => write!(f, "Str({:?})", s),
            Value::Fn(fv)   => write!(f, "Fn({})", fv.params.join(", ")),
            Value::Struct(rc) => {
                let inst = rc.borrow();
                write!(f, "{} {{", inst.type_name)?;
                let mut first = true;
                for (k, v) in &inst.fields {
                    if !first { write!(f, ", ")?; }
                    write!(f, "{}: {:?}", k, v)?;
                    first = false;
                }
                write!(f, "}}")
            }
            Value::BoundMethod(_) => write!(f, "<bound method>"),
            Value::Builtin(b)     => write!(f, "<builtin {:?}>", b),
        }
    }
}

impl std::fmt::Debug for FnValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<fn({})>", self.params.join(", "))
    }
}

// ── Environment ──────────────────────────────────────────────────────────────

/// Scoped environment: a stack of hash maps. `scopes[0]` is the global scope;
/// each function call and block body pushes/pops a new frame.
/// Struct definitions and extend methods are global (not scoped).
#[derive(Debug)]
pub struct Env {
    scopes: Vec<HashMap<String, Value>>,
    /// Maps struct type names to their ordered list of field names.
    pub struct_defs: HashMap<String, Vec<String>>,
    /// Maps struct type names to their method tables.
    pub extend_methods: HashMap<String, HashMap<String, Rc<FnValue>>>,
    /// Built-in functions that are always in scope. Stored separately so they
    /// don't appear in `-v` verbose output alongside user variables.
    builtins: HashMap<String, BuiltinFn>,
}

impl Env {
    /// Create a new environment with one (global) scope and all built-ins pre-registered.
    pub fn new() -> Self {
        let mut builtins = HashMap::new();
        builtins.insert("print".to_string(), BuiltinFn::Print);
        Env {
            scopes: vec![HashMap::new()],
            struct_defs: HashMap::new(),
            extend_methods: HashMap::new(),
            builtins,
        }
    }

    /// Push a new inner scope (on function call or block entry).
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pop the innermost scope (on function return or block exit).
    pub fn pop_scope(&mut self) {
        debug_assert!(self.scopes.len() > 1, "pop_scope called on global scope");
        self.scopes.pop();
    }

    /// Bind `name` in the innermost scope — used for `let` and function params.
    pub fn define(&mut self, name: String, value: Value) {
        self.scopes.last_mut().unwrap().insert(name, value);
    }

    /// Update an existing binding anywhere in the scope chain — used for bare `x = expr`.
    /// Returns `UndefinedVariable` if `name` was never declared in any enclosing scope.
    pub fn assign(&mut self, name: &str, value: Value, span: Span) -> Result<()> {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), value);
                return Ok(());
            }
        }
        Err(JadeError::UndefinedVariable { name: name.to_string(), span })
    }

    /// Look up a variable, searching from innermost to outermost scope,
    /// then falling back to built-in functions.
    pub fn get(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        self.builtins.get(name).map(|b| Value::Builtin(b.clone()))
    }

    /// Iterate over all top-level (global) bindings — used by `-v` output.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.scopes[0].iter()
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Walk the program and return the populated top-level environment.
pub fn evaluate(program: Program) -> Result<Env> {
    let mut env = Env::new();
    eval_block(&program.stmts, &mut env)?;
    Ok(env)
}

// ── Statement evaluator ───────────────────────────────────────────────────────

/// Evaluate a list of statements in `env`.
/// Returns `Some(value)` if a `return` was executed, `None` otherwise.
fn eval_block(stmts: &[Stmt], env: &mut Env) -> Result<Option<Value>> {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = eval_expr(value, env)?;
                env.define(name.clone(), v);
            }

            Stmt::Assign { name, value, span } => {
                let v = eval_expr(value, env)?;
                env.assign(name, v, *span)?;
            }

            Stmt::FnDef { name, params, body, .. } => {
                let fn_val = FnValue {
                    params: params.clone(),
                    body: body.clone(),
                };
                env.define(name.clone(), Value::Fn(Rc::new(fn_val)));
            }

            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(expr) => eval_expr(expr, env)?,
                    // DESIGN: bare `return` yields Int(0) until a Unit type is added.
                    None => Value::Int(0),
                };
                return Ok(Some(v));
            }

            Stmt::While { condition, body, span } => {
                loop {
                    let cond = eval_expr(condition, env)?;
                    match cond {
                        Value::Bool(true) => {
                            env.push_scope();
                            let result = eval_block(body, env);
                            env.pop_scope();
                            if let Some(v) = result? {
                                return Ok(Some(v));
                            }
                        }
                        Value::Bool(false) => break,
                        _ => return Err(JadeError::TypeError {
                            op: "while".to_string(),
                            span: *span,
                        }),
                    }
                }
            }

            Stmt::If { condition, then_body, else_body, span } => {
                let cond = eval_expr(condition, env)?;
                match cond {
                    Value::Bool(true) => {
                        env.push_scope();
                        let result = eval_block(then_body, env);
                        env.pop_scope();
                        if let Some(v) = result? {
                            return Ok(Some(v));
                        }
                    }
                    Value::Bool(false) => {
                        if let Some(body) = else_body {
                            env.push_scope();
                            let result = eval_block(body, env);
                            env.pop_scope();
                            if let Some(v) = result? {
                                return Ok(Some(v));
                            }
                        }
                    }
                    _ => return Err(JadeError::TypeError {
                        op: "if".to_string(),
                        span: *span,
                    }),
                }
            }

            Stmt::StructDef { name, fields, .. } => {
                env.struct_defs.insert(name.clone(), fields.clone());
            }

            Stmt::ExtendBlock { type_name, methods, .. } => {
                let method_map = env.extend_methods.entry(type_name.clone()).or_default();
                for method in methods {
                    if let Stmt::FnDef { name, params, body, .. } = method {
                        method_map.insert(name.clone(), Rc::new(FnValue {
                            params: params.clone(),
                            body: body.clone(),
                        }));
                    }
                }
            }

            Stmt::FieldAssign { object, field, value, span } => {
                let v = eval_expr(value, env)?;
                let obj_val = env.get(object).ok_or_else(|| JadeError::UndefinedVariable {
                    name: object.clone(),
                    span: *span,
                })?;
                match obj_val {
                    Value::Struct(rc) => {
                        {
                            let b = rc.borrow();
                            if !b.fields.contains_key(field) {
                                return Err(JadeError::UndefinedField {
                                    type_name: b.type_name.clone(),
                                    field: field.clone(),
                                    span: *span,
                                });
                            }
                        } // `b` dropped here, freeing the immutable borrow before borrow_mut
                        rc.borrow_mut().fields.insert(field.clone(), v);
                    }
                    _ => return Err(JadeError::NotAStruct { span: *span }),
                }
            }

            Stmt::Expr(expr) => {
                eval_expr(expr, env)?;
            }
        }
    }
    Ok(None)
}

// ── Expression evaluator ─────────────────────────────────────────────────────

/// Widen an integer to float for mixed-type arithmetic.
/// This match is exhaustive over all current `Value` variants. If a new
/// variant is added, the compiler will require it to be handled here — it
/// will not silently fall through.
fn to_float(v: Value) -> f64 {
    match v {
        Value::Int(i)   => i as f64,
        Value::Float(f) => f,
        // Callers must guard against non-numeric values before calling to_float.
        Value::Bool(_)        => unreachable!("to_float called on Bool"),
        Value::Str(_)         => unreachable!("to_float called on Str"),
        Value::Fn(_)          => unreachable!("to_float called on Fn"),
        Value::Struct(_)      => unreachable!("to_float called on Struct"),
        Value::BoundMethod(_) => unreachable!("to_float called on BoundMethod"),
        Value::Builtin(_)     => unreachable!("to_float called on Builtin"),
    }
}

/// Convert a `Value` to its string representation (used by f-string interpolation
/// and the `print` built-in).  Matches the same display rules as verbose output.
fn value_to_str(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            let s = format!("{}", f);
            // Mirror the `.0` suffix logic from cli/run.rs
            if s.chars().all(|c| c.is_ascii_digit() || c == '-') {
                format!("{}.0", s)
            } else {
                s
            }
        }
        Value::Bool(b)        => b.to_string(),
        Value::Str(s)         => s.clone(),
        Value::Fn(_)          => "<fn>".to_string(),
        Value::Struct(_)      => "<struct>".to_string(),
        Value::BoundMethod(_) => "<bound method>".to_string(),
        Value::Builtin(_)     => "<builtin>".to_string(),
    }
}

/// Evaluate one expression against the current environment, returning its value.
fn eval_expr(expr: &Expr, env: &mut Env) -> Result<Value> {
    match expr {
        Expr::Integer { value, .. } => Ok(Value::Int(*value)),
        Expr::Float   { value, .. } => Ok(Value::Float(*value)),
        Expr::Bool    { value, .. } => Ok(Value::Bool(*value)),
        Expr::Str     { value, .. } => Ok(Value::Str(value.clone())),

        Expr::FStr { parts, .. } => {
            let mut result = String::new();
            for part in parts {
                match part {
                    FStrPart::Literal(s) => result.push_str(s),
                    FStrPart::Expr(expr) => {
                        let v = eval_expr(expr, env)?;
                        result.push_str(&value_to_str(&v));
                    }
                }
            }
            Ok(Value::Str(result))
        }

        Expr::Identifier { name, span } => {
            env.get(name).ok_or(JadeError::UndefinedVariable { name: name.clone(), span: *span })
        }

        Expr::Call { callee, args, span } => {
            let callee_val = eval_expr(callee, env)?;

            match callee_val {
                Value::Fn(fn_rc) => {
                    if args.len() != fn_rc.params.len() {
                        return Err(JadeError::ArityMismatch {
                            expected: fn_rc.params.len(),
                            got: args.len(),
                            span: *span,
                        });
                    }
                    let mut arg_vals = Vec::with_capacity(args.len());
                    for arg_expr in args {
                        arg_vals.push(eval_expr(arg_expr, env)?);
                    }
                    env.push_scope();
                    for (param, val) in fn_rc.params.iter().zip(arg_vals) {
                        env.define(param.clone(), val);
                    }
                    let result = eval_block(&fn_rc.body, env);
                    env.pop_scope();
                    Ok(result?.unwrap_or(Value::Int(0)))
                }

                Value::BoundMethod(bm) => {
                    let fn_val = &bm.method;
                    // params[0] is the `self` parameter — it is provided automatically
                    // from the receiver; the caller supplies the remaining params.
                    let self_param_count = if fn_val.params.is_empty() { 0 } else { 1 };
                    let expected = fn_val.params.len() - self_param_count;
                    if args.len() != expected {
                        return Err(JadeError::ArityMismatch {
                            expected,
                            got: args.len(),
                            span: *span,
                        });
                    }
                    let mut arg_vals = Vec::with_capacity(args.len());
                    for arg_expr in args {
                        arg_vals.push(eval_expr(arg_expr, env)?);
                    }
                    env.push_scope();
                    if !fn_val.params.is_empty() {
                        // Bind `self` to the receiver (shared Rc — mutations inside
                        // the method body are visible on the original instance).
                        env.define(
                            fn_val.params[0].clone(),
                            Value::Struct(bm.receiver.clone()),
                        );
                        for (param, val) in fn_val.params[1..].iter().zip(arg_vals) {
                            env.define(param.clone(), val);
                        }
                    }
                    let result = eval_block(&fn_val.body, env);
                    env.pop_scope();
                    Ok(result?.unwrap_or(Value::Int(0)))
                }

                Value::Builtin(BuiltinFn::Print) => {
                    if args.len() != 1 {
                        return Err(JadeError::ArityMismatch {
                            expected: 1,
                            got: args.len(),
                            span: *span,
                        });
                    }
                    let v = eval_expr(&args[0], env)?;
                    println!("{}", value_to_str(&v));
                    Ok(Value::Int(0))
                }

                _ => Err(JadeError::NotCallable { span: *span }),
            }
        }

        Expr::StructLiteral { type_name, fields, span } => {
            let def_fields = env.struct_defs.get(type_name)
                .ok_or_else(|| JadeError::UndefinedType { name: type_name.clone(), span: *span })?
                .clone();

            // Evaluate all field expressions
            let mut field_map: HashMap<String, Value> = HashMap::new();
            for (fname, fexpr) in fields {
                let v = eval_expr(fexpr, env)?;
                field_map.insert(fname.clone(), v);
            }

            // Verify every declared field is present in the literal
            for required in &def_fields {
                if !field_map.contains_key(required) {
                    return Err(JadeError::MissingField { field: required.clone(), span: *span });
                }
            }

            // Verify no extra fields beyond what the struct defines
            for provided in field_map.keys() {
                if !def_fields.contains(provided) {
                    return Err(JadeError::UndefinedField {
                        type_name: type_name.clone(),
                        field: provided.clone(),
                        span: *span,
                    });
                }
            }

            Ok(Value::Struct(Rc::new(RefCell::new(StructInstance {
                type_name: type_name.clone(),
                fields: field_map,
            }))))
        }

        Expr::FieldAccess { object, field, span } => {
            let obj_val = eval_expr(object, env)?;
            match obj_val {
                Value::Struct(rc) => {
                    let type_name = rc.borrow().type_name.clone();
                    // Check instance fields first
                    if let Some(v) = rc.borrow().fields.get(field).cloned() {
                        return Ok(v);
                    }
                    // Fall back to extend methods, returning a bound method
                    if let Some(methods) = env.extend_methods.get(&type_name) {
                        if let Some(method) = methods.get(field) {
                            return Ok(Value::BoundMethod(Rc::new(BoundMethod {
                                receiver: rc,
                                method: method.clone(),
                            })));
                        }
                    }
                    Err(JadeError::UndefinedField { type_name, field: field.clone(), span: *span })
                }
                _ => Err(JadeError::NotAStruct { span: *span }),
            }
        }

        Expr::Index { object, index, span } => {
            let obj = eval_expr(object, env)?;
            let idx = eval_expr(index, env)?;
            match (obj, idx) {
                (Value::Str(s), Value::Int(i)) => {
                    // Collect once: avoids the double O(n) traversal of chars().count()
                    // followed by chars().nth(). Vec<char> indexing is then O(1).
                    let chars: Vec<char> = s.chars().collect();
                    let len = chars.len();
                    if i < 0 || i as usize >= len {
                        Err(JadeError::IndexOutOfBounds { index: i, len, span: *span })
                    } else {
                        Ok(Value::Str(chars[i as usize].to_string()))
                    }
                }
                (Value::Str(_), _) => Err(JadeError::TypeError { op: "[]".to_string(), span: *span }),
                _                  => Err(JadeError::TypeError { op: "[]".to_string(), span: *span }),
            }
        }

        Expr::BinOp { op, left, right, span } => {
            match op {
                // Short-circuit logical ops
                BinOpKind::And => {
                    let l = eval_expr(left, env)?;
                    match l {
                        Value::Bool(false) => Ok(Value::Bool(false)),
                        Value::Bool(true)  => match eval_expr(right, env)? {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _              => Err(JadeError::TypeError { op: "&&".to_string(), span: *span }),
                        },
                        _ => Err(JadeError::TypeError { op: "&&".to_string(), span: *span }),
                    }
                }
                BinOpKind::Or => {
                    let l = eval_expr(left, env)?;
                    match l {
                        Value::Bool(true)  => Ok(Value::Bool(true)),
                        Value::Bool(false) => match eval_expr(right, env)? {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _              => Err(JadeError::TypeError { op: "||".to_string(), span: *span }),
                        },
                        _ => Err(JadeError::TypeError { op: "||".to_string(), span: *span }),
                    }
                }

                // All other binary ops evaluate both operands eagerly
                other => {
                    let l = eval_expr(left, env)?;
                    let r = eval_expr(right, env)?;
                    match other {
                        BinOpKind::Add => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_add(b)
                                .ok_or(JadeError::IntegerOverflow { span: *span })
                                .map(Value::Int),
                            (Value::Str(a), Value::Str(b)) => Ok(Value::Str(a + &b)),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Str(_), _) | (_, Value::Str(_)) =>
                                Err(JadeError::TypeError { op: "+".to_string(), span: *span }),
                            (a, b) => Ok(Value::Float(to_float(a) + to_float(b))),
                        },
                        BinOpKind::Sub => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_sub(b)
                                .ok_or(JadeError::IntegerOverflow { span: *span })
                                .map(Value::Int),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Str(_), _)  | (_, Value::Str(_))  |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) =>
                                Err(JadeError::TypeError { op: "-".to_string(), span: *span }),
                            (a, b) => Ok(Value::Float(to_float(a) - to_float(b))),
                        },
                        BinOpKind::Mul => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_mul(b)
                                .ok_or(JadeError::IntegerOverflow { span: *span })
                                .map(Value::Int),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Str(_), _)  | (_, Value::Str(_))  |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) =>
                                Err(JadeError::TypeError { op: "*".to_string(), span: *span }),
                            (a, b) => Ok(Value::Float(to_float(a) * to_float(b))),
                        },
                        BinOpKind::Div => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::DivisionByZero { span: *span }) } else { Ok(Value::Int(a / b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Str(_), _)  | (_, Value::Str(_))  |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) =>
                                Err(JadeError::TypeError { op: "/".to_string(), span: *span }),
                            (a, b) => {
                                let bf = to_float(b);
                                if bf == 0.0 { Err(JadeError::DivisionByZero { span: *span }) } else { Ok(Value::Float(to_float(a) / bf)) }
                            }
                        },
                        BinOpKind::Mod => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::RemainderByZero { span: *span }) } else { Ok(Value::Int(a % b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Str(_), _)  | (_, Value::Str(_))  |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) =>
                                Err(JadeError::TypeError { op: "%".to_string(), span: *span }),
                            (a, b) => {
                                let bf = to_float(b);
                                if bf == 0.0 { Err(JadeError::RemainderByZero { span: *span }) } else { Ok(Value::Float(to_float(a) % bf)) }
                            }
                        },

                        BinOpKind::BitAnd => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                            _ => Err(JadeError::TypeError { op: "&".to_string(), span: *span }),
                        },
                        BinOpKind::BitOr => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                            _ => Err(JadeError::TypeError { op: "|".to_string(), span: *span }),
                        },
                        BinOpKind::BitXor => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                            _ => Err(JadeError::TypeError { op: "^".to_string(), span: *span }),
                        },
                        BinOpKind::Shl => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b < 0 || b >= 64 {
                                    Err(JadeError::InvalidShift { amount: b, span: *span })
                                } else {
                                    // SAFETY: guard above ensures 0 <= b < 64, fits safely in u32
                                    Ok(Value::Int(a << b as u32))
                                }
                            }
                            _ => Err(JadeError::TypeError { op: "<<".to_string(), span: *span }),
                        },
                        BinOpKind::Shr => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b < 0 || b >= 64 {
                                    Err(JadeError::InvalidShift { amount: b, span: *span })
                                } else {
                                    // SAFETY: guard above ensures 0 <= b < 64, fits safely in u32
                                    Ok(Value::Int(a >> b as u32))
                                }
                            }
                            _ => Err(JadeError::TypeError { op: ">>".to_string(), span: *span }),
                        },

                        BinOpKind::Eq => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a == b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a == b)),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b)),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a == b)),
                            _ => Err(JadeError::TypeError { op: "==".to_string(), span: *span }),
                        },
                        BinOpKind::Ne => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a != b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a != b)),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a != b)),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a != b)),
                            _ => Err(JadeError::TypeError { op: "!=".to_string(), span: *span }),
                        },

                        BinOpKind::Lt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) < b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a < (b as f64))),
                            // bool ordering: false=0, true=1 → false < true
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(!a && b)),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a < b)),
                            _ => Err(JadeError::TypeError { op: "<".to_string(), span: *span }),
                        },
                        BinOpKind::Gt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) > b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a > (b as f64))),
                            // bool ordering: false=0, true=1 → true > false
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a && !b)),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a > b)),
                            _ => Err(JadeError::TypeError { op: ">".to_string(), span: *span }),
                        },
                        BinOpKind::Le => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a <= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) <= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a <= (b as f64))),
                            // bool ordering: false=0, true=1 → a <= b iff a==b or a<b
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b || (!a && b))),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a <= b)),
                            _ => Err(JadeError::TypeError { op: "<=".to_string(), span: *span }),
                        },
                        BinOpKind::Ge => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a >= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) >= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a >= (b as f64))),
                            // bool ordering: false=0, true=1 → a >= b iff a==b or a>b
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b || (a && !b))),
                            (Value::Str(a),   Value::Str(b))   => Ok(Value::Bool(a >= b)),
                            _ => Err(JadeError::TypeError { op: ">=".to_string(), span: *span }),
                        },

                        BinOpKind::And | BinOpKind::Or => unreachable!(),
                    }
                }
            }
        }

        Expr::UnaryOp { op, operand, span } => {
            let val = eval_expr(operand, env)?;
            match op {
                UnaryOpKind::BitNot => match val {
                    Value::Int(i)  => Ok(Value::Int(!i)),
                    _ => Err(JadeError::TypeError { op: "~".to_string(), span: *span }),
                },
                UnaryOpKind::Not => match val {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    _ => Err(JadeError::TypeError { op: "!".to_string(), span: *span }),
                },
                UnaryOpKind::Neg => match val {
                    Value::Int(i)   => Ok(Value::Int(-i)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    _ => Err(JadeError::TypeError { op: "-".to_string(), span: *span }),
                },
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{error::JadeError, lexer, parser};

    fn eval_src(src: &str) -> Result<Env> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        evaluate(program)
    }

    fn eval_src_parse_err(src: &str) -> JadeError {
        let tokens = lexer::tokenize(src).expect("lex failed");
        parser::parse(tokens).unwrap_err()
    }

    fn get(env: &Env, name: &str) -> Value {
        env.get(name).unwrap_or_else(|| panic!("variable '{}' not found in env", name))
    }

    // ── arithmetic ───────────────────────────────────────────────────────────

    #[test]
    fn test_eval_add_int() {
        let env = eval_src("let x = 3 + 4").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(7)));
    }

    #[test]
    fn test_eval_sub_int() {
        let env = eval_src("let x = 10 - 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(7)));
    }

    #[test]
    fn test_eval_mul_int() {
        let env = eval_src("let x = 3 * 4").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(12)));
    }

    #[test]
    fn test_eval_div_int() {
        let env = eval_src("let x = 10 / 2").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(5)));
    }

    #[test]
    fn test_eval_mod_int() {
        let env = eval_src("let x = 10 % 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(1)));
    }

    #[test]
    fn test_eval_add_float() {
        let env = eval_src("let x = 1.5 + 2.5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == 4.0));
    }

    #[test]
    fn test_eval_add_promotes_to_float() {
        let env = eval_src("let x = 1 + 0.5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == 1.5));
    }

    #[test]
    fn test_eval_mul_promotes_to_float() {
        let env = eval_src("let x = 2 * 1.5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == 3.0));
    }

    #[test]
    fn test_eval_div_float() {
        let env = eval_src("let x = 5.0 / 2.0").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == 2.5));
    }

    #[test]
    fn test_eval_mod_float() {
        let env = eval_src("let x = 5.0 % 2.0").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == 1.0));
    }

    // ── bitwise ──────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_bitand() {
        let env = eval_src("let x = 6 & 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(2)));
    }

    #[test]
    fn test_eval_bitor() {
        let env = eval_src("let x = 6 | 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(7)));
    }

    #[test]
    fn test_eval_bitxor() {
        let env = eval_src("let x = 6 ^ 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(5)));
    }

    #[test]
    fn test_eval_shl() {
        let env = eval_src("let x = 1 << 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(8)));
    }

    #[test]
    fn test_eval_shr() {
        let env = eval_src("let x = 16 >> 2").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(4)));
    }

    // ── unary ────────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_bitnot_zero() {
        let env = eval_src("let x = ~0").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(-1)));
    }

    #[test]
    fn test_eval_neg_int() {
        let env = eval_src("let x = -5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(-5)));
    }

    #[test]
    fn test_eval_neg_float() {
        let env = eval_src("let x = -2.5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Float(f) if f == -2.5));
    }

    #[test]
    fn test_eval_neg_expr() {
        let env = eval_src("let x = -(3 + 4)").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(-7)));
    }

    // ── error conditions ─────────────────────────────────────────────────────

    #[test]
    fn test_eval_div_by_zero_int() {
        let err = eval_src("let x = 5 / 0").unwrap_err();
        assert!(matches!(err, JadeError::DivisionByZero { .. }));
    }

    #[test]
    fn test_eval_div_by_zero_float() {
        let err = eval_src("let x = 5.0 / 0.0").unwrap_err();
        assert!(matches!(err, JadeError::DivisionByZero { .. }));
    }

    #[test]
    fn test_eval_remainder_by_zero_int() {
        let err = eval_src("let x = 5 % 0").unwrap_err();
        assert!(matches!(err, JadeError::RemainderByZero { .. }));
    }

    #[test]
    fn test_eval_remainder_by_zero_float() {
        let err = eval_src("let x = 5.0 % 0.0").unwrap_err();
        assert!(matches!(err, JadeError::RemainderByZero { .. }));
    }

    #[test]
    fn test_eval_invalid_shift_too_large() {
        let err = eval_src("let x = 1 << 64").unwrap_err();
        assert!(matches!(err, JadeError::InvalidShift { amount: 64, .. }));
    }

    #[test]
    fn test_eval_invalid_shift_negative() {
        let err = eval_src("let x = 1 >> -1").unwrap_err();
        assert!(matches!(err, JadeError::InvalidShift { amount: -1, .. }));
    }

    #[test]
    fn test_eval_type_error_bitand_float() {
        let err = eval_src("let x = 1.0 & 2.0").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_type_error_bitnot_float() {
        let err = eval_src("let x = ~1.0").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_type_error_neg_bool() {
        let err = eval_src("let x = -true").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_type_error_add_bool() {
        let err = eval_src("let x = true + 1").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_undefined_variable() {
        let err = eval_src("let x = y").unwrap_err();
        assert!(matches!(err, JadeError::UndefinedVariable { .. }));
    }

    #[test]
    fn test_eval_variable_chain() {
        let env = eval_src("let add = 1 + 1\nlet result = add * 2").unwrap();
        assert!(matches!(get(&env, "add"),    Value::Int(2)));
        assert!(matches!(get(&env, "result"), Value::Int(4)));
    }

    // ── boolean literals and logical ops ─────────────────────────────────────

    #[test]
    fn test_eval_bool_literal_true() {
        let env = eval_src("let x = true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_bool_literal_false() {
        let env = eval_src("let x = false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_logical_and_tt() {
        let env = eval_src("let x = true && true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_logical_and_tf() {
        let env = eval_src("let x = true && false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_logical_or_ff() {
        let env = eval_src("let x = false || false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_logical_or_tf() {
        let env = eval_src("let x = true || false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_not_true() {
        let env = eval_src("let x = !true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_not_false() {
        let env = eval_src("let x = !false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_double_not() {
        let env = eval_src("let x = !!true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_short_circuit_and_skips_rhs() {
        let env = eval_src("let x = false && undefined_var").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_short_circuit_or_skips_rhs() {
        let env = eval_src("let x = true || undefined_var").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_type_error_and_on_int() {
        let err = eval_src("let x = 1 && 0").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_type_error_not_on_int() {
        let err = eval_src("let x = !1").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    // ── comparison ───────────────────────────────────────────────────────────

    #[test]
    fn test_eval_eq_int_true() {
        let env = eval_src("let x = 3 == 3").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_eq_int_false() {
        let env = eval_src("let x = 3 == 4").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_ne() {
        let env = eval_src("let x = 3 != 4").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_lt_int() {
        let env = eval_src("let x = 1 < 2").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_gt_int() {
        let env = eval_src("let x = 2 > 1").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_le_equal() {
        let env = eval_src("let x = 2 <= 2").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_ge_equal() {
        let env = eval_src("let x = 2 >= 2").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_bool_lt_false_true() {
        let env = eval_src("let x = false < true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_bool_gt_true_false() {
        let env = eval_src("let x = true > false").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_bool_eq() {
        let env = eval_src("let x = true == true").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_eq_mixed_type_error() {
        let err = eval_src("let x = 1 == 1.0").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_type_error_lt_bool_int() {
        let err = eval_src("let x = true < 1").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_compare_chain() {
        let env = eval_src("let x = 1 < 2 && 3 > 0").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_float_lt_promotes() {
        let env = eval_src("let x = 1 < 2.5").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(true)));
    }

    // ── functions — basic ─────────────────────────────────────────────────────

    #[test]
    fn test_eval_fn_add() {
        let env = eval_src("fn add(a, b) {\n    return a + b\n}\nlet sum = add(3, 4)").unwrap();
        assert!(matches!(get(&env, "sum"), Value::Int(7)));
    }

    #[test]
    fn test_eval_fn_square() {
        let env = eval_src("fn square(x) {\n    return x * x\n}\nlet sq = square(5)").unwrap();
        assert!(matches!(get(&env, "sq"), Value::Int(25)));
    }

    #[test]
    fn test_eval_fn_multiply_three() {
        let env = eval_src("fn multiply(a, b, c) {\n    return a * b * c\n}\nlet r = multiply(2, 3, 4)").unwrap();
        assert!(matches!(get(&env, "r"), Value::Int(24)));
    }

    #[test]
    fn test_eval_fn_chained_calls() {
        let src = "fn add(a, b) {\n    return a + b\n}\nfn square(x) {\n    return x * x\n}\nlet r = add(square(2), square(3))";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "r"), Value::Int(13)));
    }

    // ── functions — scope ─────────────────────────────────────────────────────

    #[test]
    fn test_eval_fn_local_let() {
        let env = eval_src("fn get_local() {\n    let x = 42\n    return x\n}\nlet a = get_local()").unwrap();
        assert!(matches!(get(&env, "a"), Value::Int(42)));
    }

    #[test]
    fn test_eval_fn_uses_param() {
        let env = eval_src("fn uses_param(x) {\n    return x + 1\n}\nlet b = uses_param(9)").unwrap();
        assert!(matches!(get(&env, "b"), Value::Int(10)));
    }

    #[test]
    fn test_eval_fn_local_shadow() {
        let env = eval_src("fn local_shadow(x) {\n    let y = x * 2\n    return y\n}\nlet c = local_shadow(5)").unwrap();
        assert!(matches!(get(&env, "c"), Value::Int(10)));
    }

    // ── functions — first-class ───────────────────────────────────────────────

    #[test]
    fn test_eval_fn_assign_to_let() {
        let src = "fn double(x) {\n    return x * 2\n}\nlet f = double\nlet a = f(5)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "a"), Value::Int(10)));
    }

    #[test]
    fn test_eval_fn_pass_as_arg() {
        let src = "fn double(x) {\n    return x * 2\n}\nfn apply(f, x) {\n    return f(x)\n}\nlet b = apply(double, 6)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "b"), Value::Int(12)));
    }

    #[test]
    fn test_eval_fn_compose() {
        let src = "fn double(x) {\n    return x * 2\n}\nfn compose(f, g, x) {\n    return f(g(x))\n}\nlet d = compose(double, double, 3)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "d"), Value::Int(12)));
    }

    // ── functions — iteration ─────────────────────────────────────────────────

    #[test]
    fn test_eval_fn_factorial_0() {
        let src = "fn factorial(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f0 = factorial(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f0"), Value::Int(1)));
    }

    #[test]
    fn test_eval_fn_factorial_1() {
        let src = "fn factorial(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f1 = factorial(1)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f1"), Value::Int(1)));
    }

    #[test]
    fn test_eval_fn_factorial_5() {
        let src = "fn factorial(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f5 = factorial(5)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f5"), Value::Int(120)));
    }

    #[test]
    fn test_eval_fn_factorial_7() {
        let src = "fn factorial(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f7 = factorial(7)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f7"), Value::Int(5040)));
    }

    #[test]
    fn test_eval_fn_fib_0() {
        let src = "fn fib(n) {\n    if n <= 1 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nlet fib0 = fib(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "fib0"), Value::Int(0)));
    }

    #[test]
    fn test_eval_fn_fib_1() {
        let src = "fn fib(n) {\n    if n <= 1 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nlet fib1 = fib(1)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "fib1"), Value::Int(1)));
    }

    #[test]
    fn test_eval_fn_fib_10() {
        let src = "fn fib(n) {\n    if n <= 1 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nlet fib10 = fib(10)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "fib10"), Value::Int(55)));
    }

    #[test]
    fn test_eval_fn_sum_to_0() {
        let src = "fn sum_to(n) {\n    if n <= 0 {\n        return 0\n    }\n    return n + sum_to(n - 1)\n}\nlet s0 = sum_to(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "s0"), Value::Int(0)));
    }

    #[test]
    fn test_eval_fn_sum_to_10() {
        let src = "fn sum_to(n) {\n    if n <= 0 {\n        return 0\n    }\n    return n + sum_to(n - 1)\n}\nlet s10 = sum_to(10)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "s10"), Value::Int(55)));
    }

    // ── functions — if/else ───────────────────────────────────────────────────

    #[test]
    fn test_eval_if_max() {
        let src = "fn max(a, b) {\n    if a > b {\n        return a\n    } else {\n        return b\n    }\n}\nlet m1 = max(3, 7)\nlet m2 = max(10, 2)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "m1"), Value::Int(7)));
        assert!(matches!(get(&env, "m2"), Value::Int(10)));
    }

    #[test]
    fn test_eval_if_is_positive() {
        let src = "fn is_positive(x) {\n    if x > 0 {\n        return true\n    } else {\n        return false\n    }\n}\nlet pos = is_positive(5)\nlet neg = is_positive(-3)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "pos"), Value::Bool(true)));
        assert!(matches!(get(&env, "neg"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_if_clamp() {
        let src = "fn clamp(x, lo, hi) {\n    if x < lo {\n        return lo\n    }\n    if x > hi {\n        return hi\n    }\n    return x\n}\nlet lo = clamp(1, 5, 10)\nlet mid = clamp(7, 5, 10)\nlet hi = clamp(15, 5, 10)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "lo"),  Value::Int(5)));
        assert!(matches!(get(&env, "mid"), Value::Int(7)));
        assert!(matches!(get(&env, "hi"),  Value::Int(10)));
    }

    // ── functions — nested if ─────────────────────────────────────────────────

    #[test]
    fn test_eval_nested_if_sign() {
        let src = "fn sign(x) {\n    if x > 0 {\n        return 1\n    } else {\n        if x < 0 {\n            return -1\n        } else {\n            return 0\n        }\n    }\n}\nlet s1 = sign(10)\nlet s2 = sign(-5)\nlet s3 = sign(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "s1"), Value::Int(1)));
        assert!(matches!(get(&env, "s2"), Value::Int(-1)));
        assert!(matches!(get(&env, "s3"), Value::Int(0)));
    }

    #[test]
    fn test_eval_nested_if_quadrant() {
        let src = "fn quadrant(a, b) {\n    if a > 0 {\n        if b > 0 {\n            return 1\n        } else {\n            return 4\n        }\n    } else {\n        if b > 0 {\n            return 2\n        } else {\n            return 3\n        }\n    }\n}\nlet q1 = quadrant(1, 1)\nlet q2 = quadrant(-1, 1)\nlet q3 = quadrant(-1, -1)\nlet q4 = quadrant(1, -1)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "q1"), Value::Int(1)));
        assert!(matches!(get(&env, "q2"), Value::Int(2)));
        assert!(matches!(get(&env, "q3"), Value::Int(3)));
        assert!(matches!(get(&env, "q4"), Value::Int(4)));
    }

    // ── elif ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_elif_classify() {
        let src = "fn classify(x) {\n    if x > 0 {\n        return 1\n    } elif x < 0 {\n        return -1\n    } else {\n        return 0\n    }\n}\nlet pos = classify(5)\nlet neg = classify(-3)\nlet zero = classify(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "pos"),  Value::Int(1)));
        assert!(matches!(get(&env, "neg"),  Value::Int(-1)));
        assert!(matches!(get(&env, "zero"), Value::Int(0)));
    }

    #[test]
    fn test_eval_elif_chain() {
        let src = "fn grade(s) {\n    if s >= 90 {\n        return 4\n    } elif s >= 80 {\n        return 3\n    } elif s >= 70 {\n        return 2\n    } elif s >= 60 {\n        return 1\n    } else {\n        return 0\n    }\n}\nlet a = grade(95)\nlet b = grade(85)\nlet c = grade(75)\nlet d = grade(65)\nlet f = grade(50)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "a"), Value::Int(4)));
        assert!(matches!(get(&env, "b"), Value::Int(3)));
        assert!(matches!(get(&env, "c"), Value::Int(2)));
        assert!(matches!(get(&env, "d"), Value::Int(1)));
        assert!(matches!(get(&env, "f"), Value::Int(0)));
    }

    #[test]
    fn test_eval_elif_no_else() {
        let src = "fn check(x) {\n    if x == 1 {\n        return 10\n    } elif x == 2 {\n        return 20\n    }\n    return 0\n}\nlet r1 = check(1)\nlet r2 = check(2)\nlet r3 = check(3)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "r1"), Value::Int(10)));
        assert!(matches!(get(&env, "r2"), Value::Int(20)));
        assert!(matches!(get(&env, "r3"), Value::Int(0)));
    }

    // ── functions — nested calls ──────────────────────────────────────────────

    #[test]
    fn test_eval_nested_calls_pipeline() {
        let src = "fn add(a, b) {\n    return a + b\n}\nfn double(x) {\n    return x * 2\n}\nfn square(x) {\n    return x * x\n}\nfn pipeline(a, b) {\n    return double(square(add(a, b)))\n}\nlet pipe = pipeline(1, 2)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "pipe"), Value::Int(18)));
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn test_eval_arity_mismatch() {
        let err = eval_src("fn f(a) {\n    return a\n}\nlet x = f(1, 2)").unwrap_err();
        assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
    }

    #[test]
    fn test_eval_not_callable() {
        let err = eval_src("let x = 5\nlet y = x(1)").unwrap_err();
        assert!(matches!(err, JadeError::NotCallable { .. }));
    }

    // ── integer overflow ──────────────────────────────────────────────────────

    #[test]
    fn test_eval_integer_overflow_add() {
        let err = eval_src(&format!("let x = {} + 1", i64::MAX)).unwrap_err();
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_eval_integer_overflow_sub() {
        // -(i64::MAX) - 2 == i64::MIN - 1; written as negation to avoid
        // the lexer's LiteralOverflow on the i64::MIN literal itself.
        let err = eval_src(&format!("let x = -{} - 2", i64::MAX)).unwrap_err();
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_eval_integer_overflow_mul() {
        let err = eval_src(&format!("let x = {} * 2", i64::MAX)).unwrap_err();
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_eval_nested_fn_parse_error() {
        let err = eval_src_parse_err("fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}");
        assert!(matches!(err, JadeError::NestedFunction { .. }));
    }

    // ── while loops ──────────────────────────────────────────────────────────

    #[test]
    fn test_eval_while_basic_count_up() {
        let env = eval_src("let i = 0\nwhile i < 5 {\n    i = i + 1\n}").unwrap();
        assert!(matches!(get(&env, "i"), Value::Int(5)));
    }

    #[test]
    fn test_eval_while_condition_false_from_start() {
        let env = eval_src("let never = 99\nwhile never < 0 {\n    never = never + 1\n}").unwrap();
        assert!(matches!(get(&env, "never"), Value::Int(99)));
    }

    #[test]
    fn test_eval_while_accumulate_sum() {
        let env = eval_src(
            "let sum = 0\nlet i = 1\nwhile i <= 10 {\n    sum = sum + i\n    i = i + 1\n}"
        ).unwrap();
        assert!(matches!(get(&env, "sum"), Value::Int(55)));
    }

    #[test]
    fn test_eval_while_boolean_flag() {
        let env = eval_src(
            "let flag = true\nlet steps = 0\nwhile flag {\n    steps = steps + 1\n    if steps == 3 {\n        flag = false\n    }\n}"
        ).unwrap();
        assert!(matches!(get(&env, "steps"), Value::Int(3)));
        assert!(matches!(get(&env, "flag"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_while_in_fn_factorial() {
        let env = eval_src(
            "fn factorial(n) {\n    let result = 1\n    let i = 1\n    while i <= n {\n        result = result * i\n        i = i + 1\n    }\n    return result\n}\nlet f5 = factorial(5)\nlet f0 = factorial(0)"
        ).unwrap();
        assert!(matches!(get(&env, "f5"), Value::Int(120)));
        assert!(matches!(get(&env, "f0"), Value::Int(1)));
    }

    #[test]
    fn test_eval_while_return_propagates() {
        // return inside a while body must exit the function immediately
        let env = eval_src(
            "fn first_above(threshold) {\n    let n = 1\n    while n * n <= threshold {\n        n = n + 1\n    }\n    return n\n}\nlet r = first_above(9)"
        ).unwrap();
        assert!(matches!(get(&env, "r"), Value::Int(4)));
    }

    #[test]
    fn test_eval_while_nested() {
        let env = eval_src(
            "let total = 0\nlet i = 0\nwhile i < 3 {\n    let j = 0\n    while j < 3 {\n        total = total + 1\n        j = j + 1\n    }\n    i = i + 1\n}"
        ).unwrap();
        assert!(matches!(get(&env, "total"), Value::Int(9)));
    }

    #[test]
    fn test_eval_while_type_error_condition() {
        let err = eval_src("while 1 {\n}").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    // ── structs ───────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_struct_field_access() {
        let env = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 10, y: 20 }\nlet px = p.x\nlet py = p.y"
        ).unwrap();
        assert!(matches!(get(&env, "px"), Value::Int(10)));
        assert!(matches!(get(&env, "py"), Value::Int(20)));
    }

    #[test]
    fn test_eval_struct_field_mutation() {
        let env = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 10, y: 20 }\np.x = 99\nlet updated = p.x"
        ).unwrap();
        assert!(matches!(get(&env, "updated"), Value::Int(99)));
    }

    #[test]
    fn test_eval_method_call_mutates_state() {
        let src = concat!(
            "struct Counter {\n    count\n}\n",
            "extend Counter {\n",
            "    fn increment(self) {\n        self.count = self.count + 1\n    }\n",
            "    fn value(self) {\n        return self.count\n    }\n",
            "}\n",
            "let c = Counter { count: 0 }\n",
            "c.increment()\nc.increment()\nlet v = c.value()"
        );
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "v"), Value::Int(2)));
    }

    #[test]
    fn test_eval_undefined_type_error() {
        let err = eval_src("let p = Foo { x: 1 }").unwrap_err();
        assert!(matches!(err, JadeError::UndefinedType { .. }));
    }

    #[test]
    fn test_eval_missing_field_error() {
        let err = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 1 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn test_eval_extra_field_error() {
        let err = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 1, y: 2, z: 3 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    #[test]
    fn test_eval_field_access_on_non_struct_error() {
        let err = eval_src("let x = 5\nlet v = x.y").unwrap_err();
        assert!(matches!(err, JadeError::NotAStruct { .. }));
    }

    #[test]
    fn test_eval_undefined_field_access_error() {
        let err = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 1, y: 2 }\nlet v = p.z"
        ).unwrap_err();
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    // ── strings ──────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_str_literal() {
        let env = eval_src(r#"let s = "hello""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "hello"));
    }

    #[test]
    fn test_eval_str_concat() {
        let env = eval_src(r#"let s = "foo" + "bar""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "foobar"));
    }

    #[test]
    fn test_eval_str_eq_true() {
        let env = eval_src(r#"let b = "abc" == "abc""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_eq_false() {
        let env = eval_src(r#"let b = "abc" == "xyz""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_str_ne() {
        let env = eval_src(r#"let b = "abc" != "xyz""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_lt() {
        let env = eval_src(r#"let b = "abc" < "abd""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_gt() {
        let env = eval_src(r#"let b = "b" > "a""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_le_equal() {
        let env = eval_src(r#"let b = "abc" <= "abc""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_ge() {
        let env = eval_src(r#"let b = "z" >= "a""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_str_index() {
        let env = eval_src("let s = \"hello\"\nlet h = s[0]").unwrap();
        assert!(matches!(get(&env, "h"), Value::Str(ref v) if v == "h"));
    }

    #[test]
    fn test_eval_str_index_last() {
        let env = eval_src("let s = \"hello\"\nlet o = s[4]").unwrap();
        assert!(matches!(get(&env, "o"), Value::Str(ref v) if v == "o"));
    }

    #[test]
    fn test_eval_str_index_out_of_bounds() {
        let err = eval_src("let s = \"hi\"\nlet x = s[10]").unwrap_err();
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: 10, len: 2, .. }));
    }

    #[test]
    fn test_eval_str_index_negative() {
        let err = eval_src("let s = \"hi\"\nlet x = s[-1]").unwrap_err();
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn test_eval_str_add_int_type_error() {
        let err = eval_src(r#"let x = "hello" + 1"#).unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_str_sub_type_error() {
        let err = eval_src(r#"let x = "a" - "b""#).unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_str_escape_tab() {
        let env = eval_src(r#"let s = "a\tb""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "a\tb"));
    }

    #[test]
    fn test_eval_str_escape_newline() {
        let env = eval_src(r#"let s = "a\nb""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "a\nb"));
    }

    #[test]
    fn test_eval_str_escape_quote() {
        let env = eval_src(r#"let s = "say \"hi\"""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == r#"say "hi""#));
    }

    #[test]
    fn test_eval_print_builtin() {
        // print is callable and returns Int(0) without error
        let env = eval_src(r#"let r = 0
print("hello")"#).unwrap();
        // Just verify the program ran without error; print goes to stdout
        assert!(matches!(get(&env, "r"), Value::Int(0)));
    }

    #[test]
    fn test_eval_print_arity_error() {
        let err = eval_src(r#"print("a", "b")"#).unwrap_err();
        assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
    }

    // ── triple-quoted strings ────────────────────────────────────────────────

    #[test]
    fn test_eval_triple_quote_simple() {
        let env = eval_src(r#"let s = """hello""""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "hello"));
    }

    #[test]
    fn test_eval_triple_quote_with_inner_quotes() {
        let env = eval_src(r#"let s = """he said "hi" to her""""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == r#"he said "hi" to her"#));
    }

    #[test]
    fn test_eval_triple_quote_concat() {
        let env = eval_src(r#"let s = """foo""" + """bar""""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "foobar"));
    }

    #[test]
    fn test_eval_triple_quote_equals_regular() {
        let env = eval_src(r#"let b = """abc""" == "abc""#).unwrap();
        assert!(matches!(get(&env, "b"), Value::Bool(true)));
    }

    // ── f-strings ────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_fstr_literal_only() {
        let env = eval_src(r#"let s = f"hello""#).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "hello"));
    }

    #[test]
    fn test_eval_fstr_str_var() {
        let env = eval_src("let name = \"Joe\"\nlet g = f\"hi {name}\"").unwrap();
        assert!(matches!(get(&env, "g"), Value::Str(ref v) if v == "hi Joe"));
    }

    #[test]
    fn test_eval_fstr_int_var() {
        let env = eval_src("let n = 42\nlet s = f\"n={n}\"").unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "n=42"));
    }

    #[test]
    fn test_eval_fstr_bool_var() {
        let env = eval_src("let b = true\nlet s = f\"b={b}\"").unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "b=true"));
    }

    #[test]
    fn test_eval_fstr_multiple_slots() {
        let env = eval_src("let x = 1\nlet y = 2\nlet s = f\"({x}, {y})\"").unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "(1, 2)"));
    }

    #[test]
    fn test_eval_fstr_field_access() {
        let env = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 3, y: 4 }\nlet s = f\"({p.x}, {p.y})\""
        ).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "(3, 4)"));
    }

    #[test]
    fn test_eval_fstr_triple_quote() {
        let env = eval_src("let name = \"Joe\"\nlet s = f\"\"\"hi {name}\"\"\"").unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "hi Joe"));
    }

    #[test]
    fn test_eval_fstr_no_slots_equals_plain_str() {
        let env = eval_src(r#"let a = f"hello"\nlet b = "hello""#.replace("\\n", "\n").as_str()).unwrap();
        // both produce Value::Str("hello")
        assert!(matches!(get(&env, "a"), Value::Str(ref v) if v == "hello"));
        assert!(matches!(get(&env, "b"), Value::Str(ref v) if v == "hello"));
    }
}
