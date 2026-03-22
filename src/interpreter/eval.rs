use std::collections::HashMap;

use super::{
    ast::{BinOpKind, Expr, Program, Stmt, UnaryOpKind},
    error::{JadeError, Result},
};

/// Holds all declared variables and their current values.
pub struct Env {
    vars: HashMap<String, i64>,
}

impl Env {
    /// Create a new, empty environment.
    pub fn new() -> Self {
        Env { vars: HashMap::new() }
    }

    /// Look up a variable by name.
    pub fn get(&self, name: &str) -> Option<i64> {
        self.vars.get(name).copied()
    }

    /// Bind a name to a value. Overwrites if already present.
    pub fn set(&mut self, name: String, value: i64) {
        self.vars.insert(name, value);
    }

    /// Iterator over all (name, value) pairs, for printing.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &i64)> {
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

/// Evaluate one expression against the current environment, returning its value.
fn eval_expr(expr: Expr, env: &Env) -> Result<i64> {
    match expr {
        Expr::Integer { value, .. } => Ok(value),

        Expr::Identifier { name, span } => {
            env.get(&name).ok_or(JadeError::UndefinedVariable { name, span })
        }

        Expr::BinOp { op, left, right, span } => {
            let l = eval_expr(*left, env)?;
            let r = eval_expr(*right, env)?;
            match op {
                BinOpKind::Add => Ok(l + r),
                BinOpKind::Sub => Ok(l - r),
                BinOpKind::Mul => Ok(l * r),
                BinOpKind::Div => {
                    if r == 0 { Err(JadeError::DivisionByZero { span }) } else { Ok(l / r) }
                }
                BinOpKind::Mod => {
                    if r == 0 { Err(JadeError::RemainderByZero { span }) } else { Ok(l % r) }
                }
                BinOpKind::BitAnd => Ok(l & r),
                BinOpKind::BitOr  => Ok(l | r),
                BinOpKind::BitXor => Ok(l ^ r),
                BinOpKind::Shl => {
                    if r < 0 || r >= 64 {
                        Err(JadeError::InvalidShift { amount: r, span })
                    } else {
                        Ok(l << r as u32)
                    }
                }
                BinOpKind::Shr => {
                    if r < 0 || r >= 64 {
                        Err(JadeError::InvalidShift { amount: r, span })
                    } else {
                        Ok(l >> r as u32)
                    }
                }
            }
        }

        Expr::UnaryOp { op, operand, .. } => {
            let val = eval_expr(*operand, env)?;
            match op {
                UnaryOpKind::BitNot => Ok(!val),
            }
        }
    }
}
