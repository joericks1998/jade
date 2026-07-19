use super::*;

fn get_int(v: VmValue) -> i64 {
    match v {
        VmValue::Int(n) => n,
        other => panic!("expected Int, got {:?}", other),
    }
}

#[test]
fn now_returns_positive_int() {
    let n = get_int(time_now(&[]).unwrap());
    // any time after 2020-01-01 (1577836800) proves the clock is sane
    assert!(n > 1_577_836_800, "unix seconds too small: {}", n);
}

#[test]
fn now_ms_returns_positive_int() {
    let n = get_int(time_now_ms(&[]).unwrap());
    assert!(n > 1_577_836_800_000, "unix millis too small: {}", n);
}

#[test]
fn now_ms_is_larger_than_now_seconds() {
    // millis value is ~1000x the seconds value; strictly greater in magnitude
    let secs = get_int(time_now(&[]).unwrap());
    let ms = get_int(time_now_ms(&[]).unwrap());
    assert!(ms > secs);
}

#[test]
fn now_is_non_decreasing() {
    let a = get_int(time_now_ms(&[]).unwrap());
    let b = get_int(time_now_ms(&[]).unwrap());
    assert!(b >= a, "time went backwards: {} then {}", a, b);
}

#[test]
fn now_arity_error() {
    let err = time_now(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn now_ms_arity_error() {
    let err = time_now_ms(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn sleep_zero_returns_nil() {
    // non-positive duration is a no-op; must return nil without blocking
    assert!(matches!(time_sleep(&[VmValue::Int(0)]).unwrap(), VmValue::Nil));
    assert!(matches!(time_sleep(&[VmValue::Float(-1.0)]).unwrap(), VmValue::Nil));
}

#[test]
fn sleep_tiny_duration_returns_nil() {
    // a millisecond-scale sleep just to exercise the positive branch
    let out = time_sleep(&[VmValue::Float(0.001)]).unwrap();
    assert!(matches!(out, VmValue::Nil));
}

#[test]
fn sleep_type_error() {
    let err = time_sleep(&[VmValue::Str("x".to_string())]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn sleep_arity_error() {
    let err = time_sleep(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn local_type_error_on_non_str() {
    let err = time_local(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn local_arity_error() {
    let err = time_local(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn local_nil_tz_returns_nonempty_str() {
    // nil tz uses the system default; `date` should produce a non-empty line.
    match time_local(&[VmValue::Nil]).unwrap() {
        VmValue::Str(s) => assert!(!s.is_empty(), "date output was empty"),
        other => panic!("expected Str, got {:?}", other),
    }
}
