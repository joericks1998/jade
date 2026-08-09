use super::*;

// grammar.new(pattern, anchor?, stop_anchor?) → VmValue::Grammar

#[test]
fn new_with_pattern_only() {
    let out = (GRAMMAR_NEW.vm_impl)(&[VmValue::Str("[0-9]+".to_string().into())]).unwrap();
    match out {
        VmValue::Grammar(g) => {
            assert_eq!(g.pattern, "[0-9]+");
            assert!(g.anchor.is_none());
            assert!(g.stop.is_none());
        }
        other => panic!("expected Grammar, got {:?}", other),
    }
}

#[test]
fn new_with_anchor() {
    let out = (GRAMMAR_NEW.vm_impl)(&[
        VmValue::Str("word".to_string().into()),
        VmValue::Str("START".to_string().into()),
    ])
    .unwrap();
    match out {
        VmValue::Grammar(g) => {
            assert_eq!(g.pattern, "word");
            assert_eq!(g.anchor.as_deref(), Some("START"));
            assert!(g.stop.is_none());
        }
        other => panic!("expected Grammar, got {:?}", other),
    }
}

#[test]
fn new_with_anchor_and_stop_anchor() {
    let out = (GRAMMAR_NEW.vm_impl)(&[
        VmValue::Str("p".to_string().into()),
        VmValue::Str("A".to_string().into()),
        VmValue::Str("Z".to_string().into()),
    ])
    .unwrap();
    match out {
        VmValue::Grammar(g) => {
            assert_eq!(g.pattern, "p");
            assert_eq!(g.anchor.as_deref(), Some("A"));
            assert_eq!(g.stop.as_deref(), Some("Z"));
        }
        other => panic!("expected Grammar, got {:?}", other),
    }
}

#[test]
fn nil_anchor_is_treated_as_none() {
    let out =
        (GRAMMAR_NEW.vm_impl)(&[VmValue::Str("p".to_string().into()), VmValue::Nil, VmValue::Nil])
            .unwrap();
    match out {
        VmValue::Grammar(g) => {
            assert!(g.anchor.is_none());
            assert!(g.stop.is_none());
        }
        other => panic!("expected Grammar, got {:?}", other),
    }
}

#[test]
fn missing_pattern_is_arity_error() {
    let err = (GRAMMAR_NEW.vm_impl)(&[]).unwrap_err();
    match err {
        JadeError::ArityMismatch { expected, got, .. } => {
            assert_eq!(expected, 1);
            assert_eq!(got, 0);
        }
        other => panic!("expected ArityMismatch, got {:?}", other),
    }
}

#[test]
fn non_str_pattern_is_type_mismatch() {
    let err = (GRAMMAR_NEW.vm_impl)(&[VmValue::Int(5)]).unwrap_err();
    match err {
        JadeError::TypeMismatch { expected, .. } => assert_eq!(expected, "str"),
        other => panic!("expected TypeMismatch, got {:?}", other),
    }
}

#[test]
fn non_str_anchor_is_type_mismatch() {
    let err = (GRAMMAR_NEW.vm_impl)(&[VmValue::Str("p".to_string().into()), VmValue::Int(1)])
        .unwrap_err();
    match err {
        JadeError::TypeMismatch { expected, .. } => assert_eq!(expected, "str"),
        other => panic!("expected TypeMismatch, got {:?}", other),
    }
}

#[test]
fn non_str_stop_anchor_is_type_mismatch() {
    let err = (GRAMMAR_NEW.vm_impl)(&[
        VmValue::Str("p".to_string().into()),
        VmValue::Str("A".to_string().into()),
        VmValue::Bool(true),
    ])
    .unwrap_err();
    match err {
        JadeError::TypeMismatch { expected, .. } => assert_eq!(expected, "str"),
        other => panic!("expected TypeMismatch, got {:?}", other),
    }
}

#[test]
fn builtin_name_is_new() {
    assert_eq!(GRAMMAR_NEW.name, "new");
}
