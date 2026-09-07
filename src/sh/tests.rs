use super::*;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string().into())
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

use jade_runtime::trust::JStr;

// ── trust model ──────────────────────────────────────────────────────────────
//
// Compiled code has refused tainted strings at code-execution sinks from the
// start. The interpreter tracked no trust at all, so the same program ran an
// untrusted command under `jade run` and was refused under `jade build`.

#[test]
fn exec_refuses_a_tainted_command() {
    let err = sh_exec(&[VmValue::Str(JStr::tainted("echo hi"))])
        .expect_err("a tainted command must be refused");
    let msg = err.to_string();
    assert!(msg.contains("refused tainted string in sh.exec(cmd)"), "got: {msg}");
    assert!(msg.contains("code-execution sink"), "got: {msg}");
}

#[test]
fn run_refuses_a_tainted_command() {
    let err = sh_run(&[VmValue::Str(JStr::tainted("true"))])
        .expect_err("a tainted command must be refused");
    assert!(err.to_string().contains("sh.run(cmd)"));
}

// `output` was the hole until v1.3.3. All three functions reach the same
// `sh -c`, so refusing two of them and not the third did not narrow what an
// untrusted command could do — it only decided how it had to be spelled.
#[test]
fn output_refuses_a_tainted_command() {
    let err = sh_output(&[VmValue::Str(JStr::tainted("echo hi"))])
        .expect_err("a tainted command must be refused");
    let msg = err.to_string();
    assert!(msg.contains("refused tainted string in sh.output(cmd)"), "got: {msg}");
    assert!(msg.contains("code-execution sink"), "got: {msg}");
}

#[test]
fn output_accepts_a_trusted_command() {
    let out = sh_output(&[VmValue::Str(JStr::trusted("echo hi"))])
        .expect("a command built from source is allowed");
    let VmValue::Dict(d) = out else { panic!("expected a dict") };
    assert!(matches!(d.get("stdout"), Some(VmValue::Str(s)) if s.as_str().trim() == "hi"));
}

#[test]
fn exec_accepts_a_trusted_command() {
    match sh_exec(&[VmValue::Str(JStr::trusted("echo hi"))])
        .expect("a command built from source is allowed")
    {
        VmValue::Str(s) => assert_eq!(s.as_str(), "hi"),
        other => panic!("expected Str, got {other:?}"),
    }
}

// The output of a shell command came from outside the program, so it is
// tainted — which is what makes feeding it back in a refusal rather than a
// silent re-execution.
#[test]
fn exec_output_is_tainted() {
    match sh_exec(&[VmValue::Str(JStr::trusted("echo hi"))]).unwrap() {
        VmValue::Str(s) => assert!(s.is_tainted(), "sh.exec output must be tainted"),
        other => panic!("expected Str, got {other:?}"),
    }
}

/// `sh.output`'s streams are command output, so they carry the same taint
/// `sh.exec`'s return does. They were built trusted, which made
/// `sh.exec(sh.output(x).stdout)` the way around a refusal that stopped
/// `sh.exec(sh.exec(x))` — accepted interpreted, refused compiled.
#[test]
fn output_streams_are_tainted() {
    let out = sh_output(&[VmValue::Str(JStr::trusted("echo hi"))]).unwrap();
    let VmValue::Dict(d) = out else { panic!("expected a dict") };
    for field in ["stdout", "stderr"] {
        match d.get(field).expect("field") {
            VmValue::Str(s) => assert!(s.is_tainted(), "sh.output {field} must be tainted"),
            other => panic!("expected Str for {field}, got {other:?}"),
        }
    }
}
