//! Unit tests for the `llm` module.
//!
//! This file used to hold golden wire tests pinning the exact JSON bytes
//! jadelang sent to the inference daemon. There is no wire left to pin: the
//! daemon and its socket are gone, and a request now reaches a provider package
//! as an in-process dict. What is worth guarding moved with it — the shape of
//! that dict, checked below, since a package reads it by key.

use crate::llm::{provider_backend::request_value, InferenceRequest};
use crate::vm::VmValue;

/// Read a string entry out of the request dict, or `None` if absent.
fn key(req: &InferenceRequest, name: &str) -> Option<String> {
    let VmValue::Dict(d) = request_value(req) else { panic!("not a dict") };
    match d.get(name) {
        Some(VmValue::Str(s)) => Some(s.as_str().to_owned()),
        _ => None,
    }
}

/// A plain `?p` sends the prompt and nothing else. Absence is meaningful: a
/// package distinguishes "no grammar" from "an empty grammar" by the key not
/// being there.
#[test]
fn a_plain_prompt_sends_only_the_prompt() {
    let req = InferenceRequest { prompt: "hi".into(), ..Default::default() };
    assert_eq!(key(&req, "prompt").as_deref(), Some("hi"));
    assert_eq!(key(&req, "grammar"), None);
    assert_eq!(key(&req, "anchor"), None);
    assert_eq!(key(&req, "stop_anchor"), None);
}

/// The grammar and both anchors travel together. Sending the grammar alone
/// would drop half of an explicit `Grammar.new(pattern, anchor, stop)` without
/// the package ever knowing the constraint was incomplete.
#[test]
fn a_grammar_carries_its_anchors() {
    let req = InferenceRequest {
        prompt: "p".into(),
        grammar: Some(r#"root ::= "{" [^}]* "}""#.into()),
        anchor: Some("<tool_call>".into()),
        stop_anchor: Some("</tool_call>".into()),
    };
    assert_eq!(key(&req, "grammar").as_deref(), Some(r#"root ::= "{" [^}]* "}""#));
    assert_eq!(key(&req, "anchor").as_deref(), Some("<tool_call>"));
    assert_eq!(key(&req, "stop_anchor").as_deref(), Some("</tool_call>"));
}

/// A typed dereference sets a grammar with no anchors — the whole reply is the
/// constrained span. The anchor keys must stay absent rather than arriving empty.
#[test]
fn a_typed_deref_sends_a_grammar_without_anchors() {
    let req = InferenceRequest {
        prompt: "p".into(),
        grammar: Some("root ::= [0-9]+".into()),
        ..Default::default()
    };
    assert!(key(&req, "grammar").is_some());
    assert_eq!(key(&req, "anchor"), None);
    assert_eq!(key(&req, "stop_anchor"), None);
}
