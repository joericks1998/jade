//! Unit tests for the `llm` module.
//!
//! This file used to hold golden wire tests pinning the exact JSON bytes
//! jadelang sent to the inference daemon. There is no wire left to pin: the
//! daemon and its socket are gone, and a request now reaches a provider package
//! as an in-process `InferRequest` struct. What is worth guarding moved with it —
//! the shape of that struct, and whether it still matches the shared definition
//! both this repo and dovata are written against.

use crate::llm::{provider_backend::request_value, InferenceRequest, REQUEST_TYPE};
use crate::vm::VmValue;

/// The request as a list of `(field, value)` pairs, values rendered as
/// `Some(text)` for a string and `None` for nil. Panics if the request is not a
/// struct of the expected type — which is the contract, not an incidental detail.
fn request_fields(req: &InferenceRequest) -> Vec<(String, Option<String>)> {
    let VmValue::Struct(arc) = request_value(req) else { panic!("request is not a struct") };
    let guard = arc.lock();
    assert_eq!(guard.type_name(), REQUEST_TYPE, "wrong struct type on the request");
    guard
        .fields()
        .iter()
        .map(|(k, v)| {
            let rendered = match v {
                VmValue::Str(s) => Some(s.as_str().to_owned()),
                VmValue::Nil => None,
                other => panic!("field `{k}` is neither a string nor nil: {other:?}"),
            };
            (k.clone(), rendered)
        })
        .collect()
}

/// A plain `?p` fills `input` and leaves the rest nil.
///
/// Unlike the dict this replaced, every field is always present — absence is
/// spelled `nil`. A package reads `request.grammar` and gets nil rather than
/// having to ask whether the key exists.
#[test]
fn a_plain_prompt_sets_only_the_input() {
    let req = InferenceRequest { prompt: "hi".into(), ..Default::default() };
    assert_eq!(
        request_fields(&req),
        [
            ("input".into(), Some("hi".into())),
            ("grammar".into(), None),
            ("anchor".into(), None),
            ("stop_anchor".into(), None),
        ]
    );
}

/// The grammar and both anchors travel together. Sending the grammar alone would
/// drop half of an explicit `Grammar.new(pattern, anchor, stop)` without the
/// package ever knowing the constraint was incomplete.
#[test]
fn a_grammar_carries_its_anchors() {
    let req = InferenceRequest {
        prompt: "p".into(),
        grammar: Some(r#"root ::= "{" [^}]* "}""#.into()),
        anchor: Some("<tool_call>".into()),
        stop_anchor: Some("</tool_call>".into()),
    };
    assert_eq!(
        request_fields(&req),
        [
            ("input".into(), Some("p".into())),
            ("grammar".into(), Some(r#"root ::= "{" [^}]* "}""#.into())),
            ("anchor".into(), Some("<tool_call>".into())),
            ("stop_anchor".into(), Some("</tool_call>".into())),
        ]
    );
}

/// A typed dereference sets a grammar with no anchors — the whole reply is the
/// constrained span.
#[test]
fn a_typed_deref_sends_a_grammar_without_anchors() {
    let req = InferenceRequest {
        prompt: "p".into(),
        grammar: Some("root ::= [0-9]+".into()),
        ..Default::default()
    };
    let fields = request_fields(&req);
    assert_eq!(fields[1], ("grammar".into(), Some("root ::= [0-9]+".into())));
    assert_eq!(fields[2].1, None, "anchor must be nil, not an empty string");
    assert_eq!(fields[3].1, None, "stop_anchor must be nil, not an empty string");
}

// ── Tripwire: the shared definition vs. what the compiler emits ──────────────
//
// `protocol/jade/infer.jde` is the one definition dovata's
// provider packages are written against. The compiler cannot import it — the
// request is built in Rust here and in C in `runtime_aot/infer/infer.c`, both
// naming fields as string literals. So a rename in that file would leave the
// two sides silently disagreeing: a provider reading `request.stop` when we
// still send `stop_anchor` gets nil, with no error at any layer.
//
// These tests close that. They parse the shared file with the compiler's own
// lexer and parser — not a regex, so a comment mentioning a field name cannot
// fool them — and assert it matches `REQUEST_FIELDS` exactly, in order. A rename
// on either side fails `cargo test` naming the drift.

use crate::frontend::{ast::Stmt, lexer, parser};
use crate::llm::REQUEST_FIELDS;

/// Path to the shared definition, relative to the crate root.
const SHARED_DEF: &str = "protocol/jade/infer.jde";

/// The field names `REQUEST_TYPE` declares in the shared file, in order.
///
/// A missing file is a hard failure, never a skip: the submodule being absent is
/// exactly when drift goes unnoticed, so a quiet pass would defeat the tripwire.
fn fields_in_shared_definition() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SHARED_DEF);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read the shared protocol definition at {}: {e}\n\
             If the submodule is not checked out, run `git submodule update --init`.\n\
             CI needs `submodules: recursive` on actions/checkout.",
            path.display()
        )
    });

    let program = parser::parse(lexer::tokenize(&src).expect("shared definition does not lex"))
        .expect("shared definition does not parse");

    for stmt in &program.stmts {
        if let Stmt::StructDef { name, fields, .. } = stmt {
            if name == REQUEST_TYPE {
                return fields.iter().map(|f| f.name().to_owned()).collect();
            }
        }
    }
    panic!("the shared definition declares no `struct {REQUEST_TYPE}`");
}

#[test]
fn request_fields_match_the_shared_definition() {
    let shared = fields_in_shared_definition();
    let ours: Vec<String> = REQUEST_FIELDS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        shared, ours,
        "\n{SHARED_DEF} and llm::REQUEST_FIELDS disagree.\n\
         shared definition: {shared:?}\n\
         compiler emits:    {ours:?}\n\
         A provider reads these by name, so a mismatch is a silently dropped \
         field rather than an error. Update whichever side is behind."
    );
}

/// The AOT engine builds the same request in C, where a Rust constant cannot
/// reach. Check its source text instead: every field must appear as a key
/// literal in `provider_request`, so the two engines cannot send different
/// shapes to the same package.
#[test]
fn the_c_emitter_names_every_request_field() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/runtime_aot/infer/infer.c");
    let src = std::fs::read_to_string(&path).expect("cannot read infer.c");
    let body = {
        let start = src.find("provider_request").expect("provider_request is gone from infer.c");
        let rest = &src[start..];
        let end = rest.find("\n}").expect("cannot find the end of provider_request");
        &rest[..end]
    };

    for field in REQUEST_FIELDS {
        assert!(
            body.contains(&format!("\"{field}\"")),
            "provider_request in infer.c never names the `{field}` field, so a \
             compiled binary sends a different request than the VM does"
        );
    }
}
