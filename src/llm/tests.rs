//! Consolidated unit tests for the `llm` module.
//!
//! Relocated from inline `#[cfg(test)] mod tests` blocks (model_profile, jaded)
//! plus new coverage for the API backends (anthropic, openai) and the built-in
//! package descriptor (pkg). The API-backend tests never touch the network —
//! they exercise only the pure pieces: request-body construction, header/auth
//! assembly, response/error parsing, model-name selection, and constructor paths.

// ── model_profile ─────────────────────────────────────────────────────────────
mod model_profile {
    use crate::llm::model_profile::*;

    #[test]
    fn qwen_profile_selected_by_name_and_glob() {
        assert_eq!(select("Qwen3-Coder-30B").unwrap().tool_call.open, "<tool_call>");
        assert_eq!(select("Qwen3-Coder-30B-Instruct").unwrap().tool_call.close, "</tool_call>");
        assert_eq!(select("qwen3-coder-7b").unwrap().tool_call.name_field, "name");
        assert!(select("some-other-model").is_none());
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("Qwen3-Coder*", "Qwen3-Coder-30B"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("Qwen3*", "Llama-3"));
    }

    #[test]
    fn find_tool_call_extracts_name_and_args() {
        let p = QWEN3_CODER_30B;
        let text = r#"Sure, let me check. <tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call> Done."#;
        let tc = p.find_tool_call(text).expect("tool call found");
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.args, r#"{"name": "get_weather", "arguments": {"city": "SF"}}"#);
    }

    #[test]
    fn find_tool_call_none_when_no_open_delimiter() {
        let p = QWEN3_CODER_30B;
        assert!(p.find_tool_call("just some plain text, no tools here").is_none());
    }

    #[test]
    fn find_tool_call_missing_name_field_yields_empty_name() {
        let p = QWEN3_CODER_30B;
        let tc = p.find_tool_call(r#"<tool_call>{"arguments":{}}</tool_call>"#).unwrap();
        assert_eq!(tc.name, "");
        assert_eq!(tc.args, r#"{"arguments":{}}"#);
    }

    #[test]
    fn find_tool_call_unterminated_uses_remainder() {
        // open present, no close → body is the remainder (defensive recovery).
        let p = QWEN3_CODER_30B;
        let tc = p.find_tool_call(r#"<tool_call>{"name":"t","arguments":{}}"#).unwrap();
        assert_eq!(tc.name, "t");
    }

    #[test]
    fn find_all_tool_calls_returns_every_call_in_order() {
        let p = QWEN3_CODER_30B;
        let text = concat!(
            "prose ",
            r#"<tool_call>{"name":"a","arguments":{}}</tool_call>"#,
            " between ",
            r#"<tool_call>{"name":"b","arguments":{"x":1}}</tool_call>"#,
            " end",
        );
        let calls = p.find_all_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
        assert_eq!(calls[1].args, r#"{"name":"b","arguments":{"x":1}}"#);
    }

    #[test]
    fn find_all_tool_calls_empty_when_none() {
        assert!(QWEN3_CODER_30B.find_all_tool_calls("no tools here").is_empty());
    }

    #[test]
    fn find_all_tool_calls_recovers_calls_with_missing_closes() {
        // The model emits two calls but drops the </tool_call> on the first.
        // Slicing to the next close would make call 1 swallow call 2's open;
        // taking the balanced object after each open recovers both.
        let p = QWEN3_CODER_30B;
        let text = concat!(
            r#"<tool_call>{"name":"a","arguments":{"k":"}"}}"#, // brace inside a string value
            r#"<tool_call>{"name":"b","arguments":{"x":1}}</tool_call>"#,
        );
        let calls = p.find_all_tool_calls(text);
        assert_eq!(calls.len(), 2, "both calls recovered despite the missing close");
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[0].args, r#"{"name":"a","arguments":{"k":"}"}}"#);
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn find_all_tool_calls_recovers_bare_json_when_no_delimiters() {
        // No <tool_call> wrappers — the daemon stripped the anchors. Two calls
        // emitted back-to-back as bare JSON must both be recovered, in order.
        let p = QWEN3_CODER_30B;
        let text = concat!(
            r#"{"name":"a","arguments":{"x":1}}"#,
            "\n",
            r#"{"name":"b","arguments":{"y":"}"}}"#, // brace inside a string value
        );
        let calls = p.find_all_tool_calls(text);
        assert_eq!(calls.len(), 2, "both bare calls recovered");
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
        // The `}` inside the string value must not split the second object.
        assert_eq!(calls[1].args, r#"{"name":"b","arguments":{"y":"}"}}"#);
    }

    #[test]
    fn find_all_tool_calls_recovers_bare_json_array() {
        let p = QWEN3_CODER_30B;
        let text = r#"[{"name":"a","arguments":{}}, {"name":"b","arguments":{}}]"#;
        let calls = p.find_all_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn bare_fallback_ignores_non_tool_json_objects() {
        // Bare JSON objects without the name field aren't tool calls.
        let p = QWEN3_CODER_30B;
        assert!(p.find_all_tool_calls(r#"{"city":"SF"} {"value":42}"#).is_empty());
        assert!(p.find_tool_call(r#"{"city":"SF"}"#).is_none());
    }

    #[test]
    fn delimited_scan_takes_precedence_over_bare_fallback() {
        // When delimiters ARE present, behavior is unchanged: the bare fallback
        // must not also kick in and double-count.
        let p = QWEN3_CODER_30B;
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call>"#;
        let calls = p.find_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "a");
    }

    #[test]
    fn find_tool_call_recovers_first_bare_json() {
        let p = QWEN3_CODER_30B;
        let tc = p
            .find_tool_call(r#"sure: {"name":"get_weather","arguments":{"city":"SF"}} done"#)
            .expect("bare call recovered");
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.args, r#"{"name":"get_weather","arguments":{"city":"SF"}}"#);
    }
}

// ── jaded (unix-only wire encoding) ─────────────────────────────────────────
mod jaded {
    use crate::llm::jaded::*;
    use crate::llm::InferenceRequest;

    // Golden wire tests: lock the exact bytes jadelang sends to the daemon so any
    // drift from the documented protocol (design/llm-package-1.1.12.md) fails CI.
    // The daemon mirrors these field names with serde(default); what this pins is
    // field ORDER and the presence of `keep_anchors` / `trust`.
    //
    // The 4-byte length prefix is not checked here: framing moved to the shared
    // transport, which adds it for every request. `jade_runtime::infer::conn`
    // covers it, against a real socket rather than a slice.

    #[test]
    fn encode_minimal_request_golden() {
        let req = InferenceRequest {
            prompt: "hi".into(),
            model: "m".into(),
            max_tokens: 10,
            ..Default::default()
        };
        let json = encode_request(&req).unwrap();
        assert_eq!(
            std::str::from_utf8(&json).unwrap(),
            r#"{"prompt":"hi","model":"m","max_tokens":10,"keep_anchors":false,"trust":0}"#,
        );
    }

    #[test]
    fn encode_full_request_golden() {
        // Tool delimiters come from the model profile, not inline literals — the
        // single source of truth for this model's tool format.
        let profile = crate::llm::model_profile::QWEN3_CODER_30B;
        let req = InferenceRequest {
            prompt: "p".into(),
            model: profile.model.into(),
            max_tokens: 64,
            grammar: Some(r#"root ::= "{" [^}]* "}""#.into()),
            anchor: Some(profile.tool_call.open.into()),
            stop_anchor: Some(profile.tool_call.close.into()),
            keep_anchors: true,
            trust: 1,
        };
        let json = encode_request(&req).unwrap();
        assert_eq!(
            std::str::from_utf8(&json).unwrap(),
            r#"{"prompt":"p","model":"Qwen3-Coder-30B","max_tokens":64,"grammar":"root ::= \"{\" [^}]* \"}\"","anchor":"<tool_call>","stop_anchor":"</tool_call>","keep_anchors":true,"trust":1}"#,
        );
    }
}

// ── anthropic (no network — pure request/response logic) ──────────────────────
mod anthropic {
    use crate::llm::anthropic::AnthropicBackend;

    // These mirror the pure pieces of `AnthropicBackend::infer` so behavior is
    // pinned without a real HTTP round-trip: request-body shape, model selection,
    // response text extraction, token accounting, and error-body formatting.

    /// Build the JSON request body exactly as `infer` does.
    fn build_body(model: &str, max_tokens: u32, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{ "role": "user", "content": prompt }],
        })
    }

    /// Model selection rule from `infer`: empty request model falls back to default.
    fn select_model<'a>(req_model: &'a str, default_model: &'a str) -> &'a str {
        if req_model.is_empty() { default_model } else { req_model }
    }

    #[test]
    fn constructor_succeeds_with_valid_key() {
        let b = AnthropicBackend::new("sk-test", "claude-opus-4-8", Some(4));
        assert!(b.is_ok(), "constructor should build a client");
    }

    #[test]
    fn constructor_succeeds_without_parallel_limit() {
        assert!(AnthropicBackend::new("sk-test", "claude-3", None).is_ok());
    }

    #[test]
    fn empty_key_is_still_accepted_by_constructor() {
        // The constructor only builds an HTTP client; auth is validated by the
        // server at call time, so an empty key is not a construction-time error.
        assert!(AnthropicBackend::new("", "claude-3", None).is_ok());
    }

    #[test]
    fn model_selection_prefers_request_over_default() {
        assert_eq!(select_model("claude-request", "claude-default"), "claude-request");
        assert_eq!(select_model("", "claude-default"), "claude-default");
    }

    #[test]
    fn request_body_has_messages_and_max_tokens() {
        let body = build_body("claude-3", 128, "hello");
        assert_eq!(body["model"], "claude-3");
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn response_text_extraction_and_trim() {
        // Mirrors: json["content"][0]["text"].as_str()?.trim()
        let json: serde_json::Value = serde_json::json!({
            "content": [{ "text": "  hi there  " }],
            "usage": { "input_tokens": 5, "output_tokens": 7 },
        });
        let text = json["content"][0]["text"].as_str().unwrap().trim();
        assert_eq!(text, "hi there");
    }

    #[test]
    fn token_accounting_sums_input_and_output() {
        // Mirrors: usage.input_tokens + usage.output_tokens
        let json: serde_json::Value = serde_json::json!({
            "usage": { "input_tokens": 5, "output_tokens": 7 },
        });
        let tokens_used = (json["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + json["usage"]["output_tokens"].as_u64().unwrap_or(0)) as i64;
        assert_eq!(tokens_used, 12);
    }

    #[test]
    fn token_accounting_defaults_to_zero_when_missing() {
        let json: serde_json::Value = serde_json::json!({ "content": [{ "text": "x" }] });
        let tokens_used = (json["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + json["usage"]["output_tokens"].as_u64().unwrap_or(0)) as i64;
        assert_eq!(tokens_used, 0);
    }

    #[test]
    fn malformed_response_has_no_text() {
        // Missing content array → as_str() is None → infer would raise.
        let json: serde_json::Value = serde_json::json!({ "error": "boom" });
        assert!(json["content"][0]["text"].as_str().is_none());
    }
}

// ── openai (no network — pure request/response logic) ─────────────────────────
mod openai {
    use crate::llm::openai::OpenAiBackend;

    /// Build the JSON request body exactly as `infer` does (note: no max_tokens).
    fn build_body(model: &str, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
        })
    }

    fn select_model<'a>(req_model: &'a str, default_model: &'a str) -> &'a str {
        if req_model.is_empty() { default_model } else { req_model }
    }

    /// Auth header assembly from `infer`.
    fn auth_header(api_key: &str) -> String {
        format!("Bearer {}", api_key)
    }

    #[test]
    fn constructor_succeeds_with_valid_key() {
        assert!(OpenAiBackend::new("sk-test", "gpt-4o", Some(2)).is_ok());
    }

    #[test]
    fn constructor_succeeds_without_parallel_limit() {
        assert!(OpenAiBackend::new("sk-test", "gpt-4o", None).is_ok());
    }

    #[test]
    fn empty_key_is_still_accepted_by_constructor() {
        assert!(OpenAiBackend::new("", "gpt-4o", None).is_ok());
    }

    #[test]
    fn model_selection_prefers_request_over_default() {
        assert_eq!(select_model("gpt-request", "gpt-default"), "gpt-request");
        assert_eq!(select_model("", "gpt-default"), "gpt-default");
    }

    #[test]
    fn auth_header_is_bearer_prefixed() {
        assert_eq!(auth_header("sk-abc"), "Bearer sk-abc");
    }

    #[test]
    fn request_body_shape() {
        let body = build_body("gpt-4o", "hi");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
        // OpenAI body carries no max_tokens field (unlike Anthropic).
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn response_text_extraction_and_trim() {
        // Mirrors: json["choices"][0]["message"]["content"].as_str()?.trim()
        let json: serde_json::Value = serde_json::json!({
            "choices": [{ "message": { "content": "  answer  " } }],
            "usage": { "total_tokens": 42 },
        });
        let text = json["choices"][0]["message"]["content"].as_str().unwrap().trim();
        assert_eq!(text, "answer");
    }

    #[test]
    fn token_accounting_reads_total_tokens() {
        let json: serde_json::Value = serde_json::json!({ "usage": { "total_tokens": 42 } });
        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0) as i64;
        assert_eq!(tokens_used, 42);
    }

    #[test]
    fn token_accounting_defaults_to_zero() {
        let json: serde_json::Value = serde_json::json!({ "choices": [] });
        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0) as i64;
        assert_eq!(tokens_used, 0);
    }

    #[test]
    fn malformed_response_has_no_text() {
        let json: serde_json::Value = serde_json::json!({ "error": "bad" });
        assert!(json["choices"][0]["message"]["content"].as_str().is_none());
    }
}

// ── pkg (LLM_PKG descriptor + NativeFnId mapping) ─────────────────────────────
mod pkg {
    use crate::llm::pkg::LLM_PKG;
    use crate::compiler::type_infer::TypeContext;
    use crate::vm::{NativeFnId, VmValue};

    #[test]
    fn package_descriptor_names() {
        assert_eq!(LLM_PKG.import_name, "llm");
        assert_eq!(LLM_PKG.global_name, "llm");
    }

    /// Every `llm.*` function is stateful, so the package carries no pure fns.
    /// A stray entry here would be a `BuiltinFn` shadowed by its native at
    /// dispatch — dead weight that reads like a real implementation.
    #[test]
    fn package_declares_no_pure_fns() {
        assert!(LLM_PKG.fns.is_empty(), "llm has no pure functions");
    }

    #[test]
    fn register_types_defines_llm_without_panicking() {
        // No public read path on TypeContext; assert the registration runs and
        // seeds the `llm` binding without error (smoke coverage of the fn ptr).
        let mut ctx = TypeContext::new();
        (LLM_PKG.register_types)(&mut ctx);
    }

    #[test]
    fn vm_dict_value_maps_every_fn_to_its_native_id() {
        let dict = LLM_PKG.vm_dict_value();
        let map = match dict {
            VmValue::Dict(m) => m,
            other => panic!("expected Dict, got {other:?}"),
        };

        let expected = [
            ("set_max_tokens", NativeFnId::LlmSetMaxTokens),
            ("count_tokens", NativeFnId::LlmCountTokens),
            ("total_tokens", NativeFnId::LlmTotalTokens),
            ("keep_anchors", NativeFnId::LlmKeepAnchors),
            ("model", NativeFnId::LlmModel),
            ("profile", NativeFnId::LlmProfile),
            ("health", NativeFnId::LlmHealth),
            ("find_tool_call", NativeFnId::LlmFindToolCall),
            ("find_tool_calls", NativeFnId::LlmFindToolCalls),
            ("tool_grammar", NativeFnId::LlmToolGrammar),
        ];
        assert_eq!(map.len(), expected.len(), "dict must map exactly ten entries");
        for (name, id) in expected {
            match map.get(name) {
                Some(VmValue::NativeFn(got)) => assert_eq!(*got, id, "wrong NativeFnId for {name}"),
                other => panic!("entry {name} was not a NativeFn: {other:?}"),
            }
        }
    }

}
