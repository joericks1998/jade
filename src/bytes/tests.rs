use super::*;
use jade_runtime::trust::{JStr, TAINTED, TRUSTED};

fn blob(data: Vec<u8>, trust: u8) -> VmValue {
    VmValue::Bytes(Arc::new(BytesObj::new(data, trust)))
}

#[test]
fn decode_round_trips_utf8() {
    let b = blob("héllo".as_bytes().to_vec(), TRUSTED);
    let out = bytes_decode(&[b]).expect("decodes");
    match out {
        VmValue::Str(s) => assert_eq!(s.as_str(), "héllo"),
        v => panic!("expected Str, got {v:?}"),
    }
}

/// Reporting beats corrupting. A caller that assumed the bytes were text needs
/// to hear that they were not, rather than getting replacement characters.
#[test]
fn decoding_invalid_utf8_reports_rather_than_substituting() {
    let b = blob(vec![b'o', b'k', 0xFF, 0xFE], TRUSTED);
    let err = bytes_decode(&[b]).expect_err("invalid UTF-8 must raise");
    match err {
        JadeError::Exception { message, .. } => {
            assert!(message.contains("not valid UTF-8"), "got: {message}");
            assert!(message.contains('2'), "names where it failed: {message}");
        }
        e => panic!("expected Exception, got {e:?}"),
    }
}

/// The hole this closes: without trust on the blob, `fs.read_bytes(p).decode()`
/// would produce a clean string and walk past the check `fs.read(p)` cannot.
#[test]
fn trust_survives_a_decode() {
    let b = blob(b"rm -rf /".to_vec(), TAINTED);
    match bytes_decode(&[b]).expect("decodes") {
        VmValue::Str(s) => assert!(s.is_tainted(), "decoded text must stay tainted"),
        v => panic!("expected Str, got {v:?}"),
    }
}

#[test]
fn trust_survives_an_encode() {
    let s = VmValue::Str(JStr::tainted("rm -rf /"));
    match (crate::string::find_str_method("encode").unwrap().vm_impl)(&[s]) {
        Ok(VmValue::Bytes(b)) => assert!(b.is_tainted(), "encoded octets must stay tainted"),
        other => panic!("expected tainted Bytes, got {other:?}"),
    }
}

/// A NUL is data, not a terminator, and 0xFF is not valid UTF-8. Both are the
/// reason `bytes` is not modelled as a string.
#[test]
fn a_blob_round_trips_through_a_nul_and_invalid_utf8() {
    let raw = vec![b'a', 0, b'b', 0xFF];
    let b = blob(raw.clone(), TRUSTED);
    match bytes_len(&[b.clone()]).expect("len") {
        VmValue::Int(n) => assert_eq!(n, 4, "a NUL must not shorten the blob"),
        v => panic!("expected Int, got {v:?}"),
    }
    match b {
        VmValue::Bytes(o) => assert_eq!(o.as_slice(), &raw[..]),
        v => panic!("expected Bytes, got {v:?}"),
    }
}

#[test]
fn slice_clamps_rather_than_raising() {
    let b = blob(vec![1, 2, 3, 4], TRUSTED);
    match bytes_slice(&[b.clone(), VmValue::Int(2), VmValue::Int(99)]).expect("slice") {
        VmValue::Bytes(o) => assert_eq!(o.as_slice(), &[3, 4]),
        v => panic!("expected Bytes, got {v:?}"),
    }
    // An inverted range is empty, not a panic.
    match bytes_slice(&[b, VmValue::Int(3), VmValue::Int(1)]).expect("slice") {
        VmValue::Bytes(o) => assert!(o.is_empty()),
        v => panic!("expected Bytes, got {v:?}"),
    }
}

#[test]
fn slice_keeps_the_trust_of_its_source() {
    let b = blob(vec![1, 2, 3], TAINTED);
    match bytes_slice(&[b, VmValue::Int(0), VmValue::Int(2)]).expect("slice") {
        VmValue::Bytes(o) => assert!(o.is_tainted(), "a slice of tainted data is tainted"),
        v => panic!("expected Bytes, got {v:?}"),
    }
}
