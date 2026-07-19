use super::*;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string())
}

fn as_str(v: VmValue) -> String {
    match v {
        VmValue::Str(s) => s,
        other => panic!("expected Str, got {:?}", other),
    }
}

// ---- join ----

#[test]
fn join_two_segments() {
    let out = path_join(&[s("a"), s("b")]).unwrap();
    assert_eq!(as_str(out), "a/b");
}

#[test]
fn join_variadic() {
    let out = path_join(&[s("a"), s("b"), s("c"), s("d")]).unwrap();
    assert_eq!(as_str(out), "a/b/c/d");
}

#[test]
fn join_absolute_part_resets() {
    // pushing an absolute path replaces the accumulated buffer
    let out = path_join(&[s("a"), s("/b")]).unwrap();
    assert_eq!(as_str(out), "/b");
}

#[test]
fn join_arity_error_needs_two() {
    let err = path_join(&[s("a")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 2, got: 1, .. }));
}

#[test]
fn join_type_error_on_non_str() {
    let err = path_join(&[s("a"), VmValue::Int(3)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

// ---- basename ----

#[test]
fn basename_file_with_ext() {
    assert_eq!(as_str(path_basename(&[s("/foo/bar.rs")]).unwrap()), "bar.rs");
}

#[test]
fn basename_no_dir() {
    assert_eq!(as_str(path_basename(&[s("bar.rs")]).unwrap()), "bar.rs");
}

#[test]
fn basename_trailing_slash() {
    assert_eq!(as_str(path_basename(&[s("/foo/bar/")]).unwrap()), "bar");
}

#[test]
fn basename_arity_error() {
    let err = path_basename(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 0, .. }));
}

#[test]
fn basename_type_error() {
    let err = path_basename(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

// ---- dirname ----

#[test]
fn dirname_normal() {
    assert_eq!(as_str(path_dirname(&[s("/foo/bar.rs")]).unwrap()), "/foo");
}

#[test]
fn dirname_bare_filename_is_dot() {
    assert_eq!(as_str(path_dirname(&[s("bar.rs")]).unwrap()), ".");
}

#[test]
fn dirname_nested() {
    assert_eq!(as_str(path_dirname(&[s("a/b/c")]).unwrap()), "a/b");
}

#[test]
fn dirname_arity_error() {
    let err = path_dirname(&[s("a"), s("b")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

// ---- ext ----

#[test]
fn ext_with_extension() {
    assert_eq!(as_str(path_ext(&[s("foo/bar.rs")]).unwrap()), ".rs");
}

#[test]
fn ext_no_extension_is_nil() {
    assert!(matches!(path_ext(&[s("foo/bar")]).unwrap(), VmValue::Nil));
}

#[test]
fn ext_dotfile_is_nil() {
    // a leading-dot file has no extension per std::path semantics
    assert!(matches!(path_ext(&[s(".bashrc")]).unwrap(), VmValue::Nil));
}

#[test]
fn ext_multiple_dots_takes_last() {
    assert_eq!(as_str(path_ext(&[s("archive.tar.gz")]).unwrap()), ".gz");
}

#[test]
fn ext_type_error() {
    let err = path_ext(&[VmValue::Bool(true)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

// ---- stem ----

#[test]
fn stem_with_ext() {
    assert_eq!(as_str(path_stem(&[s("/foo/bar.rs")]).unwrap()), "bar");
}

#[test]
fn stem_no_ext() {
    assert_eq!(as_str(path_stem(&[s("/foo/bar")]).unwrap()), "bar");
}

#[test]
fn stem_multiple_dots() {
    assert_eq!(as_str(path_stem(&[s("archive.tar.gz")]).unwrap()), "archive.tar");
}

#[test]
fn stem_arity_error() {
    let err = path_stem(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

// ---- is_abs ----

#[test]
fn is_abs_true_for_rooted() {
    assert!(matches!(path_is_abs(&[s("/foo/bar")]).unwrap(), VmValue::Bool(true)));
}

#[test]
fn is_abs_false_for_relative() {
    assert!(matches!(path_is_abs(&[s("foo/bar")]).unwrap(), VmValue::Bool(false)));
}

#[test]
fn is_abs_type_error() {
    let err = path_is_abs(&[VmValue::Int(0)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

// ---- abs ----

#[test]
fn abs_of_absolute_is_unchanged() {
    // an already-absolute path stays absolute and is returned as-is (lexically)
    let out = as_str(path_abs(&[s("/foo/bar")]).unwrap());
    assert_eq!(out, "/foo/bar");
}

#[test]
fn abs_of_relative_is_absolute() {
    let out = as_str(path_abs(&[s("foo/bar")]).unwrap());
    assert!(std::path::Path::new(&out).is_absolute());
    assert!(out.ends_with("foo/bar"));
}

#[test]
fn abs_arity_error() {
    let err = path_abs(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}
