//! Unit tests for the `llm` module.
//!
//! This file used to hold golden wire tests pinning the exact JSON bytes
//! jadelang sent to the inference daemon. There is no wire left to pin: the
//! daemon and its socket are gone, and a request now reaches a provider package
//! as an in-process `InferRequest` struct. What is worth guarding moved with it —
//! the shape of that struct, and whether it still matches the shared definition
//! both this repo and dovata are written against.

use crate::llm::{InferenceRequest, REQUEST_TYPE, provider_backend::request_value};
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

// ── Decoding a response ──────────────────────────────────────────────────────
//
// A provider returns an array of frames, each written either as a struct (whose
// type name is the frame name) or as a dict carrying that name under `"type"`.
// Both forms decode the same. Anything else raises, which is the change: the
// decoder used to skip what it could not read, so a provider that renamed `text`
// or wrote `"token"` lowercase produced an empty reply and no error anywhere —
// the model appearing to have said nothing.

use crate::llm::provider_backend::decode_frames;
use jade_runtime::coll::DictObj;

fn dict_frame(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut d = DictObj::new();
    for (k, v) in pairs {
        d.insert((*k).to_owned(), v.clone());
    }
    VmValue::dict(d)
}

fn struct_frame(type_name: &str, pairs: &[(&str, VmValue)]) -> VmValue {
    let mut obj = jade_runtime::coll::StructObj::<VmValue>::new(type_name);
    for (k, v) in pairs {
        obj.set_field(k, v.clone());
    }
    VmValue::Struct(std::sync::Arc::new(parking_lot::Mutex::new(obj)))
}

fn frames(items: Vec<VmValue>) -> VmValue {
    let mut arr = jade_runtime::coll::ArrayObj::<VmValue>::new();
    for v in items {
        arr.push(v);
    }
    VmValue::Array(std::sync::Arc::new(parking_lot::Mutex::new(arr)))
}

fn s(text: &str) -> VmValue {
    VmValue::Str(text.to_owned().into())
}

fn decode(items: Vec<VmValue>) -> crate::frontend::error::Result<String> {
    decode_frames(frames(items), Span { line: 1, col: 1 })
}

use crate::frontend::error::Span;

/// A reply is its `Token`s concatenated in order; `Done` adds nothing.
#[test]
fn tokens_concatenate_in_order() {
    let out = decode(vec![
        dict_frame(&[("type", s("Token")), ("text", s("hel"))]),
        dict_frame(&[("type", s("Token")), ("text", s("lo"))]),
        dict_frame(&[("type", s("Done")), ("tokens_used", VmValue::Int(2))]),
    ]);
    assert_eq!(out.unwrap(), "hello");
}

/// The struct form decodes identically — its type name is the frame name, so no
/// `"type"` field is needed or expected.
#[test]
fn the_struct_form_decodes_the_same_as_the_dict_form() {
    let out = decode(vec![
        struct_frame("Token", &[("text", s("hel"))]),
        struct_frame("Token", &[("text", s("lo"))]),
        struct_frame("Done", &[("tokens_used", VmValue::Int(2))]),
    ]);
    assert_eq!(out.unwrap(), "hello");
}

/// A provider may mix the two while it migrates.
#[test]
fn the_two_forms_mix_freely() {
    let out = decode(vec![
        struct_frame("Meta", &[("provider", s("anthropic"))]),
        dict_frame(&[("type", s("Token")), ("text", s("hi"))]),
        struct_frame("Done", &[("tokens_used", VmValue::Int(1))]),
    ]);
    assert_eq!(out.unwrap(), "hi");
}

/// `Meta` and `Json` are accepted and contribute no text. Before the frames were
/// declared, they were accepted only because *everything* was.
#[test]
fn meta_and_json_carry_no_text() {
    let out = decode(vec![
        dict_frame(&[("type", s("Meta")), ("provider", s("openai"))]),
        dict_frame(&[("type", s("Json")), ("json", s(r#"{"tool":"x"}"#))]),
    ]);
    assert_eq!(out.unwrap(), "");
}

/// An `Error` frame becomes a catchable inference error carrying its message.
#[test]
fn an_error_frame_raises_with_its_message() {
    let e = decode(vec![dict_frame(&[("type", s("Error")), ("message", s("model not loaded"))])])
        .expect_err("an Error frame must raise");
    assert!(format!("{e:?}").contains("model not loaded"), "got {e:?}");
}

/// A miscased tag is the realistic drift, and the one that used to be silent.
#[test]
fn an_unknown_frame_type_raises_instead_of_being_skipped() {
    let e = decode(vec![dict_frame(&[("type", s("token")), ("text", s("hi"))])])
        .expect_err("an unknown frame type must raise");
    let msg = format!("{e:?}");
    assert!(msg.contains("token"), "the message must name the bad type: {msg}");
    assert!(msg.contains("Token"), "the message must list what was expected: {msg}");
}

/// A renamed payload key is the other half of the same drift.
#[test]
fn a_token_without_string_text_raises() {
    let e = decode(vec![dict_frame(&[("type", s("Token")), ("content", s("hi"))])])
        .expect_err("a Token with no `text` must raise");
    assert!(format!("{e:?}").contains("text"), "got {e:?}");

    let e = decode(vec![dict_frame(&[("type", s("Token")), ("text", VmValue::Int(7))])])
        .expect_err("a Token whose text is not a string must raise");
    assert!(format!("{e:?}").contains("not a string"), "got {e:?}");
}

/// A frame that is neither a struct nor a tagged dict has no type to read.
#[test]
fn an_untyped_frame_raises() {
    for bad in [s("hello"), VmValue::Int(3), VmValue::Nil] {
        decode(vec![bad.clone()]).expect_err(&format!("{bad:?} is not a frame"));
    }
    decode(vec![dict_frame(&[("text", s("hi"))])])
        .expect_err("a dict with no `type` is not a frame");
}

/// A reply that is not an array at all names what arrived.
#[test]
fn a_non_array_return_names_what_it_got() {
    let e = decode_frames(s("hello"), Span { line: 1, col: 1 })
        .expect_err("a bare string is not a frame array");
    assert!(format!("{e:?}").contains("str"), "got {e:?}");
}

/// Zero frames is a legal empty reply, not a violation: a model may produce no
/// tokens, and the wire protocol has always allowed a `Done` with a zero count.
#[test]
fn an_empty_frame_array_is_an_empty_reply() {
    assert_eq!(decode(vec![]).unwrap(), "");
}

// ── Tripwire: the shared definition vs. what the compiler emits ──────────────
//
// `src/protocol/jade/infer.jde` is the one definition dovata's
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
use crate::llm::{
    FRAME_ERROR, FRAME_ERROR_MESSAGE, FRAME_TOKEN, FRAME_TOKEN_TEXT, FRAME_TYPES, REQUEST_FIELDS,
};

/// Path to the shared definition, relative to the crate root.
const SHARED_DEF: &str = "src/protocol/jade/infer.jde";

/// Every `struct` the shared file declares, as `(name, field names)`, in
/// declaration order.
///
/// A missing file is a hard failure, never a skip: the submodule being absent is
/// exactly when drift goes unnoticed, so a quiet pass would defeat the tripwire.
fn structs_in_shared_definition() -> Vec<(String, Vec<String>)> {
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

    program
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::StructDef { name, fields, .. } => {
                Some((name.clone(), fields.iter().map(|f| f.name().to_owned()).collect()))
            }
            _ => None,
        })
        .collect()
}

/// The fields of one struct in the shared file.
fn shared_fields(type_name: &str) -> Vec<String> {
    structs_in_shared_definition()
        .into_iter()
        .find(|(name, _)| name == type_name)
        .unwrap_or_else(|| panic!("the shared definition declares no `struct {type_name}`"))
        .1
}

#[test]
fn request_fields_match_the_shared_definition() {
    let shared = shared_fields(REQUEST_TYPE);
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

/// Every frame the shared file declares is one this language decodes, and every
/// frame it decodes is one the shared file declares.
///
/// Both halves matter. A frame declared there but unknown here now *raises*
/// rather than being skipped, so a provider using it would break; a frame known
/// here but absent there is a shape nothing has agreed to.
#[test]
fn frame_types_match_the_shared_definition() {
    let declared: Vec<String> = structs_in_shared_definition()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name != REQUEST_TYPE)
        .collect();
    let ours: Vec<String> = FRAME_TYPES.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        declared, ours,
        "\n{SHARED_DEF} and llm::FRAME_TYPES disagree.\n\
         shared definition: {declared:?}\n\
         compiler decodes:  {ours:?}\n\
         An undeclared frame raises now, so a provider emitting one fails loudly \
         instead of silently. Update whichever side is behind."
    );
}

/// The payload fields the language actually reads must exist on the frames it
/// reads them from. It ignores the rest of every payload, so those are left to
/// the shared file alone.
#[test]
fn the_frame_payloads_we_read_are_declared() {
    assert!(
        shared_fields(FRAME_TOKEN).contains(&FRAME_TOKEN_TEXT.to_owned()),
        "{SHARED_DEF} declares no `{FRAME_TOKEN_TEXT}` on `{FRAME_TOKEN}`, which is the \
         field every reply is assembled from"
    );
    assert!(
        shared_fields(FRAME_ERROR).contains(&FRAME_ERROR_MESSAGE.to_owned()),
        "{SHARED_DEF} declares no `{FRAME_ERROR_MESSAGE}` on `{FRAME_ERROR}`, which is what \
         a failed inference reports"
    );
}

/// Slice one C function's body out of `infer.c`, so a name found in it is a name
/// that function actually uses.
fn c_function_body(name: &str) -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime_aot/infer/infer.c");
    let src = std::fs::read_to_string(&path).expect("cannot read infer.c");
    let start = src.find(name).unwrap_or_else(|| panic!("{name} is gone from infer.c"));
    let rest = &src[start..];
    let end = rest.find("\n}").unwrap_or_else(|| panic!("cannot find the end of {name}"));
    rest[..end].to_owned()
}

/// The AOT engine builds the same request in C, where a Rust constant cannot
/// reach. Check its source text instead: every field must appear as a key
/// literal in `provider_request`, so the two engines cannot send different
/// shapes to the same package.
#[test]
fn the_c_emitter_names_every_request_field() {
    let body = c_function_body("provider_request");
    for field in REQUEST_FIELDS {
        assert!(
            body.contains(&format!("\"{field}\"")),
            "provider_request in infer.c never names the `{field}` field, so a \
             compiled binary sends a different request than the VM does"
        );
    }
}

/// Same check on the way back: the C decoder must name every frame type, or a
/// compiled binary would raise on a frame the VM accepts.
#[test]
fn the_c_decoder_names_every_frame_type() {
    let body = c_function_body("provider_infer_text");
    for ty in FRAME_TYPES {
        assert!(
            body.contains(&format!("\"{ty}\"")),
            "provider_infer_text in infer.c never names the `{ty}` frame, so a compiled \
             binary raises on a frame the VM decodes fine"
        );
    }
    for field in [FRAME_TOKEN_TEXT, FRAME_ERROR_MESSAGE] {
        assert!(
            body.contains(&format!("\"{field}\"")),
            "provider_infer_text in infer.c never reads the `{field}` payload, so the two \
             engines read different things out of the same frame"
        );
    }
}
