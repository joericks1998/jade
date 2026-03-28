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
    Bool(bool),
}

/// Holds all declared variables and their current values.
#[derive(Debug)]
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
/// Panics on Bool — arithmetic ops must reject Bool before calling this.
fn to_float(v: Value) -> f64 {
    match v {
        Value::Int(i)   => i as f64,
        Value::Float(f) => f,
        Value::Bool(_)  => unreachable!("to_float called on Bool"),
    }
}

/// Evaluate one expression against the current environment, returning its value.
fn eval_expr(expr: Expr, env: &Env) -> Result<Value> {
    match expr {
        Expr::Integer { value, .. } => Ok(Value::Int(value)),
        Expr::Float   { value, .. } => Ok(Value::Float(value)),
        Expr::Bool    { value, .. } => Ok(Value::Bool(value)),

        Expr::Identifier { name, span } => {
            env.get(&name).ok_or(JadeError::UndefinedVariable { name, span })
        }

        Expr::BinOp { op, left, right, span } => {
            match op {
                // Short-circuit logical ops — evaluate left first, skip right when possible
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
                        // Arithmetic: int+int stays int; any float promotes; Bool is a TypeError
                        BinOpKind::Add => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Int(a + b)),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) => Err(JadeError::TypeError { op: "+".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) + to_float(b))),
                        },
                        BinOpKind::Sub => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Int(a - b)),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) => Err(JadeError::TypeError { op: "-".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) - to_float(b))),
                        },
                        BinOpKind::Mul => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Int(a * b)),
                            (Value::Bool(_), _) | (_, Value::Bool(_)) => Err(JadeError::TypeError { op: "*".to_string(), span }),
                            (a, b) => Ok(Value::Float(to_float(a) * to_float(b))),
                        },
                        BinOpKind::Div => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Int(a / b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) => Err(JadeError::TypeError { op: "/".to_string(), span }),
                            (a, b) => {
                                let bf = to_float(b);
                                if bf == 0.0 { Err(JadeError::DivisionByZero { span }) } else { Ok(Value::Float(to_float(a) / bf)) }
                            }
                        },
                        BinOpKind::Mod => match (l, r) {
                            (Value::Int(a), Value::Int(b)) => {
                                if b == 0 { Err(JadeError::RemainderByZero { span }) } else { Ok(Value::Int(a % b)) }
                            }
                            (Value::Bool(_), _) | (_, Value::Bool(_)) => Err(JadeError::TypeError { op: "%".to_string(), span }),
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

                        // Equality: strict same-type; no int/float promotion
                        BinOpKind::Eq => match (l, r) {
                            (Value::Int(a),  Value::Int(b))  => Ok(Value::Bool(a == b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a == b)),
                            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a == b)),
                            _ => Err(JadeError::TypeError { op: "==".to_string(), span }),
                        },
                        BinOpKind::Ne => match (l, r) {
                            (Value::Int(a),  Value::Int(b))  => Ok(Value::Bool(a != b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a != b)),
                            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a != b)),
                            _ => Err(JadeError::TypeError { op: "!=".to_string(), span }),
                        },

                        // Ordering: int/float may mix (promote); bool/bool allowed; mixed bool+num is TypeError
                        BinOpKind::Lt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a < b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) < b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a < (b as f64))),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool((!a) & b)),
                            _ => Err(JadeError::TypeError { op: "<".to_string(), span }),
                        },
                        BinOpKind::Gt => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a > b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) > b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a > (b as f64))),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a & (!b))),
                            _ => Err(JadeError::TypeError { op: ">".to_string(), span }),
                        },
                        BinOpKind::Le => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a <= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) <= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a <= (b as f64))),
                            (Value::Bool(a),  Value::Bool(b))  => Ok(Value::Bool(a == b || ((!a) & b))),
                            _ => Err(JadeError::TypeError { op: "<=".to_string(), span }),
                        },
                        BinOpKind::Ge => match (l, r) {
                            (Value::Int(a),   Value::Int(b))   => Ok(Value::Bool(a >= b)),
                            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
                            (Value::Int(a),   Value::Float(b)) => Ok(Value::Bool((a as f64) >= b)),
                            (Value::Float(a), Value::Int(b))   => Ok(Value::Bool(a >= (b as f64))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{error::JadeError, lexer, parser};

    fn eval_src(src: &str) -> Result<Env> {
        let tokens = lexer::tokenize(src).unwrap();
        let program = parser::parse(tokens).unwrap();
        evaluate(program)
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
        // false && undefined_var should return false, not UndefinedVariable
        let env = eval_src("let x = false && undefined_var").unwrap();
        assert!(matches!(get(&env, "x"), Value::Bool(false)));
    }

    #[test]
    fn test_eval_short_circuit_or_skips_rhs() {
        // true || undefined_var should return true, not UndefinedVariable
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
        // int == float is a TypeError (strict same-type equality)
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
}
