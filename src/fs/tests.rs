use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string().into())
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
                    VmValue::Str(s) => s.to_string(),
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

// ── v1.3.23: metadata, copy, rename, rmdir ────────────────────────────────────

#[test]
fn is_file_and_is_dir_tell_the_three_cases_apart() {
    let path = unique_path("isf");
    fs_write(&[s(&path), s("x")]).unwrap();
    assert!(matches!(fs_is_file(&[s(&path)]), Ok(VmValue::Bool(true))));
    assert!(matches!(fs_is_dir(&[s(&path)]), Ok(VmValue::Bool(false))));

    let dir = unique_path("isd");
    fs_mkdir(&[s(&dir)]).unwrap();
    assert!(matches!(fs_is_dir(&[s(&dir)]), Ok(VmValue::Bool(true))));
    assert!(matches!(fs_is_file(&[s(&dir)]), Ok(VmValue::Bool(false))));

    // Absent is false for both, matching `fs.exists` rather than raising.
    let gone = unique_path("isg");
    assert!(matches!(fs_is_file(&[s(&gone)]), Ok(VmValue::Bool(false))));
    assert!(matches!(fs_is_dir(&[s(&gone)]), Ok(VmValue::Bool(false))));

    let _ = fs_delete(&[s(&path)]);
    let _ = fs_rmdir(&[s(&dir)]);
}

#[test]
fn size_reports_bytes_and_raises_when_absent() {
    let path = unique_path("sz");
    fs_write(&[s(&path), s("hello")]).unwrap();
    assert!(matches!(fs_size(&[s(&path)]), Ok(VmValue::Int(5))));
    let _ = fs_delete(&[s(&path)]);
    assert!(fs_size(&[s(&path)]).is_err(), "a missing path must raise, not answer 0");
}

#[test]
fn copy_and_rename_move_the_content() {
    let src = unique_path("cpsrc");
    let dst = unique_path("cpdst");
    let moved = unique_path("cpmv");
    fs_write(&[s(&src), s("payload")]).unwrap();

    fs_copy(&[s(&src), s(&dst)]).unwrap();
    assert!(matches!(fs_size(&[s(&dst)]), Ok(VmValue::Int(7))));
    assert!(matches!(fs_exists(&[s(&src)]), Ok(VmValue::Bool(true))), "copy leaves the source");

    fs_rename(&[s(&dst), s(&moved)]).unwrap();
    assert!(matches!(fs_exists(&[s(&moved)]), Ok(VmValue::Bool(true))));
    assert!(matches!(fs_exists(&[s(&dst)]), Ok(VmValue::Bool(false))), "rename removes the source");

    assert!(fs_copy(&[s(&unique_path("cpgone")), s(&dst)]).is_err());

    let _ = fs_delete(&[s(&src)]);
    let _ = fs_delete(&[s(&moved)]);
}

/// Deliberately not recursive: a non-empty directory is an error, not a
/// silent recursive delete.
#[test]
fn rmdir_refuses_a_non_empty_directory() {
    let dir = unique_path("rmd");
    fs_mkdir(&[s(&dir)]).unwrap();
    let inner = format!("{dir}/f.txt");
    fs_write(&[s(&inner), s("x")]).unwrap();

    assert!(fs_rmdir(&[s(&dir)]).is_err(), "a non-empty directory must not be removed");

    fs_delete(&[s(&inner)]).unwrap();
    assert!(matches!(fs_rmdir(&[s(&dir)]), Ok(VmValue::Nil)));
    assert!(matches!(fs_exists(&[s(&dir)]), Ok(VmValue::Bool(false))));
}

#[test]
fn the_new_fs_fns_check_their_arity() {
    assert!(matches!(fs_size(&[]), Err(JadeError::ArityMismatch { .. })));
    assert!(matches!(fs_copy(&[s("a")]), Err(JadeError::ArityMismatch { .. })));
    assert!(matches!(fs_is_dir(&[s("a"), s("b")]), Err(JadeError::ArityMismatch { .. })));
}
