use std::collections::HashMap;

use super::{
    ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result},
};

/// A runtime value in Jade.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
}

/// Holds all declared variables and their current values.
pub struct Env {
    vars: HashMap<String, Value>,
}

impl Env {
    /// Create a new, empty environment.
    pub fn new() -> Self {
        Env { vars: HashMap::new() }
    }

    /// Look up a variable by name.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
    }

    /// Bind a name to a value. Overwrites if already present.
    pub fn set(&mut self, name: String, value: Value) {
        self.vars.insert(name, value);
    }

    /// Iterator over all (name, value) pairs, for printing.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.vars.iter()
    }
}

/// Public entry point. Walks the program and returns the populated environment.
pub fn evaluate(program: Program) -> Result<Env> {
    let mut env = Env::new();
    for stmt in program.stmts {
        eval_stmt(stmt, &mut env)?;
    }
    Ok(env)
}

/// Evaluate one statement, mutating the environment.
fn eval_stmt(stmt: Stmt, env: &mut Env) -> Result<()> {
    match stmt {
        Stmt::Let { name, value, .. } => {
            let result = eval_expr(value, env)?;
            env.set(name, result);
            Ok(())
        }
    }
}

/// Widen an integer to float for mixed-type arithmetic.
fn to_float(v: Value) -> f64 {
    match v {
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
    }
}

/// Evaluate one expression against the current environment, returning its value.
fn eval_expr(expr: Expr, env: &Env) -> Result<Value> {
    match expr {
        Expr::Integer { value, .. } => Ok(Value::Int(value)),
        Expr::Float { value, .. } => Ok(Value::Float(value)),

        Expr::Identifier { name, span } => {
            env.get(&name).ok_or(JadeError::UndefinedVariable { name, span })
        }

        Expr::BinOp { op, left, right, span } => {
            let l = eval_expr(*left, env)?;
            let r = eval_expr(*right, env)?;
            match op {
                // Arithmetic: int+int stays int; any float operand promotes to float
                BinOpKind::Add => match (l, r) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                    (a, b) => Ok(Value::Float(to_float(a) + to_float(b))),
                },
                BinOpKind::Sub => match (l, r) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                    (a, b) => Ok(Value::Float(to_float(a) - to_float(b))),
                },
                BinOpKind::Mul => match (l, r) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                    (a, b) => Ok(Value::Float(to_float(a) * to_float(b))),
                },
                BinOpKind::Div => match (l, r) {
                    (Value::Int(a), Value::Int(b)) => {
                        if b == 0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Int(a / b)) }
                    }
                    (a, b) => {
                        let bf = to_float(b);
                        if bf == 0.0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Float(to_float(a) / bf)) }
                    }
                },
                BinOpKind::Mod => match (l, r) {
                    (Value::Int(a), Value::Int(b)) => {
                        if b == 0 { Err(JadeError::RemainderByZero { span }) } else { Ok(Value::Int(a % b)) }
                    }
                    (a, b) => {
                        let bf = to_float(b);
                        if bf == 0.0 { Err(JadeError::RemainderByZero { span }) } else { Ok(Value::Float(to_float(a) % bf)) }
                    }
                },

                // Bitwise: integers only
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
                            Ok(Value::Int(a >> b as u32))
                        }
                    }
                    _ => Err(JadeError::TypeError { op: ">>".to_string(), span }),
                },
            }
        }

        Expr::UnaryOp { op, operand, span } => {
            let val = eval_expr(*operand, env)?;
            match op {
                UnaryOpKind::BitNot => match val {
                    Value::Int(i) => Ok(Value::Int(!i)),
                    Value::Float(_) => Err(JadeError::TypeError { op: "~".to_string(), span }),
                },
            }
        }
    }
}
