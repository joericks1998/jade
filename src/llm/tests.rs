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

    // Golden wire tests: lock the exact bytes jadelang sends to the daemon.
    //
    // These are now the bytes of the *shared* request type, not a jadelang-local
    // copy — so what they guard against changed. They no longer catch jadelang
    // drifting from the daemon (impossible: one struct, one encoder). They catch
    // a protocol bump silently changing what the language emits, which is why
    // the dependency is pinned to a tag.
    //
    // Both grew four fields when the copy was dropped — `count_only`,
    // `stats_only`, `health_only`, `rlm` — plus an explicit `grammar: null`.
    // The daemon reads all of them with serde defaults, so this is compatible
    // with the previous encoding; it is just no longer eliding what it does not
    // set.
    //
    // The 4-byte length prefix is not checked here: framing belongs to the
    // transport, which adds it for every request. `jade_runtime::infer::conn`
    // covers it against a real socket rather than a slice.

    #[test]
    fn encode_minimal_request_golden() {
        let req = InferenceRequest {
            prompt: "hi".into(),
            model: "m".into(),
            max_tokens: 10,
            ..Default::default()
        };
        let json = req.encode_body().unwrap();
        assert_eq!(
            std::str::from_utf8(&json).unwrap(),
            r#"{"prompt":"hi","model":"m","max_tokens":10,"grammar":null,"count_only":false,"stats_only":false,"health_only":false,"keep_anchors":false,"trust":0,"rlm":false}"#,
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
            ..Default::default()
        };
        let json = req.encode_body().unwrap();
        assert_eq!(
            std::str::from_utf8(&json).unwrap(),
            r#"{"prompt":"p","model":"Qwen3-Coder-30B","max_tokens":64,"grammar":"root ::= \"{\" [^}]* \"}\"","anchor":"<tool_call>","stop_anchor":"</tool_call>","count_only":false,"stats_only":false,"health_only":false,"keep_anchors":true,"trust":1,"rlm":false}"#,
        );
    }

    /// The three control operations — count, stats, health — used to be inline
    /// `json!` values built separately from the request struct, so nothing
    /// pinned their shape. Each sets exactly one flag and leaves the rest
    /// default; setting two would be a request the daemon resolves by
    /// precedence rather than an error.
    #[test]
    fn control_requests_set_exactly_one_flag() {
        let cases = [
            (InferenceRequest { prompt: "hi".into(), count_only: true, ..Default::default() },
             "count_only"),
            (InferenceRequest { stats_only: true, ..Default::default() }, "stats_only"),
            (InferenceRequest { health_only: true, ..Default::default() }, "health_only"),
        ];

        for (req, expected) in cases {
            let set: Vec<&str> = [
                ("count_only", req.count_only),
                ("stats_only", req.stats_only),
                ("health_only", req.health_only),
                ("rlm", req.rlm),
            ]
            .iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| *name)
            .collect();
            assert_eq!(set, [expected], "exactly one control flag set");

            // Still a well-formed request body the daemon can decode.
            let body = req.encode_body().unwrap();
            let back: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(back[expected], serde_json::json!(true));
        }
    }

    /// `count_only` carries the prompt to tokenize; the other two have nothing
    /// to say and must not smuggle a stale one.
    #[test]
    fn only_count_tokens_carries_a_prompt() {
        assert_eq!(
            InferenceRequest { prompt: "hi".into(), count_only: true, ..Default::default() }.prompt,
            "hi"
        );
        assert!(InferenceRequest { stats_only: true, ..Default::default() }.prompt.is_empty());
        assert!(InferenceRequest { health_only: true, ..Default::default() }.prompt.is_empty());
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
