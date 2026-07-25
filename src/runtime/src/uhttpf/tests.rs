use super::*;

// Pure framing/parsing helpers only — `request` connects to a real Unix socket
// and is covered by the compiler crate's end-to-end tests.

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
    assert!(err.contains("unix://"));
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

#[test]
fn dechunk_missing_trailing_crlf_errors_no_panic() {
    // Payload fits exactly but the trailing CRLF (and 0-terminator) is missing —
    // must be a clean Err, never a panic from `pos` running past the buffer.
    assert!(dechunk(b"5\r\nhello").is_err());
    assert!(dechunk(b"5\r\nhello\r").is_err()); // only one of the two trailer bytes
    assert!(dechunk(b"3\r\nfoo\r\n5\r\nhello").is_err()); // second chunk truncated
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
