use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    ast::{BinOpKind, CatchArm, Expr, FStrPart, Program, StructFieldDef, Stmt, UnaryOpKind},
    error::{JadeError, Result, Span},
};
use crate::llm;

// ── LLM inference constants ───────────────────────────────────────────────────

/// Token budget for a normal (untyped) LLM response.
const DEFAULT_MAX_TOKENS: u32 = 1024;
/// Token budget for a retry correction reply — only a single typed value is needed.
const RETRY_MAX_TOKENS: u32 = 64;
/// Token budget for retries on complex types (Array, Dict, struct) that may produce larger outputs.
const RETRY_MAX_TOKENS_COMPLEX: u32 = 512;

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
    /// An array value. Stored as a plain `Vec` — assignment clones the whole
    /// vector (value semantics), so aliases are always independent copies.
    Array(Vec<Value>),
    /// A built-in function (e.g. `print`, `len`).
    Builtin(BuiltinFn),
    /// A prompt declaration. Holds the prompt text; dereferenced with `?`.
    Prompt(String),
    /// A dictionary value. Stored as a plain `HashMap` — assignment clones the whole
    /// map (value semantics), so aliases are always independent copies.
    Dict(HashMap<String, Value>),
}

/// Identifies a built-in function by name.
#[derive(Clone, Debug)]
pub enum BuiltinFn {
    Print,
    Len,
    Join,
}

/// Heap-allocated function body shared via `Rc`.
pub struct FnValue {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    /// Variables captured at closure-creation time (empty for named functions).
    pub captured: HashMap<String, Value>,
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
            Value::Array(vec) => {
                write!(f, "Array[")?;
                for (i, v) in vec.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{:?}", v)?;
                }
                write!(f, "]")
            }
            Value::BoundMethod(_) => write!(f, "<bound method>"),
            Value::Builtin(b)     => write!(f, "<builtin {:?}>", b),
            Value::Prompt(text)   => write!(f, "Prompt({:?})", text),
            Value::Dict(map) => {
                let mut pairs: Vec<_> = map.iter().collect();
                pairs.sort_by_key(|(k, _)| k.as_str());
                write!(f, "Dict{{")?;
                let mut first = true;
                for (k, v) in pairs {
                    if !first { write!(f, ", ")?; }
                    write!(f, "{:?}: {:?}", k, v)?;
                    first = false;
                }
                write!(f, "}}")
            }
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
pub struct Env {
    scopes: Vec<HashMap<String, Value>>,
    /// Maps struct type names to their field definitions (including defaults).
    pub struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Maps struct type names to their method tables.
    pub extend_methods: HashMap<String, HashMap<String, Rc<FnValue>>>,
    /// Maps interface names to their required method names.
    pub interface_defs: HashMap<String, Vec<String>>,
    /// Maps type names to the list of interface names they implement.
    pub interface_impls: HashMap<String, Vec<String>>,
    /// Built-in functions that are always in scope. Stored separately so they
    /// don't appear in `-v` verbose output alongside user variables.
    builtins: HashMap<String, BuiltinFn>,
    /// The value most recently raised by `raise`; consumed by the nearest `try/catch`.
    /// Stored here (rather than inside `JadeError`) to avoid a circular dependency
    /// between `error.rs` and `eval.rs`.
    pub raised_exception: Option<Value>,
    /// LLM inference backend, if one was configured for this run.
    pub inference_backend: Option<std::sync::Arc<dyn llm::InferenceBackend>>,
    /// Conversation history shared across all `?` dereferences in this program run.
    pub conversation_history: Vec<llm::Message>,
    /// Running total of tokens consumed by all inference calls.
    pub token_count: i64,
    /// Maximum number of retry attempts for typed dereferences (`?p |> Type`).
    pub max_retries: usize,
    /// Default model name passed to the inference backend.
    pub default_model: String,
}

impl Env {
    /// Create a new environment with one (global) scope and all built-ins pre-registered.
    pub fn new() -> Self {
        let mut builtins = HashMap::new();
        builtins.insert("print".to_string(), BuiltinFn::Print);
        builtins.insert("len".to_string(), BuiltinFn::Len);
        builtins.insert("join".to_string(), BuiltinFn::Join);

        // Pre-populate session variables accessible from Jade code.
        let mut global_scope: HashMap<String, Value> = HashMap::new();
        global_scope.insert("__tokens__".to_string(), Value::Int(0));
        global_scope.insert("__model__".to_string(), Value::Str(String::new()));
        global_scope.insert("__max_retries__".to_string(), Value::Int(3));
        global_scope.insert("__retry_log__".to_string(), Value::Array(vec![]));

        Env {
            scopes: vec![global_scope],
            struct_defs: HashMap::new(),
            extend_methods: HashMap::new(),
            interface_defs: HashMap::new(),
            interface_impls: HashMap::new(),
            builtins,
            raised_exception: None,
            inference_backend: None,
            conversation_history: Vec::new(),
            token_count: 0,
            max_retries: 3,
            default_model: String::new(),
        }
    }

    /// Update a session variable in the global scope (e.g. `__tokens__`).
    pub fn set_session_var(&mut self, name: &str, value: Value) {
        self.scopes[0].insert(name.to_string(), value);
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

    /// Mutable reference to the outermost (global) scope.
    ///
    /// Used by the REPL to remove temporary bindings after displaying their value.
    pub fn globals_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.scopes[0]
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

    /// Snapshot all currently visible variables (inner scopes win over outer).
    /// Used to capture the environment when creating a closure.
    fn snapshot(&self) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        for scope in self.scopes.iter() {
            for (k, v) in scope {
                map.insert(k.clone(), v.clone());
            }
        }
        map
    }

    /// Iterate over all top-level (global) bindings — used by `-v` output.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.scopes[0].iter()
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("scopes", &self.scopes)
            .field("struct_defs", &self.struct_defs)
            .field("extend_methods_keys", &self.extend_methods.keys().collect::<Vec<_>>())
            .field("interface_defs", &self.interface_defs)
            .field("interface_impls", &self.interface_impls)
            .field("inference_backend", &self.inference_backend.as_ref().map(|_| "<backend>"))
            .field("token_count", &self.token_count)
            .field("max_retries", &self.max_retries)
            .field("default_model", &self.default_model)
            .finish()
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Options controlling LLM integration for a single program run.
pub struct LlmOpts {
    pub backend: Option<std::sync::Arc<dyn llm::InferenceBackend>>,
    pub default_model: String,
    pub max_retries: usize,
}

impl Default for LlmOpts {
    fn default() -> Self {
        LlmOpts { backend: None, default_model: String::new(), max_retries: 3 }
    }
}

/// Walk the program and return the populated top-level environment.
pub fn evaluate(program: Program, opts: LlmOpts) -> Result<Env> {
    let mut env = Env::new();
    env.inference_backend = opts.backend;
    env.max_retries = opts.max_retries;
    env.default_model = opts.default_model.clone();
    env.set_session_var("__model__", Value::Str(opts.default_model));
    env.set_session_var("__max_retries__", Value::Int(opts.max_retries as i64));
    eval_block(&program.stmts, &mut env)?;
    Ok(env)
}

/// Execute a program against an **existing** environment.
///
/// Used by the REPL so that definitions from previous lines persist into
/// subsequent ones.  Does **not** reset globals or session variables.
pub fn evaluate_incremental(program: Program, env: &mut Env) -> Result<()> {
    eval_block(&program.stmts, env)?;
    Ok(())
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
                    captured: HashMap::new(),
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

            Stmt::InterfaceDef { name, methods, .. } => {
                let method_names = methods.iter().map(|m| m.name.clone()).collect();
                env.interface_defs.insert(name.clone(), method_names);
            }

            Stmt::ExtendBlock { type_name, interface_name, methods, span } => {
                // Collect the names of all methods provided in this extend block.
                let provided: std::collections::HashSet<String> = methods.iter()
                    .filter_map(|m| if let Stmt::FnDef { name, .. } = m { Some(name.clone()) } else { None })
                    .collect();

                // If this extend claims to implement an interface, validate it.
                if let Some(iface) = interface_name {
                    let required = env.interface_defs.get(iface).ok_or_else(|| JadeError::UndefinedInterface {
                        name: iface.clone(),
                        span: *span,
                    })?.clone();
                    for req in &required {
                        if !provided.contains(req) {
                            return Err(JadeError::MissingInterfaceMethod {
                                type_name: type_name.clone(),
                                interface_name: iface.clone(),
                                method: req.clone(),
                                span: *span,
                            });
                        }
                    }
                    env.interface_impls.entry(type_name.clone()).or_default().push(iface.clone());
                }

                let method_map = env.extend_methods.entry(type_name.clone()).or_default();
                for method in methods {
                    if let Stmt::FnDef { name, params, body, .. } = method {
                        method_map.insert(name.clone(), Rc::new(FnValue {
                            params: params.clone(),
                            body: body.clone(),
                            captured: HashMap::new(),
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

            Stmt::IndexAssign { name, index, value, span } => {
                let idx_val = eval_expr(index, env)?;
                let new_val = eval_expr(value, env)?;
                let arr = env.get(name).ok_or_else(|| JadeError::UndefinedVariable {
                    name: name.clone(),
                    span: *span,
                })?;
                match (arr, idx_val) {
                    (Value::Array(mut vec), Value::Int(i)) => {
                        let len = vec.len();
                        if i < 0 || i as usize >= len {
                            return Err(JadeError::IndexOutOfBounds { index: i, len, span: *span });
                        }
                        vec[i as usize] = new_val;
                        env.assign(name, Value::Array(vec), *span)?;
                    }
                    (Value::Array(_), _) => return Err(JadeError::TypeError {
                        op: "array index".to_string(), span: *span,
                    }),
                    (Value::Dict(mut map), Value::Str(key)) => {
                        map.insert(key, new_val);
                        env.assign(name, Value::Dict(map), *span)?;
                    }
                    (Value::Dict(_), _) => return Err(JadeError::TypeError {
                        op: "dict index".to_string(), span: *span,
                    }),
                    _ => return Err(JadeError::TypeError {
                        op: "index assign".to_string(), span: *span,
                    }),
                }
            }

            Stmt::PromptDecl { name, body, span } => {
                let v = eval_expr(body, env)?;
                match v {
                    Value::Str(text) => env.define(name.clone(), Value::Prompt(text)),
                    _ => return Err(JadeError::TypeError {
                        op: "prompt declaration requires a string body".to_string(),
                        span: *span,
                    }),
                }
            }

            Stmt::For { span, .. } => {
                return Err(JadeError::TypeError {
                    op: "`for` is not supported in the tree-walk evaluator".to_string(),
                    span: *span,
                });
            }

            Stmt::Use { span, .. } => {
                // `use` is not supported by the tree-walk evaluator — it is
                // resolved at the bytecode VM level. Reaching this arm means
                // the evaluator was called directly on a program that contains
                // `use`, which is not the normal execution path.
                return Err(JadeError::TypeError {
                    op: "`use` is not supported in the tree-walk evaluator".to_string(),
                    span: *span,
                });
            }

            Stmt::Raise { value, span } => {
                let v = eval_expr(value, env)?;
                let message = value_to_str(&v);
                env.raised_exception = Some(v);
                return Err(JadeError::Exception { message, span: *span });
            }

            Stmt::TryCatch { body, arms, span } => {
                env.push_scope();
                let result = eval_block(body, env);
                env.pop_scope();

                let raised_val: Value = match result {
                    // Block completed normally or hit a `return` — propagate unchanged.
                    Ok(ret) => return Ok(ret),
                    // A Jade `raise` — take the payload out of env.
                    Err(JadeError::Exception { .. }) => {
                        env.raised_exception.take()
                            .unwrap_or_else(|| Value::Str("unknown exception".to_string()))
                    }
                    // Built-in runtime error (division by zero, type error, …) —
                    // wrap as a `RuntimeError { message }` struct so catch arms can
                    // match on it and access `.message`.
                    Err(other) => {
                        let mut fields = HashMap::new();
                        fields.insert("message".to_string(), Value::Str(other.to_string()));
                        Value::Struct(Rc::new(RefCell::new(StructInstance {
                            type_name: "RuntimeError".to_string(),
                            fields,
                        })))
                    }
                };

                // Try each catch arm in order; first match wins.
                for arm in arms {
                    let matches = match &arm.catch_type {
                        None => true, // catch-all always matches
                        Some(type_name) => match &raised_val {
                            Value::Struct(rc) => rc.borrow().type_name == *type_name,
                            _ => false,
                        },
                    };

                    if matches {
                        env.push_scope();
                        env.define(arm.binding.clone(), raised_val);
                        let arm_result = eval_block(&arm.body, env);
                        env.pop_scope();
                        return arm_result;
                    }
                }

                // No arm matched — re-raise so an outer try/catch can handle it.
                let message = value_to_str(&raised_val);
                env.raised_exception = Some(raised_val);
                return Err(JadeError::Exception { message, span: *span });
            }

            // In the tree-walk evaluator, async fn behaves like a regular fn.
            // True async execution only happens through the bytecode VM / LLVM path.
            Stmt::AsyncFnDef { name, params, body, span } => {
                eprintln!("[{}:{}] warning: '{}' is defined as async fn but the REPL runs it synchronously — use `jade run` for true async execution", span.line, span.col, name);
                let fn_val = FnValue {
                    params: params.clone(),
                    body: body.clone(),
                    captured: HashMap::new(),
                };
                env.define(name.clone(), Value::Fn(Rc::new(fn_val)));
            }

            Stmt::Expr(expr) => {
                eval_expr(expr, env)?;
            }
        }
    }
    Ok(None)
}

// ── Expression evaluator ─────────────────────────────────────────────────────

/// Extract the source `Span` from any expression node.
fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Integer      { span, .. } => *span,
        Expr::Float        { span, .. } => *span,
        Expr::Bool         { span, .. } => *span,
        Expr::Str          { span, .. } => *span,
        Expr::Identifier   { span, .. } => *span,
        Expr::Call         { span, .. } => *span,
        Expr::BinOp        { span, .. } => *span,
        Expr::UnaryOp      { span, .. } => *span,
        Expr::StructLiteral{ span, .. } => *span,
        Expr::FieldAccess  { span, .. } => *span,
        Expr::Index        { span, .. } => *span,
        Expr::Array        { span, .. } => *span,
        Expr::FStr         { span, .. } => *span,
        Expr::PromptLiteral{ span, .. } => *span,
        Expr::PromptDeref  { span, .. } => *span,
        Expr::Dict         { span, .. } => *span,
        Expr::Closure      { span, .. } => *span,
        Expr::Await        { span, .. } => *span,
    }
}

/// Build a human-readable description of an expression for use in error messages.
fn expr_display(e: &Expr) -> String {
    match e {
        Expr::Identifier { name, .. } => name.clone(),
        Expr::FieldAccess { object, field, .. } => format!("{}.{}", expr_display(object), field),
        _ => "<expression>".to_string(),
    }
}

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
        Value::Array(_)       => unreachable!("to_float called on Array"),
        Value::BoundMethod(_) => unreachable!("to_float called on BoundMethod"),
        Value::Builtin(_)     => unreachable!("to_float called on Builtin"),
        Value::Prompt(_)      => unreachable!("to_float called on Prompt"),
        Value::Dict(_)        => unreachable!("to_float called on Dict"),
    }
}

/// Convert a `Value` to its string representation (used by f-string interpolation,
/// the `print` built-in, and `-v` verbose output).  Single source of truth for
/// how values display to the user — update here and everywhere benefits.
pub(crate) fn value_to_str(v: &Value) -> String {
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
        Value::Array(vec) => {
            let parts: Vec<String> = vec.iter().map(value_to_str).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Dict(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            let parts: Vec<String> = pairs.iter()
                .map(|(k, v)| format!("{:?}: {}", k, value_to_str(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Fn(_)          => "<fn>".to_string(),
        Value::Struct(rc) => {
            let inst = rc.borrow();
            let mut parts: Vec<String> = inst.fields.iter()
                .map(|(k, v)| format!("{}: {}", k, value_to_str(v)))
                .collect();
            parts.sort(); // deterministic output regardless of HashMap iteration order
            if parts.is_empty() {
                inst.type_name.clone()
            } else {
                format!("{} {{ {} }}", inst.type_name, parts.join(", "))
            }
        }
        Value::BoundMethod(_) => "<bound method>".to_string(),
        Value::Builtin(_)     => "<builtin>".to_string(),
        Value::Prompt(_)      => "<prompt>".to_string(),
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_)         => "int",
        Value::Float(_)       => "float",
        Value::Bool(_)        => "bool",
        Value::Str(_)         => "str",
        Value::Array(_)       => "array",
        Value::Dict(_)        => "dict",
        Value::Struct(_)      => "struct",
        Value::Fn(_)          => "fn",
        Value::BoundMethod(_) => "fn",
        Value::Builtin(_)     => "fn",
        Value::Prompt(_)      => "prompt",
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
                    // Inject captured variables first so params can shadow them.
                    for (name, val) in &fn_rc.captured {
                        env.define(name.clone(), val.clone());
                    }
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

                Value::Builtin(BuiltinFn::Len) => {
                    if args.len() != 1 {
                        return Err(JadeError::ArityMismatch {
                            expected: 1,
                            got: args.len(),
                            span: *span,
                        });
                    }
                    let v = eval_expr(&args[0], env)?;
                    match v {
                        Value::Str(s)     => Ok(Value::Int(s.chars().count() as i64)),
                        Value::Array(vec) => Ok(Value::Int(vec.len() as i64)),
                        Value::Dict(map)  => Ok(Value::Int(map.len() as i64)),
                        _ => Err(JadeError::TypeError { op: "len".to_string(), span: *span }),
                    }
                }

                // join(f1, f2, ...) — in the tree-walk evaluator async fns run
                // synchronously, so each argument is already a resolved value;
                // join just collects them into an array.
                Value::Builtin(BuiltinFn::Join) => {
                    let mut results = Vec::with_capacity(args.len());
                    for a in args {
                        results.push(eval_expr(a, env)?);
                    }
                    Ok(Value::Array(results))
                }

                _ => Err(JadeError::NotCallable { span: *span }),
            }
        }

        Expr::StructLiteral { type_name, fields, span } => {
            // Clone is required: `eval_expr(default, env)` below needs `&mut env`, which
            // would conflict with an immutable borrow of `env.struct_defs` kept alive
            // across the call. The clone drops that borrow before mutation begins.
            let def_fields: Vec<StructFieldDef> = env.struct_defs
                .get(type_name)
                .ok_or_else(|| JadeError::UndefinedType { name: type_name.clone(), span: *span })?
                .clone();

            // Evaluate all caller-provided field expressions. Store each value alongside
            // the expression's span for precise error reporting, and reject duplicates.
            let mut provided: HashMap<String, (Value, Span)> = HashMap::new();
            for (fname, fexpr) in fields {
                let field_span = expr_span(fexpr);
                if provided.contains_key(fname.as_str()) {
                    return Err(JadeError::DuplicateField { field: fname.clone(), span: field_span });
                }
                let v = eval_expr(fexpr, env)?;
                provided.insert(fname.clone(), (v, field_span));
            }

            // Verify no extra fields beyond what the struct defines
            for (key, (_, key_span)) in &provided {
                if !def_fields.iter().any(|f| f.name() == key) {
                    return Err(JadeError::UndefinedField {
                        type_name: type_name.clone(),
                        field: key.clone(),
                        span: *key_span,
                    });
                }
            }

            // Walk the definition in order, filling final fields from provided values or defaults
            let mut final_fields: HashMap<String, Value> = HashMap::new();
            for def_field in &def_fields {
                match def_field {
                    StructFieldDef::Required(name) => {
                        match provided.remove(name) {
                            Some((v, _)) => { final_fields.insert(name.clone(), v); }
                            None         => return Err(JadeError::MissingField { field: name.clone(), span: *span }),
                        }
                    }
                    StructFieldDef::Let { name, default } => {
                        let v = if let Some((pv, _)) = provided.remove(name) {
                            pv
                        } else {
                            eval_expr(default, env)?
                        };
                        final_fields.insert(name.clone(), v);
                    }
                    StructFieldDef::Prompt { name, default } => {
                        let v = if let Some((pv, field_span)) = provided.remove(name) {
                            // Caller-provided value must be a string; wrap as Prompt
                            match pv {
                                Value::Str(text) => Value::Prompt(text),
                                _ => return Err(JadeError::PromptFieldNotStr {
                                    field: name.clone(),
                                    span: field_span,
                                }),
                            }
                        } else {
                            // Evaluate default; must yield a string
                            match eval_expr(default, env)? {
                                Value::Str(text) => Value::Prompt(text),
                                _ => return Err(JadeError::PromptFieldNotStr {
                                    field: name.clone(),
                                    span: *span,
                                }),
                            }
                        };
                        final_fields.insert(name.clone(), v);
                    }
                }
            }

            Ok(Value::Struct(Rc::new(RefCell::new(StructInstance {
                type_name: type_name.clone(),
                fields: final_fields,
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

        Expr::Array { elements, .. } => {
            let mut vec = Vec::with_capacity(elements.len());
            for elem in elements {
                vec.push(eval_expr(elem, env)?);
            }
            Ok(Value::Array(vec))
        }

        Expr::Dict { entries, .. } => {
            let mut map = HashMap::with_capacity(entries.len());
            for (key_expr, val_expr) in entries {
                let key_span = expr_span(key_expr);
                let key = match eval_expr(key_expr, env)? {
                    Value::Str(s) => s,
                    _ => return Err(JadeError::TypeError {
                        op: "dict key".to_string(),
                        span: key_span,
                    }),
                };
                let val = eval_expr(val_expr, env)?;
                // Duplicate keys are allowed; the last definition wins (Python semantics).
                map.insert(key, val);
            }
            Ok(Value::Dict(map))
        }

        Expr::Closure { params, body, .. } => {
            let fn_val = FnValue {
                params: params.clone(),
                body: body.clone(),
                captured: env.snapshot(),
            };
            Ok(Value::Fn(Rc::new(fn_val)))
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
                (Value::Array(vec), Value::Int(i)) => {
                    let len = vec.len();
                    if i < 0 || i as usize >= len {
                        Err(JadeError::IndexOutOfBounds { index: i, len, span: *span })
                    } else {
                        Ok(vec[i as usize].clone())
                    }
                }
                (Value::Dict(map), Value::Str(key)) => {
                    map.get(&key).cloned().ok_or_else(|| JadeError::KeyNotFound { key, span: *span })
                }
                (Value::Dict(_), _) => Err(JadeError::TypeError { op: "dict index".to_string(), span: *span }),
                _ => Err(JadeError::TypeError { op: "[]".to_string(), span: *span }),
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
                            (Value::Array(_), _) | (_, Value::Array(_)) |
                            (Value::Dict(_), _) | (_, Value::Dict(_)) |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Prompt(_), _) | (_, Value::Prompt(_)) |
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
                            (Value::Array(_), _) | (_, Value::Array(_)) |
                            (Value::Dict(_), _) | (_, Value::Dict(_)) |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Prompt(_), _) | (_, Value::Prompt(_)) =>
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
                            (Value::Array(_), _) | (_, Value::Array(_)) |
                            (Value::Dict(_), _) | (_, Value::Dict(_)) |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Prompt(_), _) | (_, Value::Prompt(_)) =>
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
                            (Value::Array(_), _) | (_, Value::Array(_)) |
                            (Value::Dict(_), _) | (_, Value::Dict(_)) |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Prompt(_), _) | (_, Value::Prompt(_)) =>
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
                            (Value::Array(_), _) | (_, Value::Array(_)) |
                            (Value::Dict(_), _) | (_, Value::Dict(_)) |
                            (Value::Struct(_), _) | (_, Value::Struct(_)) |
                            (Value::BoundMethod(_), _) | (_, Value::BoundMethod(_)) |
                            (Value::Builtin(_), _) | (_, Value::Builtin(_)) |
                            (Value::Prompt(_), _) | (_, Value::Prompt(_)) =>
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

        // In the tree-walk evaluator, await just evaluates the expression immediately.
        Expr::Await { expr, .. } => eval_expr(expr, env),

        // `prompt <expr>` as an expression: evaluate body to string, wrap as Prompt value.
        Expr::PromptLiteral { body, span } => {
            match eval_expr(body, env)? {
                Value::Str(text) => Ok(Value::Prompt(text)),
                other => Err(JadeError::TypeError {
                    op: format!("prompt body must be a string, got {}", value_type_name(&other)),
                    span: *span,
                }),
            }
        }

        Expr::PromptDeref { expr, output_type, span } => {
            // 1. Evaluate the target expression and extract the prompt text.
            let prompt_text = match eval_expr(expr, env)? {
                Value::Prompt(text) => text,
                _ => return Err(JadeError::NotAPrompt { name: expr_display(expr), span: *span }),
            };

            // 2. Call the backend with the current conversation history.
            let initial_resp = {
                let backend = env.inference_backend.as_ref()
                    .ok_or(JadeError::MissingApiKey { span: *span })?;
                let req = llm::InferenceRequest {
                    prompt: prompt_text.clone(),
                    model: env.default_model.clone(),
                    history: env.conversation_history.clone(),
                    max_tokens: DEFAULT_MAX_TOKENS,
                };
                crate::llm::infer_sync(backend.as_ref(), req, *span)?
            };

            // 3. Record the exchange in conversation history and update token count.
            env.conversation_history.push(llm::Message { role: "user".to_string(), content: prompt_text });
            env.conversation_history.push(llm::Message { role: "assistant".to_string(), content: initial_resp.text.clone() });
            env.token_count += initial_resp.tokens_used;
            let new_token_count = env.token_count;
            env.set_session_var("__tokens__", Value::Int(new_token_count));

            // 4. Untyped dereference: return raw LLM response as a string.
            let Some(type_name) = output_type else {
                return Ok(Value::Str(initial_resp.text));
            };

            // 5. Typed dereference: retry loop with coercion.
            let max_retries = env.max_retries;
            let history_len_before_retries = env.conversation_history.len();
            let mut current_response = initial_resp.text;
            // Clone struct_defs so the retry loop can borrow other env fields freely.
            let struct_defs = env.struct_defs.clone();

            // Use a larger token budget for complex types that produce more output.
            let retry_max_tokens = if matches!(type_name.as_str(), "int" | "float" | "bool" | "str") {
                RETRY_MAX_TOKENS
            } else {
                RETRY_MAX_TOKENS_COMPLEX
            };

            // Each loop iteration checks the current response, then sends the coercion
            // error back to the LLM as a correction prompt. The user never sees these
            // intermediate failures — only a PromptOverflow if all retries are exhausted.
            for attempt in 0..max_retries {
                match coerce_to_type(&current_response, type_name, &struct_defs) {
                    Ok(v) => {
                        env.conversation_history.truncate(history_len_before_retries);
                        return Ok(v);
                    }
                    Err(correction) => {
                        // Record the failed attempt in __retry_log__
                        let entry = Value::Str(format!(
                            "attempt {}: response={:?} hint={:?}",
                            attempt + 1, current_response.trim(), correction
                        ));
                        if let Some(log_val) = env.scopes.first_mut()
                            .and_then(|s| s.get_mut("__retry_log__"))
                        {
                            if let Value::Array(log) = log_val {
                                log.push(entry);
                            }
                        }

                        // Send the coercion error back to the LLM and collect its correction.
                        let retry_resp = {
                            let backend = env.inference_backend.as_ref()
                                .ok_or(JadeError::MissingApiKey { span: *span })?;
                            crate::llm::infer_sync(backend.as_ref(), llm::InferenceRequest {
                                prompt: correction.clone(),
                                model: env.default_model.clone(),
                                history: env.conversation_history.clone(),
                                max_tokens: retry_max_tokens,
                            }, *span)?
                        };
                        env.conversation_history.push(llm::Message {
                            role: "user".to_string(), content: correction,
                        });
                        env.conversation_history.push(llm::Message {
                            role: "assistant".to_string(), content: retry_resp.text.clone(),
                        });
                        current_response = retry_resp.text;
                    }
                }
            }

            // Final coercion attempt after all retry corrections have been sent.
            match coerce_to_type(&current_response, type_name, &struct_defs) {
                Ok(v) => {
                    env.conversation_history.truncate(history_len_before_retries);
                    Ok(v)
                }
                Err(_) => {
                    env.conversation_history.truncate(history_len_before_retries);
                    Err(JadeError::PromptOverflow {
                        name: expr_display(expr),
                        attempts: max_retries + 1,
                        span: *span,
                    })
                }
            }
        }
    }
}

/// Strip markdown code fences that LLMs often wrap JSON in (``` or ```json).
fn extract_json_text(text: &str) -> &str {
    let t = text.trim();
    let inner = t
        .strip_prefix("```json").or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    inner.unwrap_or(t)
}

/// Recursively convert a `serde_json::Value` to a Jade `Value`.
fn json_to_value(json: &serde_json::Value) -> std::result::Result<Value, String> {
    match json {
        serde_json::Value::Null => Err("null is not a valid Jade value".to_string()),
        serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { Ok(Value::Int(i)) }
            else if let Some(f) = n.as_f64() { Ok(Value::Float(f)) }
            else { Err(format!("number {} cannot be represented as int or float", n)) }
        }
        serde_json::Value::String(s) => Ok(Value::Str(s.clone())),
        serde_json::Value::Array(arr) => arr.iter().enumerate()
            .map(|(i, v)| json_to_value(v).map_err(|e| format!("element {}: {}", i, e)))
            .collect::<std::result::Result<Vec<Value>, String>>()
            .map(Value::Array),
        serde_json::Value::Object(obj) => obj.iter()
            .map(|(k, v)| json_to_value(v)
                .map(|val| (k.clone(), val))
                .map_err(|e| format!("field '{}': {}", k, e)))
            .collect::<std::result::Result<HashMap<String, Value>, String>>()
            .map(Value::Dict),
    }
}

/// Summarise struct field names and optionality for LLM error messages.
fn field_summary(def: &[StructFieldDef]) -> String {
    def.iter().map(|f| match f {
        StructFieldDef::Required(n)      => format!("{} (required)", n),
        StructFieldDef::Let { name, .. } => format!("{} (optional)", name),
        StructFieldDef::Prompt { name, .. } => format!("{} (prompt, optional)", name),
    }).collect::<Vec<_>>().join(", ")
}

/// Parse an LLM JSON response into a struct `Value`.
fn coerce_struct(
    text: &str,
    type_name: &str,
    def: &[StructFieldDef],
) -> std::result::Result<Value, String> {
    let raw = extract_json_text(text);
    let json: serde_json::Value = serde_json::from_str(raw).map_err(|e| format!(
        "Your response could not be parsed as a {} struct: {}. \
         Respond with a JSON object with fields: {}.",
        type_name, e, field_summary(def)
    ))?;
    let obj = json.as_object().ok_or_else(|| format!(
        "Your response is not a JSON object. \
         Respond with a JSON object for struct '{}' with fields: {}.",
        type_name, field_summary(def)
    ))?;

    let mut fields: HashMap<String, Value> = HashMap::new();
    for field_def in def {
        match field_def {
            StructFieldDef::Required(name) => {
                let raw_val = obj.get(name.as_str()).ok_or_else(|| format!(
                    "Missing required field '{}' for struct '{}'. \
                     Respond with a JSON object containing all required fields: {}.",
                    name, type_name, field_summary(def)
                ))?;
                let val = json_to_value(raw_val).map_err(|e| format!(
                    "Field '{}' is invalid: {}. \
                     Respond with a corrected JSON object for struct '{}'.",
                    name, e, type_name
                ))?;
                fields.insert(name.clone(), val);
            }
            StructFieldDef::Let { name, .. } => {
                if let Some(raw_val) = obj.get(name.as_str()) {
                    let val = json_to_value(raw_val).map_err(|e| format!(
                        "Field '{}' is invalid: {}. \
                         Respond with a corrected JSON object for struct '{}'.",
                        name, e, type_name
                    ))?;
                    fields.insert(name.clone(), val);
                }
            }
            StructFieldDef::Prompt { name, .. } => {
                if let Some(raw_val) = obj.get(name.as_str()) {
                    let s = raw_val.as_str().ok_or_else(|| format!(
                        "Prompt field '{}' must be a string value.", name
                    ))?;
                    fields.insert(name.clone(), Value::Prompt(s.to_string()));
                }
            }
        }
    }

    Ok(Value::Struct(Rc::new(RefCell::new(StructInstance {
        type_name: type_name.to_string(),
        fields,
    }))))
}

/// Try to coerce a raw LLM response string to a Jade typed value.
/// Returns `Ok(value)` on success or `Err(correction_prompt)` on failure —
/// the correction is fed back to the LLM, never surfaced to the user directly.
fn coerce_to_type(
    text: &str,
    type_name: &str,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> std::result::Result<Value, String> {
    match type_name {
        "int" => text.trim().parse::<i64>().map(Value::Int).map_err(|_| format!(
            "Your response {:?} could not be parsed as an integer. \
             Respond with only a plain integer, e.g. 42.",
            text.trim()
        )),
        "float" => text.trim().parse::<f64>().map(Value::Float).map_err(|_| format!(
            "Your response {:?} could not be parsed as a float. \
             Respond with only a plain float, e.g. 3.14.",
            text.trim()
        )),
        "str" => Ok(Value::Str(text.trim().to_string())),
        "bool" => match text.trim().to_lowercase().as_str() {
            "true"  => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!(
                "Your response {:?} could not be parsed as a boolean. \
                 Respond with only 'true' or 'false'.",
                text.trim()
            )),
        },
        "Array" | "array" => {
            let raw = extract_json_text(text);
            serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON array: {}. \
                     Respond with only a JSON array, e.g. [1, \"two\", true].",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Array(arr) => arr.iter().enumerate()
                        .map(|(i, elem)| json_to_value(elem)
                            .map_err(|e| format!("element {}: {}", i, e)))
                        .collect::<std::result::Result<Vec<Value>, String>>()
                        .map(Value::Array)
                        .map_err(|e| format!(
                            "Your response array could not be fully converted: {}. \
                             Respond with only a JSON array of int, float, bool, or string values.",
                            e
                        )),
                    _ => Err("Your response is not a JSON array. \
                              Respond with only a JSON array, e.g. [1, \"two\", true].".to_string()),
                })
        }
        "Dict" | "dict" => {
            let raw = extract_json_text(text);
            serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON object: {}. \
                     Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Object(obj) => obj.iter()
                        .map(|(k, val)| json_to_value(val)
                            .map(|v| (k.clone(), v))
                            .map_err(|e| format!("field '{}': {}", k, e)))
                        .collect::<std::result::Result<HashMap<String, Value>, String>>()
                        .map(Value::Dict)
                        .map_err(|e| format!(
                            "Your response dict could not be fully converted: {}. \
                             Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                            e
                        )),
                    _ => Err("Your response is not a JSON object. \
                              Respond with only a JSON object, e.g. {\"key\": \"value\"}.".to_string()),
                })
        }
        name => {
            if let Some(def) = struct_defs.get(name) {
                coerce_struct(text, name, def)
            } else {
                Err(format!(
                    "Unknown type '{}'. Cannot coerce LLM response to this type.", name
                ))
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
        evaluate(program, LlmOpts::default())
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
    fn test_eval_neg_paren_ok() {
        // -(expr) is now valid syntax.
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
    fn test_eval_nested_fn_ok() {
        // Nested function definitions are now allowed.
        let env = eval_src("fn outer() {\n    fn inner() {\n        return 1\n    }\n    return 2\n}").unwrap();
        let _ = get(&env, "outer");
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

    // ── pipe operator |> ─────────────────────────────────────────────────────

    #[test]
    fn test_eval_pipe_simple() {
        let src = "fn double(x) {\n    return x * 2\n}\nlet n = 5 |> double";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(10)));
    }

    #[test]
    fn test_eval_pipe_chained() {
        let src = "fn double(x) {\n    return x * 2\n}\nlet m = 3 |> double |> double";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "m"), Value::Int(12)));
    }

    #[test]
    fn test_eval_pipe_with_extra_arg() {
        let src = "fn add(a, b) {\n    return a + b\n}\nlet r = 5 |> add(3)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "r"), Value::Int(8)));
    }

    #[test]
    fn test_eval_pipe_with_string() {
        let src = "fn greet(name) {\n    return f\"hello, {name}!\"\n}\nlet g = \"Jade\" |> greet";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "g"), Value::Str(ref v) if v == "hello, Jade!"));
    }

    #[test]
    fn test_eval_pipe_arithmetic_lhs() {
        let src = "fn double(x) {\n    return x * 2\n}\nlet x = (2 + 3) |> double";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(10)));
    }

    // ── arrays ───────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_array_empty() {
        let env = eval_src("let a = []").unwrap();
        assert!(matches!(get(&env, "a"), Value::Array(ref v) if v.is_empty()));
    }

    #[test]
    fn test_eval_array_int_elements() {
        let env = eval_src("let a = [10, 20, 30]").unwrap();
        let Value::Array(ref vec) = get(&env, "a") else { panic!("not an array") };
        assert!(matches!(vec[0], Value::Int(10)));
        assert!(matches!(vec[1], Value::Int(20)));
        assert!(matches!(vec[2], Value::Int(30)));
    }

    #[test]
    fn test_eval_array_index_first() {
        let env = eval_src("let a = [10, 20, 30]\nlet x = a[0]").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(10)));
    }

    #[test]
    fn test_eval_array_index_last() {
        let env = eval_src("let a = [10, 20, 30]\nlet x = a[2]").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(30)));
    }

    #[test]
    fn test_eval_array_index_out_of_bounds() {
        let err = eval_src("let a = [1]\nlet x = a[1]").unwrap_err();
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: 1, len: 1, .. }));
    }

    #[test]
    fn test_eval_array_index_negative() {
        let err = eval_src("let a = [1]\nlet x = a[-1]").unwrap_err();
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn test_eval_array_index_assign() {
        let env = eval_src("let a = [1, 2, 3]\na[1] = 99\nlet x = a[1]").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(99)));
    }

    #[test]
    fn test_eval_array_value_semantics() {
        // b is an independent copy — mutating b does not affect a
        let env = eval_src("let a = [1, 2]\nlet b = a\nb[0] = 42\nlet x = a[0]").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(1)));
    }

    #[test]
    fn test_eval_array_heterogeneous_ok() {
        // Heterogeneous arrays are now allowed.
        let env = eval_src("let a = [1, 2.0, true, \"hello\"]").unwrap();
        let _ = get(&env, "a");
    }

    #[test]
    fn test_eval_array_nested() {
        let env = eval_src("let m = [[1, 2], [3, 4]]\nlet x = m[0][1]").unwrap();
        assert!(matches!(get(&env, "x"), Value::Int(2)));
    }

    #[test]
    fn test_eval_array_trailing_comma() {
        let env = eval_src("let a = [1, 2, 3,]").unwrap();
        let Value::Array(ref vec) = get(&env, "a") else { panic!("not an array") };
        assert_eq!(vec.len(), 3);
    }

    #[test]
    fn test_eval_len_array() {
        let env = eval_src("let a = [1, 2, 3]\nlet n = len(a)").unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(3)));
    }

    #[test]
    fn test_eval_len_string() {
        let env = eval_src("let n = len(\"hello\")").unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(5)));
    }

    #[test]
    fn test_eval_len_empty_array() {
        let env = eval_src("let n = len([])").unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(0)));
    }

    #[test]
    fn test_eval_len_type_error() {
        let err = eval_src("let n = len(42)").unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    // ── interfaces ────────────────────────────────────────────────────────────

    #[test]
    fn test_eval_interface_basic() {
        let src = concat!(
            "interface Displayable {\n",
            "    fn to_str(self) -> str\n",
            "}\n",
            "struct Point {\n    x,\n    y\n}\n",
            "extend Point: Displayable {\n",
            "    fn to_str(self) -> str {\n",
            "        return \"point\"\n",
            "    }\n",
            "}\n",
            "let p = Point { x: 1, y: 2 }\n",
            "let s = p.to_str()\n",
        );
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "s"), Value::Str(ref v) if v == "point"));
    }

    #[test]
    fn test_eval_interface_missing_method_error() {
        let src = concat!(
            "interface Displayable {\n",
            "    fn to_str(self) -> str\n",
            "}\n",
            "struct Point {\n    x,\n    y\n}\n",
            // Extend block does NOT implement `to_str` — should error
            "extend Point: Displayable {\n",
            "    fn area(self) {\n",
            "        return 0\n",
            "    }\n",
            "}\n",
        );
        let err = eval_src(src).unwrap_err();
        assert!(matches!(err, JadeError::MissingInterfaceMethod { .. }));
    }

    #[test]
    fn test_eval_interface_undefined_error() {
        let src = concat!(
            "struct Point {\n    x,\n    y\n}\n",
            // Interface `Displayable` is never defined
            "extend Point: Displayable {\n",
            "    fn to_str(self) -> str {\n",
            "        return \"point\"\n",
            "    }\n",
            "}\n",
        );
        let err = eval_src(src).unwrap_err();
        assert!(matches!(err, JadeError::UndefinedInterface { .. }));
    }

    #[test]
    fn test_eval_interface_impl_registered() {
        let src = concat!(
            "interface Showable {\n",
            "    fn show(self) -> str\n",
            "}\n",
            "struct Box {\n    val\n}\n",
            "extend Box: Showable {\n",
            "    fn show(self) -> str {\n",
            "        return \"box\"\n",
            "    }\n",
            "}\n",
        );
        let env = eval_src(src).unwrap();
        assert!(env.interface_impls.get("Box").map_or(false, |v| v.contains(&"Showable".to_string())));
    }

    // ── LLM / prompt ────────────────────────────────────────────────────────

    fn eval_with_mock(src: &str, responses: Vec<&str>) -> Result<Env> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        let backend = std::sync::Arc::new(crate::llm::MockBackend::new(responses));
        evaluate(program, LlmOpts {
            backend: Some(backend),
            default_model: "mock-model".to_string(),
            max_retries: 3,
        })
    }

    #[test]
    fn test_eval_prompt_decl_stores_prompt_value() {
        let env = eval_src("prompt p = \"hello\"").unwrap();
        assert!(matches!(get(&env, "p"), Value::Prompt(t) if t == "hello"));
    }

    #[test]
    fn test_eval_prompt_deref_no_backend_returns_error() {
        let tokens = lexer::tokenize("prompt p = \"hi\"\nlet x = ?p").expect("lex");
        let program = parser::parse(tokens).expect("parse");
        let err = evaluate(program, LlmOpts::default()).unwrap_err();
        assert!(matches!(err, JadeError::MissingApiKey { .. }));
    }

    #[test]
    fn test_eval_prompt_deref_not_a_prompt_returns_error() {
        let err = eval_with_mock("let x = 5\nlet y = ?x", vec!["42"]).unwrap_err();
        assert!(matches!(err, JadeError::NotAPrompt { .. }));
    }

    #[test]
    fn test_eval_prompt_deref_field_access_no_backend() {
        // ?obj.field resolves the prompt field and tries to call the backend
        let tokens = lexer::tokenize(
            "struct Agent {\n    prompt system = \"helpful\"\n}\nlet a = Agent {}\nlet r = ?a.system"
        ).expect("lex");
        let program = parser::parse(tokens).expect("parse");
        let err = evaluate(program, LlmOpts::default()).unwrap_err();
        assert!(matches!(err, JadeError::MissingApiKey { .. }));
    }

    #[test]
    fn test_eval_prompt_deref_field_access_not_a_prompt() {
        // ?obj.field where the field is not a prompt → NotAPrompt error
        let err = eval_with_mock(
            "struct S {\n    x,\n}\nlet s = S { x: 42 }\nlet r = ?s.x",
            vec![]
        ).unwrap_err();
        assert!(matches!(err, JadeError::NotAPrompt { .. }));
    }

    #[test]
    fn test_eval_prompt_deref_field_access_with_mock() {
        // ?obj.field works end-to-end with a mock backend
        let env = eval_with_mock(
            "struct Agent {\n    prompt system = \"Say hello\"\n}\nlet a = Agent {}\nlet r = ?a.system",
            vec!["hello!"]
        ).unwrap();
        assert!(matches!(get(&env, "r"), Value::Str(s) if s == "hello!"));
    }

    #[test]
    fn test_eval_typed_deref_int_success() {
        let env = eval_with_mock("prompt p = \"What is 2+2?\"\nlet n = ?p |> int", vec!["4"]).unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(4)));
    }

    #[test]
    fn test_eval_typed_deref_float_success() {
        let env = eval_with_mock("prompt p = \"pi\"\nlet n = ?p |> float", vec!["3.14"]).unwrap();
        assert!(matches!(get(&env, "n"), Value::Float(f) if (f - 3.14).abs() < 0.001));
    }

    #[test]
    fn test_eval_typed_deref_bool_success() {
        let env = eval_with_mock("prompt p = \"true?\"\nlet n = ?p |> bool", vec!["true"]).unwrap();
        assert!(matches!(get(&env, "n"), Value::Bool(true)));
    }

    #[test]
    fn test_eval_typed_deref_str_success() {
        let env = eval_with_mock("prompt p = \"hello\"\nlet n = ?p |> str", vec!["world"]).unwrap();
        assert!(matches!(get(&env, "n"), Value::Str(s) if s == "world"));
    }

    #[test]
    fn test_eval_typed_deref_overflow() {
        // 4 responses all non-int: initial + 3 retries = 4 calls, all fail
        let err = eval_with_mock(
            "prompt p = \"bad\"\nlet n = ?p |> int",
            vec!["oops", "still wrong", "nope", "nah"],
        ).unwrap_err();
        assert!(matches!(err, JadeError::PromptOverflow { .. }));
    }

    #[test]
    fn test_eval_tokens_incremented_after_deref() {
        let env = eval_with_mock("prompt p = \"hi\"\nlet x = ?p", vec!["hello"]).unwrap();
        // MockBackend returns tokens_used = 10 per call
        assert!(matches!(get(&env, "__tokens__"), Value::Int(n) if n > 0));
    }

    #[test]
    fn test_eval_untyped_deref_returns_str() {
        let env = eval_with_mock("prompt p = \"test\"\nlet x = ?p", vec!["result"]).unwrap();
        assert!(matches!(get(&env, "x"), Value::Str(s) if s == "result"));
    }

    #[test]
    fn test_eval_typed_deref_retry_succeeds_on_second_attempt() {
        // First response fails coercion, second succeeds
        let env = eval_with_mock(
            "prompt p = \"number?\"\nlet n = ?p |> int",
            vec!["not a number", "42"],
        ).unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(42)));
    }

    // ── dict tests ───────────────────────────────────────────────────────────

    #[test]
    fn test_eval_dict_empty() {
        let env = eval_src("let d = {}").unwrap();
        assert!(matches!(get(&env, "d"), Value::Dict(m) if m.is_empty()));
    }

    #[test]
    fn test_eval_dict_string_values() {
        let env = eval_src(r#"let d = {"name": "jade", "lang": "cool"}"#).unwrap();
        match get(&env, "d") {
            Value::Dict(m) => {
                assert!(matches!(m.get("name"), Some(Value::Str(s)) if s == "jade"));
                assert!(matches!(m.get("lang"), Some(Value::Str(s)) if s == "cool"));
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn test_eval_dict_int_value() {
        let env = eval_src(r#"let d = {"x": 42}"#).unwrap();
        match get(&env, "d") {
            Value::Dict(m) => assert!(matches!(m.get("x"), Some(Value::Int(42)))),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn test_eval_dict_index_read() {
        let src = "let d = {\"k\": 7}\nlet v = d[\"k\"]";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "v"), Value::Int(7)));
    }

    #[test]
    fn test_eval_dict_index_read_string_value() {
        let src = "let d = {\"a\": \"hello\"}\nlet v = d[\"a\"]";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "v"), Value::Str(s) if s == "hello"));
    }

    #[test]
    fn test_eval_dict_key_not_found() {
        let src = "let d = {\"x\": 1}\nlet v = d[\"y\"]";
        let err = eval_src(src).unwrap_err();
        assert!(matches!(err, JadeError::KeyNotFound { key, .. } if key == "y"));
    }

    #[test]
    fn test_eval_dict_index_assign_existing_key() {
        let src = "let d = {\"v\": 1}\nd[\"v\"] = 99";
        let env = eval_src(src).unwrap();
        match get(&env, "d") {
            Value::Dict(m) => assert!(matches!(m.get("v"), Some(Value::Int(99)))),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn test_eval_dict_index_assign_new_key() {
        let src = "let d = {}\nd[\"k\"] = 5";
        let env = eval_src(src).unwrap();
        match get(&env, "d") {
            Value::Dict(m) => assert!(matches!(m.get("k"), Some(Value::Int(5)))),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn test_eval_dict_len() {
        let src = "let d = {\"a\": 1, \"b\": 2, \"c\": 3}\nlet n = len(d)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(3)));
    }

    #[test]
    fn test_eval_dict_len_empty() {
        let env = eval_src("let d = {}\nlet n = len(d)").unwrap();
        assert!(matches!(get(&env, "n"), Value::Int(0)));
    }

    #[test]
    fn test_eval_dict_value_semantics() {
        // Assigning a dict copies it; mutation of the copy does not affect original
        let src = "let d = {\"x\": 1}\nlet d2 = d\nd2[\"x\"] = 99";
        let env = eval_src(src).unwrap();
        match get(&env, "d") {
            Value::Dict(m) => assert!(matches!(m.get("x"), Some(Value::Int(1)))),
            _ => panic!("expected Dict"),
        }
        match get(&env, "d2") {
            Value::Dict(m) => assert!(matches!(m.get("x"), Some(Value::Int(99)))),
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn test_eval_dict_variable_key() {
        // Key expression that evaluates to a string at runtime
        let src = "let k = \"name\"\nlet d = {k: \"jade\"}\nlet v = d[\"name\"]";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "v"), Value::Str(s) if s == "jade"));
    }

    #[test]
    fn test_eval_dict_non_string_key_type_error() {
        let src = "let d = {1: \"oops\"}";
        let err = eval_src(src).unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    #[test]
    fn test_eval_dict_non_string_index_type_error() {
        let src = "let d = {\"x\": 1}\nlet v = d[0]";
        let err = eval_src(src).unwrap_err();
        assert!(matches!(err, JadeError::TypeError { .. }));
    }

    // ── struct field defaults ─────────────────────────────────────────────────

    #[test]
    fn test_eval_struct_default_omitted() {
        let env = eval_src(
            "struct Config {\n    let host = \"localhost\"\n}\nlet c = Config {}\nlet h = c.host"
        ).unwrap();
        assert!(matches!(env.get("h"), Some(Value::Str(s)) if s == "localhost"));
    }

    #[test]
    fn test_eval_struct_default_overridden() {
        let env = eval_src(
            "struct Config {\n    let host = \"localhost\"\n}\nlet c = Config { host: \"example.com\" }\nlet h = c.host"
        ).unwrap();
        assert!(matches!(env.get("h"), Some(Value::Str(s)) if s == "example.com"));
    }

    #[test]
    fn test_eval_struct_all_defaults_empty_literal() {
        let env = eval_src(
            "struct Config {\n    let host = \"localhost\"\n    let port = 8080\n}\nlet c = Config {}\nlet h = c.host\nlet p = c.port"
        ).unwrap();
        assert!(matches!(env.get("h"), Some(Value::Str(s)) if s == "localhost"));
        assert!(matches!(env.get("p"), Some(Value::Int(8080))));
    }

    #[test]
    fn test_eval_struct_required_still_required() {
        let err = eval_src(
            "struct Mixed {\n    x,\n    let label = \"origin\"\n}\nlet m = Mixed {}"
        ).unwrap_err();
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn test_eval_struct_mixed_fields() {
        let env = eval_src(
            "struct Mixed {\n    x,\n    y,\n    let label = \"origin\"\n}\nlet m = Mixed { x: 1, y: 2 }\nlet lbl = m.label"
        ).unwrap();
        assert!(matches!(env.get("lbl"), Some(Value::Str(s)) if s == "origin"));
    }

    #[test]
    fn test_eval_struct_prompt_field_default() {
        let env = eval_src(
            "struct Agent {\n    prompt system = \"You are helpful\"\n}\nlet a = Agent {}\nlet s = a.system"
        ).unwrap();
        assert!(matches!(env.get("s"), Some(Value::Prompt(t)) if t == "You are helpful"));
    }

    #[test]
    fn test_eval_struct_prompt_field_override() {
        let env = eval_src(
            "struct Agent {\n    prompt system = \"You are helpful\"\n}\nlet a = Agent { system: \"Custom\" }\nlet s = a.system"
        ).unwrap();
        assert!(matches!(env.get("s"), Some(Value::Prompt(t)) if t == "Custom"));
    }

    #[test]
    fn test_eval_struct_prompt_field_non_string_error() {
        let err = eval_src(
            "struct Bad {\n    prompt sys = 42\n}\nlet b = Bad {}"
        ).unwrap_err();
        assert!(matches!(err, JadeError::PromptFieldNotStr { .. }));
    }

    #[test]
    fn test_eval_struct_prompt_field_override_non_string_error() {
        let err = eval_src(
            "struct Agent {\n    prompt system = \"ok\"\n}\nlet a = Agent { system: 99 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::PromptFieldNotStr { .. }));
    }

    #[test]
    fn test_eval_struct_extra_field_still_errors_with_defaults() {
        let err = eval_src(
            "struct Agent {\n    let name = \"Jade\"\n}\nlet a = Agent { name: \"x\", extra: 1 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    #[test]
    fn test_eval_struct_duplicate_field_error() {
        let err = eval_src(
            "struct Point {\n    x,\n    y\n}\nlet p = Point { x: 1, y: 2, x: 3 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::DuplicateField { field, .. } if field == "x"));
    }

    #[test]
    fn test_eval_struct_default_references_variable() {
        // Default expressions are evaluated lazily in the current env
        let env = eval_src(
            "let base = 10\nstruct S {\n    let x = base\n}\nlet s = S {}\nlet v = s.x"
        ).unwrap();
        assert!(matches!(env.get("v"), Some(Value::Int(10))));
    }

    #[test]
    fn test_eval_struct_required_after_let_field() {
        // Required field after a Let field: omitting it still errors
        let err = eval_src(
            "struct S {\n    let x = 0,\n    y\n}\nlet s = S { x: 1 }"
        ).unwrap_err();
        assert!(matches!(err, JadeError::MissingField { field, .. } if field == "y"));
    }
}
