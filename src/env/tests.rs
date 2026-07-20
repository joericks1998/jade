use super::*;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string().into())
}

// Unique var names per test avoid cross-test interference (tests run in parallel).

#[test]
fn get_present_var() {
    let name = "JADE_ENV_TEST_GET_PRESENT";
    #[allow(deprecated)]
    unsafe { std::env::set_var(name, "hello") };
    let out = env_get(&[s(name)]).unwrap();
    assert!(matches!(out, VmValue::Str(ref v) if v == "hello"));
    #[allow(deprecated)]
    unsafe { std::env::remove_var(name) };
}

#[test]
fn get_absent_var_is_nil() {
    let name = "JADE_ENV_TEST_GET_ABSENT_DEFINITELY_UNSET";
    #[allow(deprecated)]
    unsafe { std::env::remove_var(name) };
    let out = env_get(&[s(name)]).unwrap();
    assert!(matches!(out, VmValue::Nil));
}

#[test]
fn set_round_trip_through_get() {
    let name = "JADE_ENV_TEST_SET_ROUNDTRIP";
    let out = env_set(&[s(name), s("world")]).unwrap();
    assert!(matches!(out, VmValue::Nil));
    let got = env_get(&[s(name)]).unwrap();
    assert!(matches!(got, VmValue::Str(ref v) if v == "world"));
    #[allow(deprecated)]
    unsafe { std::env::remove_var(name) };
}

#[test]
fn get_arity_error() {
    let err = env_get(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn get_type_error() {
    let err = env_get(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn set_arity_error() {
    let err = env_set(&[s("X")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 2, got: 1, .. }));
}

#[test]
fn set_type_error_name() {
    let err = env_set(&[VmValue::Int(1), s("v")]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn set_type_error_value() {
    let err = env_set(&[s("X"), VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn args_returns_nonempty_array() {
    let out = env_args(&[]).unwrap();
    match out {
        VmValue::Array(arc) => {
            let g = arc.lock();
            assert!(!g.is_empty(), "argv should contain at least the program name");
            assert!(matches!(g[0], VmValue::Str(_)));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

#[test]
fn args_arity_error() {
    let err = env_args(&[s("x")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}

#[test]
fn cwd_returns_nonempty_str() {
    let out = env_cwd(&[]).unwrap();
    match out {
        VmValue::Str(v) => assert!(!v.is_empty()),
        other => panic!("expected Str, got {:?}", other),
    }
}

#[test]
fn cwd_arity_error() {
    let err = env_cwd(&[s("x")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 0, got: 1, .. }));
}
