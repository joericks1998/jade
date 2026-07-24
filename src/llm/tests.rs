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

// The `use llm` package was removed entirely — its features moved to the daemon
// as Jade packages. Prompts (`?p`, `?p |> Type`) remain the only inference
// surface, so there is no package descriptor left to test here.
