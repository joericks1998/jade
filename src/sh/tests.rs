use super::*;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string())
}

// ---- exec ----

#[test]
fn exec_echo_captures_stdout_trimmed() {
    let out = sh_exec(&[s("echo hello")]).unwrap();
    // trailing newline is stripped
    assert!(matches!(out, VmValue::Str(ref v) if v == "hello"));
}

#[test]
fn exec_nonzero_exit_errors() {
    let err = sh_exec(&[s("exit 3")]).unwrap_err();
    match err {
        JadeError::IoError { message, .. } => assert!(message.contains("code 3")),
        other => panic!("expected IoError, got {:?}", other),
    }
}

#[test]
fn exec_type_error() {
    let err = sh_exec(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn exec_arity_error() {
    let err = sh_exec(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

// ---- run ----

#[test]
fn run_returns_zero_on_success() {
    let out = sh_run(&[s("true")]).unwrap();
    assert!(matches!(out, VmValue::Int(0)));
}

#[test]
fn run_returns_exit_code() {
    let out = sh_run(&[s("exit 5")]).unwrap();
    assert!(matches!(out, VmValue::Int(5)));
}

#[test]
fn run_type_error() {
    let err = sh_run(&[VmValue::Bool(true)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn run_arity_error() {
    let err = sh_run(&[s("a"), s("b")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

// ---- output ----

#[test]
fn output_dict_has_stdout_stderr_code() {
    let out = sh_output(&[s("echo hi")]).unwrap();
    match out {
        VmValue::Dict(map) => {
            assert!(matches!(map.get("stdout"), Some(VmValue::Str(v)) if v == "hi\n"));
            assert!(matches!(map.get("stderr"), Some(VmValue::Str(v)) if v.is_empty()));
            assert!(matches!(map.get("code"), Some(VmValue::Int(0))));
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}

#[test]
fn output_captures_stderr_and_code() {
    let out = sh_output(&[s("echo oops 1>&2; exit 2")]).unwrap();
    match out {
        VmValue::Dict(map) => {
            assert!(matches!(map.get("stdout"), Some(VmValue::Str(v)) if v.is_empty()));
            assert!(matches!(map.get("stderr"), Some(VmValue::Str(v)) if v.contains("oops")));
            assert!(matches!(map.get("code"), Some(VmValue::Int(2))));
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}

#[test]
fn output_type_error() {
    let err = sh_output(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn output_arity_error() {
    let err = sh_output(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}
