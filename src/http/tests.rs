use super::*;
use jade_runtime::coll::DictObj;

// NOTE: no live-network tests here. The request-dispatch path (`execute`) opens
// a real TCP connection, so we only cover the PURE helpers and the arg-validation
// error branches that fire *before* any network I/O happens.

// ── require_str_owned ─────────────────────────────────────────────────────

#[test]
fn require_str_owned_ok() {
    let args = [VmValue::Str("hi".to_string().into())];
    assert_eq!(require_str_owned(&args, 0, "http.get").unwrap(), "hi");
}

#[test]
fn require_str_owned_wrong_type() {
    let args = [VmValue::Int(3)];
    match require_str_owned(&args, 0, "http.get").unwrap_err() {
        JadeError::TypeError { message, .. } => assert_eq!(message, "http.get"),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

#[test]
fn require_str_owned_missing_arg() {
    let args: [VmValue; 0] = [];
    match require_str_owned(&args, 0, "http.get").unwrap_err() {
        JadeError::ArityMismatch { expected, got, .. } => {
            assert_eq!(expected, 1);
            assert_eq!(got, 0);
        }
        other => panic!("expected ArityMismatch, got {:?}", other),
    }
}

// ── extract_headers ───────────────────────────────────────────────────────

#[test]
fn extract_headers_none_is_empty() {
    assert!(extract_headers(None).unwrap().is_empty());
}

#[test]
fn extract_headers_nil_is_empty() {
    assert!(extract_headers(Some(&VmValue::Nil)).unwrap().is_empty());
}

#[test]
fn extract_headers_dict_of_strings() {
    let mut map = DictObj::new();
    map.insert("Accept".to_string(), VmValue::Str("application/json".to_string().into()));
    let hs = extract_headers(Some(&VmValue::dict(map))).unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0], ("Accept".to_string(), "application/json".to_string()));
}

#[test]
fn extract_headers_non_str_value_errors() {
    let mut map = DictObj::new();
    map.insert("X".to_string(), VmValue::Int(1));
    match extract_headers(Some(&VmValue::dict(map))).unwrap_err() {
        JadeError::TypeError { message, .. } => assert!(message.contains("header value")),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

#[test]
fn extract_headers_non_dict_errors() {
    match extract_headers(Some(&VmValue::Str("nope".to_string().into()))).unwrap_err() {
        JadeError::TypeError { message, .. } => assert!(message.contains("dict")),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

// ── make_response ─────────────────────────────────────────────────────────

#[test]
fn make_response_shape() {
    match make_response(200, "OK".to_string()) {
        VmValue::Dict(map) => {
            match map.get("status") {
                Some(VmValue::Int(200)) => {}
                other => panic!("bad status: {:?}", other),
            }
            match map.get("body") {
                Some(VmValue::Str(b)) => assert_eq!(b, "OK"),
                other => panic!("bad body: {:?}", other),
            }
        }
        other => panic!("expected Dict, got {:?}", other),
    }
}

// ── arg-validation error branches (no network) ────────────────────────────

#[test]
fn http_get_empty_args_arity_err() {
    match http_get(&[]).unwrap_err() {
        JadeError::ArityMismatch { .. } => {}
        other => panic!("expected ArityMismatch, got {:?}", other),
    }
}

#[test]
fn http_get_too_many_args_arity_err() {
    let args = [VmValue::Str("a".to_string().into()), VmValue::Nil, VmValue::Nil];
    match http_get(&args).unwrap_err() {
        JadeError::ArityMismatch { .. } => {}
        other => panic!("expected ArityMismatch, got {:?}", other),
    }
}

#[test]
fn http_post_too_few_args_arity_err() {
    let args = [VmValue::Str("url".to_string().into())];
    match http_post(&args).unwrap_err() {
        JadeError::ArityMismatch { expected, .. } => assert_eq!(expected, 2),
        other => panic!("expected ArityMismatch, got {:?}", other),
    }
}

#[test]
fn http_post_bad_url_type_err() {
    let args = [VmValue::Int(1), VmValue::Str("body".to_string().into())];
    match http_post(&args).unwrap_err() {
        JadeError::TypeError { message, .. } => assert_eq!(message, "http.post"),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

#[test]
fn http_delete_and_head_reject_empty_args() {
    assert!(matches!(http_delete(&[]).unwrap_err(), JadeError::ArityMismatch { .. }));
    assert!(matches!(http_head(&[]).unwrap_err(), JadeError::ArityMismatch { .. }));
}

#[test]
fn http_put_bad_body_type_err() {
    let args = [VmValue::Str("url".to_string().into()), VmValue::Int(9)];
    match http_put(&args).unwrap_err() {
        JadeError::TypeError { message, .. } => assert_eq!(message, "http.put"),
        other => panic!("expected TypeError, got {:?}", other),
    }
}

// ── package descriptor ────────────────────────────────────────────────────

#[test]
fn pkg_descriptor_lists_all_verbs() {
    assert_eq!(HTTP_PKG.import_name, "std/http");
    assert_eq!(HTTP_PKG.global_name, "http");
    let names: Vec<&str> = HTTP_PKG.fns.iter().map(|f| f.name).collect();
    for verb in ["get", "post", "put", "delete", "head", "get_bytes", "post_bytes"] {
        assert!(names.contains(&verb), "missing {verb}");
    }
}

/// `std::uhttp` grew the byte pair a release after `std::http` did, and the gap
/// was invisible because nothing compared the two tables. This does.
#[test]
fn http_and_uhttp_expose_the_same_functions() {
    use crate::uhttp::UHTTP_PKG;
    let mut mine: Vec<&str> = HTTP_PKG.fns.iter().map(|f| f.name).collect();
    let mut theirs: Vec<&str> = UHTTP_PKG
        .fns
        .iter()
        .map(|f| f.name)
        // `stream` is uhttp-only: a TCP streaming read has no caller yet.
        .chain(UHTTP_PKG.natives.iter().map(|(n, _)| *n).filter(|n| *n != "stream"))
        .collect();
    mine.sort_unstable();
    theirs.sort_unstable();
    assert_eq!(mine, theirs, "the two HTTP packages must offer the same surface");
}

#[test]
fn register_types_runs() {
    let mut ctx = TypeContext::new();
    (HTTP_PKG.register_types)(&mut ctx);
}
