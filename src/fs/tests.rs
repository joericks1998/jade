use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string())
}

// Unique path per call within the process temp dir; auto-cleaned by each test.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_path(tag: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut p = std::env::temp_dir();
    p.push(format!("jade_fs_test_{}_{}_{}", tag, pid, n));
    p.to_string_lossy().into_owned()
}

// ---- write / read / append / exists / delete round-trip ----

#[test]
fn write_then_read_round_trip() {
    let path = unique_path("wr");
    assert!(matches!(fs_write(&[s(&path), s("hello")]).unwrap(), VmValue::Nil));
    let out = fs_read(&[s(&path)]).unwrap();
    assert!(matches!(out, VmValue::Str(ref v) if v == "hello"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn append_after_write() {
    let path = unique_path("ap");
    fs_write(&[s(&path), s("foo")]).unwrap();
    fs_append(&[s(&path), s("bar")]).unwrap();
    let out = fs_read(&[s(&path)]).unwrap();
    assert!(matches!(out, VmValue::Str(ref v) if v == "foobar"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn append_creates_file_if_missing() {
    let path = unique_path("apc");
    let _ = std::fs::remove_file(&path);
    fs_append(&[s(&path), s("x")]).unwrap();
    let out = fs_read(&[s(&path)]).unwrap();
    assert!(matches!(out, VmValue::Str(ref v) if v == "x"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn exists_true_then_false_after_delete() {
    let path = unique_path("ex");
    fs_write(&[s(&path), s("data")]).unwrap();
    assert!(matches!(fs_exists(&[s(&path)]).unwrap(), VmValue::Bool(true)));
    assert!(matches!(fs_delete(&[s(&path)]).unwrap(), VmValue::Nil));
    assert!(matches!(fs_exists(&[s(&path)]).unwrap(), VmValue::Bool(false)));
}

#[test]
fn exists_false_for_missing() {
    let path = unique_path("miss");
    let _ = std::fs::remove_file(&path);
    assert!(matches!(fs_exists(&[s(&path)]).unwrap(), VmValue::Bool(false)));
}

// ---- mkdir / list_dir ----

#[test]
fn mkdir_then_list_dir() {
    let dir = unique_path("dir");
    assert!(matches!(fs_mkdir(&[s(&dir)]).unwrap(), VmValue::Nil));
    // create two files inside
    let f1 = format!("{}/a.txt", dir);
    let f2 = format!("{}/b.txt", dir);
    fs_write(&[s(&f1), s("1")]).unwrap();
    fs_write(&[s(&f2), s("2")]).unwrap();

    let listing = fs_list_dir(&[s(&dir)]).unwrap();
    match listing {
        VmValue::Array(arc) => {
            let mut names: Vec<String> = arc
                .lock()
                .iter()
                .map(|v| match v {
                    VmValue::Str(s) => s.clone(),
                    _ => panic!("expected Str entries"),
                })
                .collect();
            names.sort();
            assert_eq!(names, vec!["a.txt".to_string(), "b.txt".to_string()]);
        }
        other => panic!("expected Array, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mkdir_nested_creates_all() {
    let base = unique_path("nested");
    let nested = format!("{}/a/b/c", base);
    fs_mkdir(&[s(&nested)]).unwrap();
    assert!(std::path::Path::new(&nested).is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

// ---- error paths ----

#[test]
fn read_missing_file_errors() {
    let path = unique_path("nofile");
    let _ = std::fs::remove_file(&path);
    let err = fs_read(&[s(&path)]).unwrap_err();
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn delete_missing_file_errors() {
    let path = unique_path("nodel");
    let _ = std::fs::remove_file(&path);
    let err = fs_delete(&[s(&path)]).unwrap_err();
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn list_dir_missing_errors() {
    let path = unique_path("nodir");
    let err = fs_list_dir(&[s(&path)]).unwrap_err();
    assert!(matches!(err, JadeError::IoError { .. }));
}

#[test]
fn read_arity_error_zero() {
    let err = fs_read(&[]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn read_arity_error_too_many() {
    let err = fs_read(&[s("a"), s("b"), s("c")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn read_trust_flag_accepted() {
    // fs.read accepts an optional trailing arg (ignored by the VM).
    let path = unique_path("trust");
    fs_write(&[s(&path), s("ok")]).unwrap();
    let out = fs_read(&[s(&path), VmValue::Bool(true)]).unwrap();
    assert!(matches!(out, VmValue::Str(ref v) if v == "ok"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn write_type_error_path() {
    let err = fs_write(&[VmValue::Int(1), s("x")]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn write_type_error_content() {
    let err = fs_write(&[s("/tmp/whatever"), VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}

#[test]
fn write_arity_error() {
    let err = fs_write(&[s("a")]).unwrap_err();
    assert!(matches!(err, JadeError::ArityMismatch { .. }));
}

#[test]
fn exists_type_error() {
    let err = fs_exists(&[VmValue::Int(1)]).unwrap_err();
    assert!(matches!(err, JadeError::TypeError { .. }));
}
