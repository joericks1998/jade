//! Tests for the pure pieces of the build-daemon client: TIR→JSON request
//! serialization, the length-prefix ENCODE side, and the response-frame DECODE
//! side (given raw bytes). The socket IO in `build()` is not exercised here —
//! it has no separable pure helper beyond `encode_request`/`decode_frame`.

use super::*;

fn empty_program() -> TProgram {
    TProgram { stmts: vec![] }
}

// ── encode_request ────────────────────────────────────────────────────────────

/// Strip the 4-byte LE length prefix and return (declared_len, json_value).
fn decode_payload(payload: &[u8]) -> (u32, serde_json::Value) {
    assert!(payload.len() >= 4, "payload must carry a length prefix");
    let len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let json = &payload[4..];
    let value: serde_json::Value = serde_json::from_slice(json).expect("valid json body");
    (len, value)
}

#[test]
fn encode_request_length_prefix_matches_body() {
    let prog = empty_program();
    let payload = encode_request(
        &prog,
        Path::new("/src/main.jde"),
        Path::new("/out/bin"),
        &Emit::Binary,
    )
    .unwrap();
    let (declared, _) = decode_payload(&payload);
    // Prefix must equal the actual JSON byte length.
    assert_eq!(declared as usize, payload.len() - 4);
}

#[test]
fn encode_request_binary_emit_fields() {
    let prog = empty_program();
    let payload = encode_request(
        &prog,
        Path::new("/proj/app.jde"),
        Path::new("/proj/dist/app"),
        &Emit::Binary,
    )
    .unwrap();
    let (_, v) = decode_payload(&payload);
    assert_eq!(v["op"], "build");
    assert_eq!(v["emit"], "binary");
    assert_eq!(v["out"], "/proj/dist/app");
    assert_eq!(v["source_path"], "/proj/app.jde");
    assert!(v["target"].is_null());
    // TIR is embedded as an object with a `stmts` array.
    assert!(v["tir"]["stmts"].is_array());
    assert_eq!(v["tir"]["stmts"].as_array().unwrap().len(), 0);
}

#[test]
fn encode_request_ir_emit_flag() {
    let prog = empty_program();
    let payload = encode_request(&prog, Path::new("/a.jde"), Path::new("/a.ll"), &Emit::Ir).unwrap();
    let (_, v) = decode_payload(&payload);
    assert_eq!(v["emit"], "ir");
}

#[test]
fn encode_request_roundtrips_tir() {
    // A program with one Let stmt should serialize and be recoverable.
    use crate::compiler::tir::{TExpr, TExprKind, TStmt};
    use crate::compiler::tir::JadeType;
    use crate::frontend::error::Span;

    let stmt = TStmt::Let {
        name: "x".into(),
        value: TExpr {
            kind: TExprKind::Integer(42),
            ty: JadeType::Int,
            span: Span { line: 1, col: 1 },
        },
        span: Span { line: 1, col: 1 },
    };
    let prog = TProgram { stmts: vec![stmt] };
    let payload = encode_request(&prog, Path::new("/x.jde"), Path::new("/x"), &Emit::Binary).unwrap();
    let (_, v) = decode_payload(&payload);
    let stmts = v["tir"]["stmts"].as_array().unwrap();
    assert_eq!(stmts.len(), 1);
    // Recover the full TProgram from the embedded tir value.
    let recovered: TProgram = serde_json::from_value(v["tir"].clone()).unwrap();
    assert_eq!(recovered.stmts.len(), 1);
}

// ── decode_frame ──────────────────────────────────────────────────────────────

/// Build a raw frame: [u8 type][u16 LE len][payload].
fn frame(ty: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(ty);
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

#[test]
fn decode_frame_incomplete_header() {
    // Fewer than 3 bytes → Incomplete.
    assert!(matches!(decode_frame(&[]), FrameResult::Incomplete));
    assert!(matches!(decode_frame(&[0x01]), FrameResult::Incomplete));
    assert!(matches!(decode_frame(&[0x01, 0x00]), FrameResult::Incomplete));
}

#[test]
fn decode_frame_incomplete_payload() {
    // Header says 5 payload bytes but only 2 present.
    let mut buf = vec![0x01, 0x05, 0x00];
    buf.extend_from_slice(b"ab");
    assert!(matches!(decode_frame(&buf), FrameResult::Incomplete));
}

#[test]
fn decode_frame_token() {
    let f = frame(0x01, b"llvm ir here");
    match decode_frame(&f) {
        FrameResult::Token(s, consumed) => {
            assert_eq!(s, "llvm ir here");
            assert_eq!(consumed, f.len());
        }
        other => panic!("expected Token, got {other:?}"),
    }
}

#[test]
fn decode_frame_done() {
    let f = frame(0x02, b"");
    match decode_frame(&f) {
        FrameResult::Done(consumed) => assert_eq!(consumed, 3),
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn decode_frame_error() {
    let f = frame(0x03, b"link failed");
    match decode_frame(&f) {
        FrameResult::Error(msg, consumed) => {
            assert_eq!(msg, "link failed");
            assert_eq!(consumed, f.len());
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn decode_frame_meta() {
    let f = frame(0x04, b"informational");
    match decode_frame(&f) {
        FrameResult::Meta(consumed) => assert_eq!(consumed, f.len()),
        other => panic!("expected Meta, got {other:?}"),
    }
}

#[test]
fn decode_frame_unknown_type() {
    let f = frame(0x7f, b"whatever");
    match decode_frame(&f) {
        FrameResult::UnknownType(t) => assert_eq!(t, 0x7f),
        other => panic!("expected UnknownType, got {other:?}"),
    }
}

#[test]
fn decode_frame_invalid_utf8_token_is_error() {
    // 0xff 0xfe is not valid UTF-8.
    let f = frame(0x01, &[0xff, 0xfe]);
    match decode_frame(&f) {
        FrameResult::Error(msg, _) => assert!(msg.contains("invalid UTF-8")),
        other => panic!("expected Error for bad utf8, got {other:?}"),
    }
}

#[test]
fn decode_frame_consumes_exactly_one_frame() {
    // Two frames concatenated: decode should report consuming only the first.
    let mut buf = frame(0x01, b"first");
    let second = frame(0x02, b"");
    buf.extend_from_slice(&second);
    match decode_frame(&buf) {
        FrameResult::Token(s, consumed) => {
            assert_eq!(s, "first");
            // The remaining bytes are exactly the second frame.
            assert_eq!(&buf[consumed..], second.as_slice());
        }
        other => panic!("expected Token, got {other:?}"),
    }
}

// FrameResult needs Debug for the panic!("{other:?}") messages above.
impl std::fmt::Debug for FrameResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameResult::Token(s, c) => write!(f, "Token({s:?}, {c})"),
            FrameResult::Meta(c) => write!(f, "Meta({c})"),
            FrameResult::Done(c) => write!(f, "Done({c})"),
            FrameResult::Error(s, c) => write!(f, "Error({s:?}, {c})"),
            FrameResult::Incomplete => write!(f, "Incomplete"),
            FrameResult::UnknownType(t) => write!(f, "UnknownType({t:#04x})"),
        }
    }
}

#[test]
fn cdylib_request_carries_emit_and_exports() {
    let prog = TProgram { stmts: vec![] };
    let payload = encode_request(
        &prog,
        Path::new("/lib.jde"),
        Path::new("/lib.so"),
        &Emit::CDylib { exports: vec!["add".into(), "triple".into()] },
    )
    .unwrap();

    let json: serde_json::Value = serde_json::from_slice(&payload[4..]).unwrap();
    assert_eq!(json["emit"], "cdylib");
    assert_eq!(json["exports"], serde_json::json!(["add", "triple"]));
}

#[test]
fn a_non_cdylib_request_has_a_null_exports_field() {
    // The field is always present so the daemon can read it unconditionally.
    let prog = TProgram { stmts: vec![] };
    let payload =
        encode_request(&prog, Path::new("/a.jde"), Path::new("/a"), &Emit::Binary).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&payload[4..]).unwrap();
    assert_eq!(json["emit"], "binary");
    assert!(json["exports"].is_null());
}
