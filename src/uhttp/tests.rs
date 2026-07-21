use jade_runtime::coll::DictObj;
use super::*;

// NOTE: no live-socket tests. `execute`/`open_stream` connect to a real Unix
// socket, so we cover only the PURE framing/parsing helpers and the
// arg-validation error branches that fire before any I/O.

// ── parse_unix_url ────────────────────────────────────────────────────────

#[test]
fn parse_url_sock_and_path() {
    let (sock, path) = parse_unix_url("unix:///var/run/docker.sock:/v1.43/containers/json").unwrap();
    assert_eq!(sock, "/var/run/docker.sock");
    assert_eq!(path, "/v1.43/containers/json");
}

#[test]
fn parse_url_no_path_defaults_to_root() {
    let (sock, path) = parse_unix_url("unix:///tmp/my.sock").unwrap();
    assert_eq!(sock, "/tmp/my.sock");
    assert_eq!(path, "/");
}

#[test]
fn parse_url_empty_path_after_colon_defaults_to_root() {
    let (sock, path) = parse_unix_url("unix:///tmp/my.sock:").unwrap();
    assert_eq!(sock, "/tmp/my.sock");
    assert_eq!(path, "/");
}

#[test]
fn parse_url_keeps_colons_in_request_path() {
    // Only the first colon after the scheme splits sock/path.
    let (sock, path) = parse_unix_url("unix:///s.sock:/a?x=1:2:3").unwrap();
    assert_eq!(sock, "/s.sock");
    assert_eq!(path, "/a?x=1:2:3");
}

#[test]
fn parse_url_missing_scheme_errors() {
    let err = parse_unix_url("http://x").unwrap_err();
    match err {
        JadeError::IoError { message, .. } => assert!(message.contains("unix://")),
        other => panic!("expected IoError, got {:?}", other),
    }
}

#[test]
fn parse_url_empty_socket_errors() {
    // "unix://:" → empty socket path.
    assert!(parse_unix_url("unix://:/path").is_err());
    // "unix://" with nothing after.
    assert!(parse_unix_url("unix://").is_err());
}

// ── find_subsequence ──────────────────────────────────────────────────────

#[test]
fn find_subsequence_found() {
    assert_eq!(find_subsequence(b"abc\r\n\r\ndef", b"\r\n\r\n"), Some(3));
}

#[test]
fn find_subsequence_not_found() {
    assert_eq!(find_subsequence(b"abcdef", b"xyz"), None);
}

#[test]
fn find_subsequence_empty_needle_or_too_short() {
    assert_eq!(find_subsequence(b"abc", b""), None);
    assert_eq!(find_subsequence(b"a", b"abc"), None);
}

// ── dechunk ───────────────────────────────────────────────────────────────

#[test]
fn dechunk_single_chunk() {
    // "5\r\nhello\r\n0\r\n"
    let data = b"5\r\nhello\r\n0\r\n";
    assert_eq!(dechunk(data).unwrap(), b"hello");
}

#[test]
fn dechunk_multiple_chunks() {
    let data = b"3\r\nfoo\r\n3\r\nbar\r\n0\r\n";
    assert_eq!(dechunk(data).unwrap(), b"foobar");
}

#[test]
fn dechunk_ignores_chunk_extensions() {
    let data = b"5;ext=1\r\nhello\r\n0\r\n";
    assert_eq!(dechunk(data).unwrap(), b"hello");
}

#[test]
fn dechunk_zero_chunk_yields_empty() {
    assert_eq!(dechunk(b"0\r\n").unwrap(), Vec::<u8>::new());
}

#[test]
fn dechunk_bad_size_errors() {
    assert!(dechunk(b"zz\r\nhello\r\n0\r\n").is_err());
}

#[test]
fn dechunk_truncated_body_errors() {
    // says 10 bytes but only 2 present
    assert!(dechunk(b"a\r\nhi\r\n0\r\n").is_err());
}

// ── parse_response ────────────────────────────────────────────────────────

#[test]
fn parse_response_content_length() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
    let (status, body) = parse_response(raw, false).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "hello");
}

#[test]
fn parse_response_content_length_truncates_extra() {
    // Content-Length shorter than actual bytes → body clipped to len.
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhello";
    let (_, body) = parse_response(raw, false).unwrap();
    assert_eq!(body, "he");
}

#[test]
fn parse_response_chunked() {
    let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n";
    let (status, body) = parse_response(raw, false).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "hello");
}

#[test]
fn parse_response_no_length_reads_to_eof() {
    let raw = b"HTTP/1.1 201 Created\r\n\r\nbodytext";
    let (status, body) = parse_response(raw, false).unwrap();
    assert_eq!(status, 201);
    assert_eq!(body, "bodytext");
}

#[test]
fn parse_response_head_suppresses_body() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
    let (status, body) = parse_response(raw, true).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "");
}

#[test]
fn parse_response_case_insensitive_headers() {
    let raw = b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\n\r\nabcXXX";
    let (_, body) = parse_response(raw, false).unwrap();
    assert_eq!(body, "abc");
}

#[test]
fn parse_response_no_header_terminator_errors() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n";
    assert!(parse_response(raw, false).is_err());
}

#[test]
fn parse_response_malformed_status_line_errors() {
    let raw = b"GARBAGE\r\n\r\nbody";
    assert!(parse_response(raw, false).is_err());
}

// ── extract_headers (pub) ─────────────────────────────────────────────────

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

// ── HttpMethod ────────────────────────────────────────────────────────────

#[test]
fn http_method_verb_and_body() {
    assert_eq!(HttpMethod::Get.verb(), "GET");
    assert_eq!(HttpMethod::Delete.verb(), "DELETE");
    assert_eq!(HttpMethod::Head.verb(), "HEAD");
    let p = HttpMethod::Post("b".to_string());
    assert_eq!(p.verb(), "POST");
    assert_eq!(p.body(), Some("b"));
    let put = HttpMethod::Put("c".to_string());
    assert_eq!(put.verb(), "PUT");
    assert_eq!(put.body(), Some("c"));
    assert_eq!(HttpMethod::Get.body(), None);
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
    for verb in ["get", "post", "put", "delete", "head"] {
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
