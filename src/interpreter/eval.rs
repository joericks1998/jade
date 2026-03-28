use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result},
};

// ── Value ────────────────────────────────────────────────────────────────────

/// A runtime value in Jade.
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Fn(Rc<FnValue>),
}

/// The data behind a function value: its signature, body, and defining scope.
pub struct FnValue {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    /// The environment in which this function was defined. Used as the parent
    /// scope for each call frame, enabling cross-function calls and recursion.
    pub def_env: Rc<RefCell<Env>>,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(i)   => write!(f, "Int({})", i),
            Value::Float(v) => write!(f, "Float({})", v),
            Value::Bool(b)  => write!(f, "Bool({})", b),
            Value::Fn(fv)   => write!(f, "Fn({})", fv.params.join(", ")),
        }
    }
}

impl std::fmt::Debug for FnValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<fn({})>", self.params.join(", "))
    }
}

// ── Environment ──────────────────────────────────────────────────────────────

/// Holds variable bindings for one scope frame.
/// Reads walk the parent chain; writes always target the current frame.
#[derive(Debug)]
pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Rc<RefCell<Env>>>,
}

impl Env {
    /// Create a new, empty top-level environment.
    pub fn new() -> Self {
        Env { vars: HashMap::new(), parent: None }
    }

    /// Create a child frame whose lookups fall through to `parent`.
    fn new_child(parent: Rc<RefCell<Env>>) -> Self {
        Env { vars: HashMap::new(), parent: Some(parent) }
    }

    /// Look up a variable by name, walking the parent chain.
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        if let Some(p) = &self.parent {
            return p.borrow().get(name);
        }
        None
    }

    /// Bind a name to a value in the current frame (never writes to parent).
    pub fn set(&mut self, name: String, value: Value) {
        self.vars.insert(name, value);
    }

    /// Iterate over bindings in this frame only (used by `-v` output).
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.vars.iter()
    }
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Walk the program and return the populated top-level environment.
pub fn evaluate(program: Program) -> Result<Env> {
    let env_rc = Rc::new(RefCell::new(Env::new()));
    eval_block(program.stmts, Rc::clone(&env_rc))?;
    // FnValues hold Rc clones of env_rc, so try_unwrap would fail.
    // Snapshot the top-level frame into a plain Env for the caller.
    let mut result = Env::new();
    for (k, v) in env_rc.borrow().entries() {
        result.set(k.clone(), v.clone());
    }
    Ok(result)
}

// ── Statement evaluator ───────────────────────────────────────────────────────

/// Evaluate a list of statements in `env`.
/// Returns `Some(value)` if a `return` was executed, `None` otherwise.
fn eval_block(stmts: Vec<Stmt>, env: Rc<RefCell<Env>>) -> Result<Option<Value>> {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = eval_expr(value, &env)?;
                env.borrow_mut().set(name, v);
            }

            Stmt::FnDef { name, params, body, .. } => {
                let fn_val = FnValue {
                    params,
                    body,
                    def_env: Rc::clone(&env),
                };
                env.borrow_mut().set(name, Value::Fn(Rc::new(fn_val)));
            }

            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(expr) => eval_expr(expr, &env)?,
                    None => Value::Int(0),
                };
                return Ok(Some(v));
            }

            Stmt::If { condition, then_body, else_body, span } => {
                let cond = eval_expr(condition, &env)?;
                match cond {
                    Value::Bool(true) => {
                        if let Some(v) = eval_block(then_body, Rc::clone(&env))? {
                            return Ok(Some(v));
                        }
                    }
                    Value::Bool(false) => {
                        if let Some(body) = else_body {
                            if let Some(v) = eval_block(body, Rc::clone(&env))? {
                                return Ok(Some(v));
                            }
                        }
                    }
                    _ => return Err(JadeError::TypeError {
                        op: "if".to_string(),
                        span,
                    }),
                }
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
        // Callers must guard against Bool and Fn before calling to_float.
        Value::Bool(_)  => unreachable!("to_float called on Bool"),
        Value::Fn(_)    => unreachable!("to_float called on Fn"),
    }
}

/// Evaluate one expression against the current environment, returning its value.
fn eval_expr(expr: Expr, env: &Rc<RefCell<Env>>) -> Result<Value> {
    match expr {
        Expr::Integer { value, .. } => Ok(Value::Int(value)),
        Expr::Float   { value, .. } => Ok(Value::Float(value)),
        Expr::Bool    { value, .. } => Ok(Value::Bool(value)),

        Expr::Identifier { name, span } => {
            env.borrow().get(&name).ok_or(JadeError::UndefinedVariable { name, span })
        }

        Expr::Call { callee, args, span } => {
            let callee_val = eval_expr(*callee, env)?;
            let Value::Fn(fn_rc) = callee_val else {
                return Err(JadeError::NotCallable { span });
            };

            // Clone what we need before releasing the Rc borrow
            let params  = fn_rc.params.clone();
            let body    = fn_rc.body.clone();
            let def_env = Rc::clone(&fn_rc.def_env);

            if args.len() != params.len() {
                return Err(JadeError::ArityMismatch {
                    expected: params.len(),
                    got: args.len(),
                    span,
                });
            }

            // Evaluate arguments in the *caller's* scope
            let mut arg_vals = Vec::with_capacity(args.len());
            for arg_expr in args {
                arg_vals.push(eval_expr(arg_expr, env)?);
            }

            // Build the call frame as a child of the function's defining scope
            let mut call_env = Env::new_child(def_env);
            for (param, val) in params.into_iter().zip(arg_vals) {
                call_env.set(param, val);
            }
            let call_env_rc = Rc::new(RefCell::new(call_env));

            Ok(eval_block(body, call_env_rc)?.unwrap_or(Value::Int(0)))
        }

        Expr::BinOp { op, left, right, span } => {
            match op {
                // Short-circuit logical ops
                BinOpKind::And => {
                    let l = eval_expr(*left, env)?;
                    match l {
                        Value::Bool(false) => Ok(Value::Bool(false)),
                        Value::Bool(true)  => match eval_expr(*right, env)? {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _              => Err(JadeError::TypeError { op: "&&".to_string(), span }),
                        },
                        _ => Err(JadeError::TypeError { op: "&&".to_string(), span }),
                    }
                }
                BinOpKind::Or => {
                    let l = eval_expr(*left, env)?;
                    match l {
                        Value::Bool(true)  => Ok(Value::Bool(true)),
                        Value::Bool(false) => match eval_expr(*right, env)? {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _              => Err(JadeError::TypeError { op: "||".to_string(), span }),
                        },
                        _ => Err(JadeError::TypeError { op: "||".to_string(), span }),
                    }
                }

                // All other binary ops evaluate both operands eagerly
                other => {
                    let l = eval_expr(*left, env)?;
                    let r = eval_expr(*right, env)?;
                    match other {
                        BinOpKind::Add => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_add(b)
                                .ok_or(JadeError::IntegerOverflow { span })
                                .map(Value::Int),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   => Err(JadeError::TypeError { op: "+".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) + to_float(b))),
                        },
                        BinOpKind::Sub => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_sub(b)
                                .ok_or(JadeError::IntegerOverflow { span })
                                .map(Value::Int),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   => Err(JadeError::TypeError { op: "-".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) - to_float(b))),
                        },
                        BinOpKind::Mul => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => a.checked_mul(b)
                                .ok_or(JadeError::IntegerOverflow { span })
                                .map(Value::Int),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   => Err(JadeError::TypeError { op: "*".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) * to_float(b))),
                        },
                        BinOpKind::Div => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Int(a / b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   => Err(JadeError::TypeError { op: "/".to_string(), span }),
                            (a, b) => {
                                let bf = to_float(b);
                                if bf == 0.0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Float(to_float(a) / bf)) }
                            }
                        },
                        BinOpKind::Mod => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::RemainderByZero { span }) } else { Ok(Value::Int(a % b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) |
                            (Value::Fn(_),   _) | (_, Value::Fn(_))   => Err(JadeError::TypeError { op: "%".to_string(), span }),
                            (a, b) => {
                                let bf = to_float(b);
                                if bf == 0.0 { Err(JadeError::RemainderByZero { span }) } else { Ok(Value::Float(to_float(a) % bf)) }
                            }
                        },

                        BinOpKind::BitAnd => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
                            _ => Err(JadeError::TypeError { op: "&".to_string(), span }),
                        },
                        BinOpKind::BitOr => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
                            _ => Err(JadeError::TypeError { op: "|".to_string(), span }),
                        },
                        BinOpKind::BitXor => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
                            _ => Err(JadeError::TypeError { op: "^".to_string(), span }),
                        },
                        BinOpKind::Shl => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b < 0 || b >= 64 {
                                    Err(JadeError::InvalidShift { amount: b, span })
                                } else {
                                    // SAFETY: guard above ensures 0 <= b < 64, fits safely in u32
                                    Ok(Value::Int(a << b as u32))
                                }
                            }
                            _ => Err(JadeError::TypeError { op: "<<".to_string(), span }),
                        },
                        BinOpKind::Shr => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b < 0 || b >= 64 {
                                    Err(JadeError::InvalidShift { amount: b, span })
                                } else {
                                    // SAFETY: guard above ensures 0 <= b < 64, fits safely in u32
                                    Ok(Value::Int(a >> b as u32))
                                }
                            }
                            _ => Err(JadeError::TypeError { op: ">>".to_string(), span }),
                        },

                        BinOpKind::Eq => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a == b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a == b)),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b)),
                            _ => Err(JadeError::TypeError { op: "==".to_string(), span }),
                        },
                        BinOpKind::Ne => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a != b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a != b)),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a != b)),
                            _ => Err(JadeError::TypeError { op: "!=".to_string(), span }),
                        },

                        BinOpKind::Lt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) < b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a < (b as f64))),
                            // bool ordering: false=0, true=1 → false < true
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool((!a) & b)),
                            _ => Err(JadeError::TypeError { op: "<".to_string(), span }),
                        },
                        BinOpKind::Gt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) > b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a > (b as f64))),
                            // bool ordering: false=0, true=1 → true > false
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a & (!b))),
                            _ => Err(JadeError::TypeError { op: ">".to_string(), span }),
                        },
                        BinOpKind::Le => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a <= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) <= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a <= (b as f64))),
                            // bool ordering: false=0, true=1 → a <= b iff a==b or a<b
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b || ((!a) & b))),
                            _ => Err(JadeError::TypeError { op: "<=".to_string(), span }),
                        },
                        BinOpKind::Ge => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a >= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) >= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a >= (b as f64))),
                            // bool ordering: false=0, true=1 → a >= b iff a==b or a>b
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b || (a & (!b)))),
                            _ => Err(JadeError::TypeError { op: ">=".to_string(), span }),
                        },

                        BinOpKind::And | BinOpKind::Or => unreachable!(),
                    }
                }
            }
        }

        Expr::UnaryOp { op, operand, span } => {
            let val = eval_expr(*operand, env)?;
            match op {
                UnaryOpKind::BitNot => match val {
                    Value::Int(i)  => Ok(Value::Int(!i)),
                    _ => Err(JadeError::TypeError { op: "~".to_string(), span }),
                },
                UnaryOpKind::Not => match val {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    _ => Err(JadeError::TypeError { op: "!".to_string(), span }),
                },
                UnaryOpKind::Neg => match val {
                    Value::Int(i)   => Ok(Value::Int(-i)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    _ => Err(JadeError::TypeError { op: "-".to_string(), span }),
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
        let tokens = lexer::tokenize(src).unwrap();
        let program = parser::parse(tokens).unwrap();
        evaluate(program)
    }

    fn eval_src_parse_err(src: &str) -> JadeError {
        let tokens = lexer::tokenize(src).unwrap();
        parser::parse(tokens).unwrap_err()
    }

    fn get(env: &Env, name: &str) -> Value {
        env.get(name).unwrap_or_else(|| panic!("variable '{}' not found", name))
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

    // ── functions — recursion ─────────────────────────────────────────────────

    #[test]
    fn test_eval_fn_factorial_0() {
        let src = "fn factorial(n) {\n    if n == 0 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f0 = factorial(0)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f0"), Value::Int(1)));
    }

    #[test]
    fn test_eval_fn_factorial_5() {
        let src = "fn factorial(n) {\n    if n == 0 {\n        return 1\n    }\n    return n * factorial(n - 1)\n}\nlet f5 = factorial(5)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "f5"), Value::Int(120)));
    }

    #[test]
    fn test_eval_fn_fib_10() {
        let src = "fn fib(n) {\n    if n <= 1 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nlet fib10 = fib(10)";
        let env = eval_src(src).unwrap();
        assert!(matches!(get(&env, "fib10"), Value::Int(55)));
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
}
