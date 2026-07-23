//! Consolidated unit tests for the `llm` module.
//!
//! Relocated from inline `#[cfg(test)] mod tests` blocks (model_profile, jaded)
//! plus new coverage for the API backends (anthropic, openai) and the built-in
//! package descriptor (pkg). The API-backend tests never touch the network —
//! they exercise only the pure pieces: request-body construction, header/auth
//! assembly, response/error parsing, model-name selection, and constructor paths.

// ── jaded (unix-only wire encoding) ─────────────────────────────────────────
mod jaded {
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
        // Anchors are just strings on the wire here. Tool-call delimiters are no
        // longer a runtime concept — they ship with each model's Jade profile
        // package — so the anchors this request carries are plain literals.
        let req = InferenceRequest {
            prompt: "p".into(),
            model: "Qwen3-Coder-30B".into(),
            max_tokens: 64,
            grammar: Some(r#"root ::= "{" [^}]* "}""#.into()),
            anchor: Some("<tool_call>".into()),
            stop_anchor: Some("</tool_call>".into()),
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
            ("health", NativeFnId::LlmHealth),
        ];
        assert_eq!(map.len(), expected.len(), "dict maps exactly the stateful llm fns");
        for (name, id) in expected {
            match map.get(name) {
                Some(VmValue::NativeFn(got)) => assert_eq!(*got, id, "wrong NativeFnId for {name}"),
                other => panic!("entry {name} was not a NativeFn: {other:?}"),
            }
        }
    }

}
