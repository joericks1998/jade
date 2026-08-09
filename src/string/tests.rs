use super::*;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string().into())
}

fn arr_strs(v: &VmValue) -> Vec<String> {
    match v {
        VmValue::Array(arc) => arc
            .lock()
            .iter()
            .map(|e| match e {
                VmValue::Str(s) => s.to_string(),
                _ => panic!("not a str"),
            })
            .collect(),
        _ => panic!("not an array"),
    }
}

// ── str_len (char count, not byte count) ──────────────────────────────────────

#[test]
fn len_ascii() {
    assert!(matches!(str_len(&[s("hello")]), Ok(VmValue::Int(5))));
}

#[test]
fn len_empty() {
    assert!(matches!(str_len(&[s("")]), Ok(VmValue::Int(0))));
}

#[test]
fn len_counts_chars_not_bytes() {
    // "é" is 2 bytes but 1 char
    assert!(matches!(str_len(&[s("é")]), Ok(VmValue::Int(1))));
}

#[test]
fn len_wrong_type() {
    assert!(matches!(str_len(&[VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── upper / lower ─────────────────────────────────────────────────────────────

#[test]
fn upper() {
    assert!(matches!(str_upper(&[s("aB")]), Ok(VmValue::Str(x)) if x == "AB"));
}

#[test]
fn lower() {
    assert!(matches!(str_lower(&[s("aB")]), Ok(VmValue::Str(x)) if x == "ab"));
}

#[test]
fn upper_wrong_type() {
    assert!(matches!(str_upper(&[VmValue::Nil]), Err(JadeError::TypeError { .. })));
}

// ── trim ──────────────────────────────────────────────────────────────────────

#[test]
fn trim_both_sides() {
    assert!(matches!(str_trim(&[s("  hi \n")]), Ok(VmValue::Str(x)) if x == "hi"));
}

#[test]
fn trim_no_ws() {
    assert!(matches!(str_trim(&[s("hi")]), Ok(VmValue::Str(x)) if x == "hi"));
}

// ── split ─────────────────────────────────────────────────────────────────────

#[test]
fn split_basic() {
    let r = str_split(&[s("a,b,c"), s(",")]).unwrap();
    assert_eq!(arr_strs(&r), vec!["a", "b", "c"]);
}

#[test]
fn split_no_match_returns_whole() {
    let r = str_split(&[s("abc"), s(",")]).unwrap();
    assert_eq!(arr_strs(&r), vec!["abc"]);
}

#[test]
fn split_missing_sep() {
    assert!(matches!(str_split(&[s("abc")]), Err(JadeError::TypeError { .. })));
}

#[test]
fn split_non_str_sep() {
    assert!(matches!(str_split(&[s("abc"), VmValue::Int(1)]), Err(JadeError::TypeError { .. })));
}

// ── contains ──────────────────────────────────────────────────────────────────

#[test]
fn contains_true() {
    assert!(matches!(str_contains(&[s("hello"), s("ell")]), Ok(VmValue::Bool(true))));
}

#[test]
fn contains_false() {
    assert!(matches!(str_contains(&[s("hello"), s("xyz")]), Ok(VmValue::Bool(false))));
}

#[test]
fn contains_missing_arg() {
    assert!(matches!(str_contains(&[s("hi")]), Err(JadeError::TypeError { .. })));
}

// ── replace ───────────────────────────────────────────────────────────────────

#[test]
fn replace_all_occurrences() {
    assert!(matches!(
        str_replace(&[s("a-a-a"), s("a"), s("b")]),
        Ok(VmValue::Str(x)) if x == "b-b-b"
    ));
}

#[test]
fn replace_no_match() {
    assert!(matches!(
        str_replace(&[s("abc"), s("z"), s("y")]),
        Ok(VmValue::Str(x)) if x == "abc"
    ));
}

#[test]
fn replace_missing_third_arg() {
    assert!(matches!(str_replace(&[s("abc"), s("a")]), Err(JadeError::TypeError { .. })));
}

// ── starts_with / ends_with ───────────────────────────────────────────────────

#[test]
fn starts_with_true() {
    assert!(matches!(str_starts_with(&[s("hello"), s("he")]), Ok(VmValue::Bool(true))));
}

#[test]
fn starts_with_false() {
    assert!(matches!(str_starts_with(&[s("hello"), s("lo")]), Ok(VmValue::Bool(false))));
}

#[test]
fn ends_with_true() {
    assert!(matches!(str_ends_with(&[s("hello"), s("lo")]), Ok(VmValue::Bool(true))));
}

#[test]
fn ends_with_false() {
    assert!(matches!(str_ends_with(&[s("hello"), s("he")]), Ok(VmValue::Bool(false))));
}

// ── package fns mirror primitive methods ──────────────────────────────────────

#[test]
fn pkg_upper_matches() {
    assert!(matches!(pkg_upper(&[s("x")]), Ok(VmValue::Str(v)) if v == "X"));
}

#[test]
fn pkg_split_matches() {
    let r = pkg_split(&[s("1 2"), s(" ")]).unwrap();
    assert_eq!(arr_strs(&r), vec!["1", "2"]);
}

#[test]
fn pkg_replace_matches() {
    assert!(matches!(
        pkg_replace(&[s("aa"), s("a"), s("z")]),
        Ok(VmValue::Str(v)) if v == "zz"
    ));
}

#[test]
fn pkg_contains_wrong_type() {
    assert!(matches!(pkg_contains(&[VmValue::Int(1), s("a")]), Err(JadeError::TypeError { .. })));
}

// ── method lookup ─────────────────────────────────────────────────────────────

#[test]
fn find_method_known() {
    assert!(find_str_method("upper").is_some());
    assert!(find_str_method("ends_with").is_some());
}

#[test]
fn find_method_unknown() {
    assert!(find_str_method("nope").is_none());
}
