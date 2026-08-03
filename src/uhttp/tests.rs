use jade_runtime::coll::DictObj;
use super::*;

// VM-surface tests only. The framing/parsing helpers (parse_unix_url, dechunk,
// parse_response, find_subsequence) now live in `jade_runtime::uhttpf` and are
// unit-tested there. Here we cover VmValue marshalling, arg validation, and the
// package descriptor — all branches that fire before any socket I/O.

// ── extract_headers ───────────────────────────────────────────────────────

#[test]
fn extract_headers_variants() {
    assert!(extract_headers(None).unwrap().is_empty());
    assert!(extract_headers(Some(&VmValue::Nil)).unwrap().is_empty());

    let mut map = DictObj::new();
    map.insert("X-Test".to_string(), VmValue::Str("1".to_string().into()));
    let hs = extract_headers(Some(&VmValue::Dict(map))).unwrap();
    assert_eq!(hs, vec![("X-Test".to_string(), "1".to_string())]);

    let mut bad = DictObj::new();
    bad.insert("X".to_string(), VmValue::Int(1));
    assert!(extract_headers(Some(&VmValue::Dict(bad))).is_err());
    assert!(extract_headers(Some(&VmValue::Int(0))).is_err());
}

// ── arg-validation error branches (no socket) ─────────────────────────────

#[test]
fn uhttp_get_arity_errors() {
    assert!(matches!(uhttp_get(&[]).unwrap_err(), JadeError::ArityMismatch { .. }));
    let three = [VmValue::Str("a".to_string().into()), VmValue::Nil, VmValue::Nil];
    assert!(matches!(uhttp_get(&three).unwrap_err(), JadeError::ArityMismatch { .. }));
}

#[test]
fn uhttp_post_arity_and_type_errors() {
    // too few
    let one = [VmValue::Str("u".to_string().into())];
    assert!(matches!(uhttp_post(&one).unwrap_err(), JadeError::ArityMismatch { .. }));
    // bad url type
    let bad = [VmValue::Int(1), VmValue::Str("b".to_string().into())];
    match uhttp_post(&bad).unwrap_err() {
        JadeError::TypeError { message, .. } => assert_eq!(message, "uhttp.post"),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

#[test]
fn uhttp_get_bad_url_scheme_returns_ioerror() {
    // Valid arity + type, but url parse fails before any socket connect.
    let args = [VmValue::Str("not-a-unix-url".to_string().into())];
    match uhttp_get(&args).unwrap_err() {
        JadeError::IoError { message, .. } => assert!(message.contains("unix://")),
        other => panic!("expected IoError, got {:?}", other),
    }
}

// ── the byte-bodied pair ──────────────────────────────────────────────────

#[test]
fn uhttp_post_bytes_rejects_a_string_body() {
    // A str body is the mistake worth naming: `uhttp.post` takes one, so the
    // two spellings differ by exactly this argument. The message says which
    // type arrived rather than only which was wanted.
    let args = [
        VmValue::Str("unix:///tmp/x.sock:/p".to_string().into()),
        VmValue::Str("not bytes".to_string().into()),
    ];
    match uhttp_post_bytes(&args).unwrap_err() {
        JadeError::TypeError { message, .. } => {
            assert!(message.contains("expects bytes"), "{message}");
            assert!(message.contains("str"), "names what arrived: {message}");
        }
        other => panic!("expected TypeError, got {:?}", other),
    }
}

#[test]
fn uhttp_bytes_pair_validates_arity_and_url() {
    assert!(matches!(uhttp_get_bytes(&[]).unwrap_err(), JadeError::ArityMismatch { .. }));
    let one = [VmValue::Str("u".to_string().into())];
    assert!(matches!(uhttp_post_bytes(&one).unwrap_err(), JadeError::ArityMismatch { .. }));

    // Url parse fails before any socket connect, as on the text path.
    let bad = [VmValue::Str("not-a-unix-url".to_string().into())];
    match uhttp_get_bytes(&bad).unwrap_err() {
        JadeError::IoError { message, .. } => assert!(message.contains("unix://")),
        other => panic!("expected IoError, got {:?}", other),
    }
}

#[test]
fn open_stream_bad_url_errors_synchronously() {
    assert!(open_stream("bogus", vec![]).is_err());
}

// ── package descriptor ────────────────────────────────────────────────────

#[test]
fn pkg_descriptor() {
    assert_eq!(UHTTP_PKG.import_name, "std/uhttp");
    assert_eq!(UHTTP_PKG.global_name, "uhttp");
    let names: Vec<&str> = UHTTP_PKG.fns.iter().map(|f| f.name).collect();
    for verb in ["get", "post", "put", "delete", "head", "get_bytes", "post_bytes"] {
        assert!(names.contains(&verb), "missing {verb}");
    }
}

#[test]
fn vm_dict_value_injects_stream_native() {
    match UHTTP_PKG.vm_dict_value() {
        VmValue::Dict(map) => {
            assert!(map.contains_key("get"));
            match map.get("stream") {
                Some(VmValue::NativeFn(NativeFnId::UhttpStream)) => {}
                other => panic!("stream not a native fn: {:?}", other),
            }
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}
