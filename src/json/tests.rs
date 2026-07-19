use jade_runtime::coll::DictObj;
use super::*;
use crate::builtins::make_array;

fn s(x: &str) -> VmValue {
    VmValue::Str(x.to_string())
}

fn parse(x: &str) -> VmValue {
    json_parse(&[s(x)]).unwrap()
}

fn stringify(v: VmValue) -> String {
    match json_stringify(&[v]).unwrap() {
        VmValue::Str(s) => s,
        _ => panic!("not a str"),
    }
}

// ── json_parse: scalars ───────────────────────────────────────────────────────

#[test]
fn parse_int() {
    assert!(matches!(parse("42"), VmValue::Int(42)));
}

#[test]
fn parse_float() {
    assert!(matches!(parse("3.5"), VmValue::Float(f) if f == 3.5));
}

#[test]
fn parse_bool() {
    assert!(matches!(parse("true"), VmValue::Bool(true)));
    assert!(matches!(parse("false"), VmValue::Bool(false)));
}

#[test]
fn parse_string() {
    assert!(matches!(parse("\"hi\""), VmValue::Str(x) if x == "hi"));
}

#[test]
fn parse_null_is_nil() {
    assert!(matches!(parse("null"), VmValue::Nil));
}

// ── json_parse: composites ────────────────────────────────────────────────────

#[test]
fn parse_array() {
    let v = parse("[1, 2, 3]");
    match v {
        VmValue::Array(arc) => {
            let g = arc.lock();
            assert_eq!(g.len(), 3);
            assert!(matches!(g[0], VmValue::Int(1)));
        }
        _ => panic!(),
    }
}

#[test]
fn parse_object() {
    let v = parse(r#"{"a": 1, "b": "x"}"#);
    match v {
        VmValue::Dict(m) => {
            assert!(matches!(m.get("a"), Some(VmValue::Int(1))));
            assert!(matches!(m.get("b"), Some(VmValue::Str(s)) if s == "x"));
        }
        _ => panic!(),
    }
}

#[test]
fn parse_nested_with_null() {
    let v = parse(r#"{"k": [null, 1]}"#);
    match v {
        VmValue::Dict(m) => match m.get("k") {
            Some(VmValue::Array(arc)) => {
                let g = arc.lock();
                assert!(matches!(g[0], VmValue::Nil));
                assert!(matches!(g[1], VmValue::Int(1)));
            }
            _ => panic!(),
        },
        _ => panic!(),
    }
}

// ── json_parse: error paths ───────────────────────────────────────────────────

#[test]
fn parse_invalid_is_io_error() {
    assert!(matches!(
        json_parse(&[s("{ not json")]),
        Err(JadeError::IoError { .. })
    ));
}

#[test]
fn parse_non_str_arg() {
    assert!(matches!(
        json_parse(&[VmValue::Int(1)]),
        Err(JadeError::TypeError { .. })
    ));
}

#[test]
fn parse_arity() {
    assert!(matches!(
        json_parse(&[]),
        Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })
    ));
}

// ── json_stringify ────────────────────────────────────────────────────────────

#[test]
fn stringify_int() {
    assert_eq!(stringify(VmValue::Int(7)), "7");
}

#[test]
fn stringify_nil_is_null() {
    assert_eq!(stringify(VmValue::Nil), "null");
}

#[test]
fn stringify_string() {
    assert_eq!(stringify(VmValue::Str("hi".into())), "\"hi\"");
}

#[test]
fn stringify_array() {
    let a = make_array(vec![VmValue::Int(1), VmValue::Int(2)]);
    assert_eq!(stringify(a), "[1,2]");
}

#[test]
fn stringify_arity() {
    assert!(matches!(
        json_stringify(&[]),
        Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })
    ));
}

// ── round-trips ───────────────────────────────────────────────────────────────

#[test]
fn roundtrip_object() {
    let mut m = DictObj::new();
    m.insert("n".to_string(), VmValue::Int(5));
    let orig = VmValue::Dict(m);
    let text = stringify(orig);
    let back = parse(&text);
    match back {
        VmValue::Dict(m) => assert!(matches!(m.get("n"), Some(VmValue::Int(5)))),
        _ => panic!(),
    }
}

#[test]
fn roundtrip_null() {
    let text = stringify(VmValue::Nil);
    assert!(matches!(parse(&text), VmValue::Nil));
}

#[test]
fn roundtrip_nested() {
    let inner = make_array(vec![VmValue::Bool(true), VmValue::Nil]);
    let mut m = DictObj::new();
    m.insert("list".to_string(), inner);
    let text = stringify(VmValue::Dict(m));
    let back = parse(&text);
    match back {
        VmValue::Dict(m) => match m.get("list") {
            Some(VmValue::Array(arc)) => {
                let g = arc.lock();
                assert!(matches!(g[0], VmValue::Bool(true)));
                assert!(matches!(g[1], VmValue::Nil));
            }
            _ => panic!(),
        },
        _ => panic!(),
    }
}

// ── json_stringify_pretty ─────────────────────────────────────────────────────

#[test]
fn stringify_pretty_has_newlines() {
    let a = make_array(vec![VmValue::Int(1), VmValue::Int(2)]);
    match json_stringify_pretty(&[a]).unwrap() {
        VmValue::Str(s) => assert!(s.contains('\n')),
        _ => panic!(),
    }
}

#[test]
fn stringify_pretty_arity() {
    assert!(matches!(
        json_stringify_pretty(&[]),
        Err(JadeError::ArityMismatch { expected: 1, got: 0, .. })
    ));
}
