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
    assert!(matches!(native_len(&[VmValue::dict(m)]), Ok(VmValue::Int(1))));
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
    assert_eq!(CANCELLED.name, "cancelled");
    assert_eq!(MAX_TASKS.name, "max_tasks");
    assert_eq!(SET_MAX_TASKS.name, "set_max_tasks");
}

// ── max_tasks / set_max_tasks ────────────────────────────────────────────────
//
// The limit is one process-wide number, so anything that writes it runs under
// this lock and puts it back. `cargo test` is parallel and the value is not
// per-VM.
static LIMIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_limit_restored<T>(f: impl FnOnce() -> T) -> T {
    let _g = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = jade_runtime::task::max_tasks();
    let out = f();
    jade_runtime::task::set_max_tasks(prev);
    out
}

#[test]
fn max_tasks_reads_the_live_value() {
    with_limit_restored(|| {
        jade_runtime::task::set_max_tasks(11);
        assert!(matches!(native_max_tasks(&[]), Ok(VmValue::Int(11))));
    });
}

#[test]
fn max_tasks_takes_no_arguments() {
    assert!(matches!(
        native_max_tasks(&[VmValue::Int(1)]),
        Err(JadeError::ArityMismatch { expected: 0, got: 1, .. })
    ));
}

/// Clamped rather than refused, and the answer is what took effect — that is
/// what lets a caller see it got 512 instead of the 9999 it asked for.
#[test]
fn set_max_tasks_answers_with_the_clamped_value() {
    with_limit_restored(|| {
        assert!(matches!(native_set_max_tasks(&[VmValue::Int(9)]), Ok(VmValue::Int(9))));
        assert!(matches!(native_set_max_tasks(&[VmValue::Int(0)]), Ok(VmValue::Int(1))));
        assert!(matches!(native_set_max_tasks(&[VmValue::Int(-3)]), Ok(VmValue::Int(1))));
        assert!(matches!(native_set_max_tasks(&[VmValue::Int(9999)]), Ok(VmValue::Int(512))));
    });
}

#[test]
fn set_max_tasks_arity_and_type() {
    assert!(matches!(
        native_set_max_tasks(&[]),
        Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })
    ));
    assert!(matches!(
        native_set_max_tasks(&[VmValue::Int(1), VmValue::Int(2)]),
        Err(JadeError::ArityMismatch { expected: 1, got: 2, .. })
    ));
    // A float is the near miss worth naming: `set_max_tasks(2.5)` has no
    // sensible reading, so it says so rather than truncating.
    match native_set_max_tasks(&[VmValue::Float(2.5)]) {
        Err(JadeError::TypeError { message, .. }) => {
            assert!(message.contains("expects an int"), "unhelpful message: {message}");
        }
        other => panic!("expected a type error, got {other:?}"),
    }
}

#[test]
fn len_const_dispatches() {
    let a = make_array(vec![VmValue::Int(1)]);
    assert!(matches!((LEN.vm_impl)(&[a]), Ok(VmValue::Int(1))));
}
