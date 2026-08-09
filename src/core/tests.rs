use super::*;
use crate::builtins::make_array;
use jade_runtime::coll::DictObj;

// native_write / native_len / native_input are private; reachable via `use super::*`.

// ── write ─────────────────────────────────────────────────────────────────────

#[test]
fn write_returns_nil() {
    assert!(matches!(native_write(&[VmValue::Str("x".into())]), Ok(VmValue::Nil)));
}

#[test]
fn write_arity() {
    assert!(matches!(
        native_write(&[VmValue::Int(1), VmValue::Int(2)]),
        Err(JadeError::ArityMismatch { expected: 1, got: 2, .. })
    ));
    assert!(matches!(native_write(&[]), Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })));
}

// ── len (str / array / dict) ──────────────────────────────────────────────────

#[test]
fn len_str_char_count() {
    assert!(matches!(native_len(&[VmValue::Str("héllo".into())]), Ok(VmValue::Int(5))));
}

#[test]
fn len_array() {
    let a = make_array(vec![VmValue::Int(1), VmValue::Int(2)]);
    assert!(matches!(native_len(&[a]), Ok(VmValue::Int(2))));
}

#[test]
fn len_dict() {
    let mut m = DictObj::new();
    m.insert("a".to_string(), VmValue::Int(1));
    assert!(matches!(native_len(&[VmValue::Dict(m)]), Ok(VmValue::Int(1))));
}

#[test]
fn len_wrong_type() {
    assert!(matches!(native_len(&[VmValue::Int(5)]), Err(JadeError::TypeError { .. })));
}

#[test]
fn len_arity() {
    assert!(matches!(native_len(&[]), Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })));
    assert!(matches!(
        native_len(&[VmValue::Str("a".into()), VmValue::Str("b".into())]),
        Err(JadeError::ArityMismatch { expected: 1, got: 2, .. })
    ));
}

// ── input (only exercise the pure validation paths; do not read stdin) ────────

#[test]
fn input_too_many_args() {
    assert!(matches!(
        native_input(&[VmValue::Str("a".into()), VmValue::Str("b".into())]),
        Err(JadeError::ArityMismatch { expected: 1, got: 2, .. })
    ));
}

#[test]
fn input_non_str_prompt() {
    // A single non-str arg hits the TypeError branch before any stdin read.
    assert!(matches!(native_input(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── BuiltinFn constants wired correctly ───────────────────────────────────────

#[test]
fn builtin_names() {
    assert_eq!(WRITE.name, "write");
    assert_eq!(LEN.name, "len");
    assert_eq!(INPUT.name, "input");
}

#[test]
fn len_const_dispatches() {
    let a = make_array(vec![VmValue::Int(1)]);
    assert!(matches!((LEN.vm_impl)(&[a]), Ok(VmValue::Int(1))));
}
