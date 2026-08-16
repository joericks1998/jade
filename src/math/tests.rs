use super::*;

// ── floor ─────────────────────────────────────────────────────────────────────

#[test]
fn floor_float() {
    assert!(matches!(math_floor(&[VmValue::Float(3.7)]), Ok(VmValue::Int(3))));
}

#[test]
fn floor_negative_float() {
    assert!(matches!(math_floor(&[VmValue::Float(-1.2)]), Ok(VmValue::Int(-2))));
}

#[test]
fn floor_int_passthrough() {
    assert!(matches!(math_floor(&[VmValue::Int(5)]), Ok(VmValue::Int(5))));
}

#[test]
fn floor_wrong_type() {
    assert!(matches!(math_floor(&[VmValue::Nil]), Err(JadeError::TypeError { .. })));
}

// ── ceil ──────────────────────────────────────────────────────────────────────

#[test]
fn ceil_float() {
    assert!(matches!(math_ceil(&[VmValue::Float(3.1)]), Ok(VmValue::Int(4))));
}

#[test]
fn ceil_negative_float() {
    assert!(matches!(math_ceil(&[VmValue::Float(-1.9)]), Ok(VmValue::Int(-1))));
}

#[test]
fn ceil_int_passthrough() {
    assert!(matches!(math_ceil(&[VmValue::Int(2)]), Ok(VmValue::Int(2))));
}

// ── abs ───────────────────────────────────────────────────────────────────────

#[test]
fn abs_int() {
    assert!(matches!(math_abs(&[VmValue::Int(-9)]), Ok(VmValue::Int(9))));
}

#[test]
fn abs_float() {
    assert!(matches!(math_abs(&[VmValue::Float(-2.5)]), Ok(VmValue::Float(f)) if f == 2.5));
}

#[test]
fn abs_wrong_type() {
    assert!(matches!(math_abs(&[VmValue::Str("x".into())]), Err(JadeError::TypeError { .. })));
}

// ── sqrt (always float) ───────────────────────────────────────────────────────

#[test]
fn sqrt_float() {
    assert!(matches!(math_sqrt(&[VmValue::Float(9.0)]), Ok(VmValue::Float(f)) if f == 3.0));
}

#[test]
fn sqrt_int_promotes_to_float() {
    assert!(matches!(math_sqrt(&[VmValue::Int(16)]), Ok(VmValue::Float(f)) if f == 4.0));
}

#[test]
fn sqrt_wrong_type() {
    assert!(matches!(math_sqrt(&[VmValue::Bool(true)]), Err(JadeError::TypeError { .. })));
}

// ── min / max ─────────────────────────────────────────────────────────────────

#[test]
fn min_ints() {
    assert!(matches!(math_min(&[VmValue::Int(3), VmValue::Int(1)]), Ok(VmValue::Int(1))));
}

#[test]
fn max_ints() {
    assert!(matches!(math_max(&[VmValue::Int(3), VmValue::Int(1)]), Ok(VmValue::Int(3))));
}

#[test]
fn min_mixed_promotes_to_float() {
    assert!(matches!(
        math_min(&[VmValue::Int(2), VmValue::Float(1.5)]),
        Ok(VmValue::Float(f)) if f == 1.5
    ));
}

#[test]
fn max_mixed_float_int() {
    assert!(matches!(
        math_max(&[VmValue::Float(2.5), VmValue::Int(3)]),
        Ok(VmValue::Float(f)) if f == 3.0
    ));
}

#[test]
fn min_missing_second_arg() {
    assert!(matches!(math_min(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

#[test]
fn max_wrong_type() {
    assert!(matches!(
        math_max(&[VmValue::Str("a".into()), VmValue::Str("b".into())]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── pow ───────────────────────────────────────────────────────────────────────

#[test]
fn pow_int_positive_exp() {
    assert!(matches!(math_pow(&[VmValue::Int(2), VmValue::Int(10)]), Ok(VmValue::Int(1024))));
}

#[test]
fn pow_int_negative_exp_is_float() {
    assert!(matches!(
        math_pow(&[VmValue::Int(2), VmValue::Int(-1)]),
        Ok(VmValue::Float(f)) if f == 0.5
    ));
}

#[test]
fn pow_float_base() {
    assert!(matches!(
        math_pow(&[VmValue::Float(2.0), VmValue::Float(3.0)]),
        Ok(VmValue::Float(f)) if f == 8.0
    ));
}

#[test]
fn pow_int_base_float_exp() {
    assert!(matches!(
        math_pow(&[VmValue::Int(9), VmValue::Float(0.5)]),
        Ok(VmValue::Float(f)) if f == 3.0
    ));
}

#[test]
fn pow_float_base_int_exp() {
    assert!(matches!(
        math_pow(&[VmValue::Float(2.0), VmValue::Int(3)]),
        Ok(VmValue::Float(f)) if f == 8.0
    ));
}

#[test]
fn pow_missing_arg() {
    assert!(matches!(math_pow(&[VmValue::Int(2)]), Err(JadeError::TypeError { .. })));
}

// ── v1.3.23 additions ─────────────────────────────────────────────────────────

#[test]
fn round_and_sign_marshal_the_kind_back() {
    assert!(matches!(math_round(&[VmValue::Float(2.5)]), Ok(VmValue::Int(3))));
    assert!(matches!(math_round(&[VmValue::Int(7)]), Ok(VmValue::Int(7))));
    assert!(matches!(math_sign(&[VmValue::Int(-7)]), Ok(VmValue::Int(-1))));
    assert!(matches!(math_sign(&[VmValue::Float(-0.5)]), Ok(VmValue::Float(f)) if f == -1.0));
}

#[test]
fn clamp_takes_three() {
    assert!(matches!(
        math_clamp(&[VmValue::Int(15), VmValue::Int(0), VmValue::Int(10)]),
        Ok(VmValue::Int(10))
    ));
    assert!(matches!(math_clamp(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

#[test]
fn the_predicates_answer_a_bool() {
    assert!(matches!(math_is_nan(&[VmValue::Float(f64::NAN)]), Ok(VmValue::Bool(true))));
    assert!(matches!(math_is_nan(&[VmValue::Int(1)]), Ok(VmValue::Bool(false))));
    assert!(matches!(math_is_inf(&[VmValue::Float(f64::INFINITY)]), Ok(VmValue::Bool(true))));
}

/// A constant is spelled as a call, so it takes no arguments — and says so
/// rather than quietly ignoring one.
#[test]
fn a_constant_refuses_an_argument() {
    assert!(matches!(math_pi(&[]), Ok(VmValue::Float(_))));
    assert!(matches!(math_pi(&[VmValue::Int(1)]), Err(JadeError::ArityMismatch { .. })));
    assert!(matches!(math_inf(&[]), Ok(VmValue::Float(f)) if f.is_infinite()));
    assert!(matches!(math_nan(&[]), Ok(VmValue::Float(f)) if f.is_nan()));
}
