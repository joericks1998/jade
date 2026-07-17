use super::*;
use std::collections::HashMap;

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut m = HashMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    VmValue::Dict(m)
}

fn strs(v: &VmValue) -> Vec<String> {
    match v {
        VmValue::Array(arc) => arc
            .lock()
            .iter()
            .map(|e| match e {
                VmValue::Str(s) => s.clone(),
                _ => panic!("not a str"),
            })
            .collect(),
        _ => panic!("not an array"),
    }
}

// ── dict_len ──────────────────────────────────────────────────────────────────

#[test]
fn len_empty() {
    assert!(matches!(dict_len(&[dict(&[])]), Ok(VmValue::Int(0))));
}

#[test]
fn len_nonempty() {
    let d = dict(&[("a", VmValue::Int(1)), ("b", VmValue::Int(2))]);
    assert!(matches!(dict_len(&[d]), Ok(VmValue::Int(2))));
}

#[test]
fn len_wrong_type() {
    assert!(matches!(
        dict_len(&[VmValue::Int(1)]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── dict_keys (sorted) ────────────────────────────────────────────────────────

#[test]
fn keys_are_sorted() {
    let d = dict(&[("b", VmValue::Int(1)), ("a", VmValue::Int(2)), ("c", VmValue::Int(3))]);
    let keys = dict_keys(&[d]).unwrap();
    assert_eq!(strs(&keys), vec!["a", "b", "c"]);
}

#[test]
fn keys_empty() {
    let keys = dict_keys(&[dict(&[])]).unwrap();
    assert_eq!(strs(&keys), Vec::<String>::new());
}

#[test]
fn keys_wrong_type() {
    assert!(matches!(
        dict_keys(&[VmValue::Nil]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── dict_values (ordered by sorted keys) ──────────────────────────────────────

#[test]
fn values_ordered_by_key() {
    let d = dict(&[("b", VmValue::Int(20)), ("a", VmValue::Int(10))]);
    let vals = dict_values(&[d]).unwrap();
    match &vals {
        VmValue::Array(arc) => {
            let g = arc.lock();
            assert!(matches!(g[0], VmValue::Int(10))); // key "a" first
            assert!(matches!(g[1], VmValue::Int(20))); // key "b" second
        }
        _ => panic!(),
    }
}

#[test]
fn values_wrong_type() {
    assert!(matches!(
        dict_values(&[VmValue::Int(0)]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── dict_has ──────────────────────────────────────────────────────────────────

#[test]
fn has_present() {
    let d = dict(&[("x", VmValue::Int(1))]);
    assert!(matches!(
        dict_has(&[d, VmValue::Str("x".into())]),
        Ok(VmValue::Bool(true))
    ));
}

#[test]
fn has_absent() {
    let d = dict(&[("x", VmValue::Int(1))]);
    assert!(matches!(
        dict_has(&[d, VmValue::Str("y".into())]),
        Ok(VmValue::Bool(false))
    ));
}

#[test]
fn has_non_str_key() {
    let d = dict(&[("x", VmValue::Int(1))]);
    assert!(matches!(
        dict_has(&[d, VmValue::Int(1)]),
        Err(JadeError::TypeError { .. })
    ));
}

#[test]
fn has_missing_key_arg() {
    let d = dict(&[]);
    assert!(matches!(
        dict_has(&[d]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── dict_get ──────────────────────────────────────────────────────────────────

#[test]
fn get_present() {
    let d = dict(&[("k", VmValue::Int(7))]);
    assert!(matches!(
        dict_get(&[d, VmValue::Str("k".into())]),
        Ok(VmValue::Int(7))
    ));
}

#[test]
fn get_absent_returns_nil() {
    let d = dict(&[]);
    assert!(matches!(
        dict_get(&[d, VmValue::Str("missing".into())]),
        Ok(VmValue::Nil)
    ));
}

#[test]
fn get_non_str_key() {
    let d = dict(&[]);
    assert!(matches!(
        dict_get(&[d, VmValue::Float(1.0)]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── pkg_merge ─────────────────────────────────────────────────────────────────

#[test]
fn merge_combines_and_overrides() {
    let d1 = dict(&[("a", VmValue::Int(1)), ("b", VmValue::Int(2))]);
    let d2 = dict(&[("b", VmValue::Int(99)), ("c", VmValue::Int(3))]);
    let merged = pkg_merge(&[d1, d2]).unwrap();
    match merged {
        VmValue::Dict(m) => {
            assert!(matches!(m.get("a"), Some(VmValue::Int(1))));
            assert!(matches!(m.get("b"), Some(VmValue::Int(99)))); // d2 wins
            assert!(matches!(m.get("c"), Some(VmValue::Int(3))));
            assert_eq!(m.len(), 3);
        }
        _ => panic!(),
    }
}

#[test]
fn merge_wrong_types() {
    let d1 = dict(&[]);
    assert!(matches!(
        pkg_merge(&[d1, VmValue::Int(1)]),
        Err(JadeError::TypeError { .. })
    ));
}

// ── method lookup ─────────────────────────────────────────────────────────────

#[test]
fn find_method_known() {
    assert!(find_dict_method("keys").is_some());
    assert!(find_dict_method("get").is_some());
}

#[test]
fn find_method_unknown() {
    assert!(find_dict_method("nope").is_none());
}
