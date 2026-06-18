//! Per-model token & tool formats — the model-specific vocabulary the language
//! layer needs to talk to a given model.
//!
//! ## This is the one place to read off "how does model X spell tool calls?"
//!
//! Tool-call delimiters (`<tool>…</tool>`), the tool-name JSON field, and any
//! special-token spans (e.g. `<think>…</think>`) are properties of the **model**,
//! not of Jade or the wire protocol. They ship with the `.gguf` and differ
//! between models. Keeping them here — as plain data, grouped per model — means
//! nothing downstream hardcodes a `<tool>` literal: it asks the profile.
//!
//! ## Temporary: local mirror of the daemon's `jade-model-profile`
//!
//! The field shape below intentionally mirrors `jade-model-profile::ModelProfile`
//! (in `jade-inference-daemon`) field-for-field, so the eventual swap is
//! mechanical. **The source of truth is JadeOS / the daemon, not this file.**
//! Once profiles are shipped with JadeOS (or fetched from the daemon, whose
//! `Meta` frame already reports the live model name), [`select`] should resolve
//! against that source and this hardcoded table goes away. Until then, this is a
//! small, obvious, swappable stand-in.

/// A model's token/turn-taking vocabulary. Mirrors
/// `jade-model-profile::ModelProfile`.
#[derive(Debug, Clone, Copy)]
pub struct ModelProfile {
    /// Human-readable model name this profile targets (matched against the name
    /// the daemon reports in its `Meta` frame).
    pub model: &'static str,
    /// Glob patterns (against the reported model name) this profile applies to.
    /// `*` matches any sequence; first match wins. Mirrors the daemon's `match`.
    pub match_patterns: &'static [&'static str],
    /// How this model emits a tool invocation.
    pub tool_call: ToolCall,
    /// Additional special-token spans the model may stream (e.g. reasoning).
    pub spans: &'static [Span],
}

/// Delimiters + parsing convention for a tool call the model emits.
#[derive(Debug, Clone, Copy)]
pub struct ToolCall {
    /// Opening delimiter, e.g. `<tool>`.
    pub open: &'static str,
    /// Closing delimiter, e.g. `</tool>`.
    pub close: &'static str,
    /// JSON field inside the call object naming the tool, e.g. `tool_name`.
    pub name_field: &'static str,
}

/// A generic special-token span the model streams (beyond the tool call).
#[derive(Debug, Clone, Copy)]
pub struct Span {
    /// Semantic label, e.g. `reasoning`.
    pub tag: &'static str,
    /// Opening delimiter, e.g. `<think>`.
    pub open: &'static str,
    /// Closing delimiter, e.g. `</think>`.
    pub close: &'static str,
}

/// A tool call located in model output. Mirrors `jade-model-profile`'s
/// `Event::ToolCall`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallMatch {
    /// The `name_field` value lifted from the call JSON (empty if absent or the
    /// body isn't valid JSON).
    pub name: String,
    /// The verbatim, trimmed JSON body found between the delimiters.
    pub args: String,
}

impl ModelProfile {
    /// Find the first tool call in `text`, delimited by this model's
    /// `tool_call.open`/`tool_call.close`.
    ///
    /// Returns `None` when no opening delimiter is present. If the opening
    /// delimiter is found without a matching close, the remainder of the text is
    /// treated as the body — matching the daemon's defensive finish behavior
    /// (`ToolStreamParser::finish`), since with `keep_anchors` a complete
    /// response normally carries both delimiters in-band.
    pub fn find_tool_call(&self, text: &str) -> Option<ToolCallMatch> {
        let start = text.find(self.tool_call.open)? + self.tool_call.open.len();
        let rest = &text[start..];
        let body = match rest.find(self.tool_call.close) {
            Some(end) => &rest[..end],
            None => rest,
        };
        Some(self.match_from_body(body))
    }

    /// Find EVERY tool call in `text`, in order. Empty when none are present.
    /// Each match is delimited by this model's `open`/`close`; a trailing
    /// unterminated call (open without close) is recovered from the remainder.
    pub fn find_all_tool_calls(&self, text: &str) -> Vec<ToolCallMatch> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(open_at) = rest.find(self.tool_call.open) {
            let after = &rest[open_at + self.tool_call.open.len()..];
            let (body, advance) = match after.find(self.tool_call.close) {
                Some(end) => (&after[..end], end + self.tool_call.close.len()),
                None => (after, after.len()),
            };
            out.push(self.match_from_body(body));
            if advance == 0 {
                break; // empty close delimiter would loop forever; defensive.
            }
            rest = &after[advance..];
        }
        out
    }

    /// Build a [`ToolCallMatch`] from a raw delimiter body: trim it and lift the
    /// `name_field` out of the JSON (empty when absent or unparseable).
    fn match_from_body(&self, body: &str) -> ToolCallMatch {
        let args = body.trim().to_owned();
        let name = serde_json::from_str::<serde_json::Value>(&args)
            .ok()
            .and_then(|v| {
                v.get(self.tool_call.name_field)
                    .and_then(|n| n.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        ToolCallMatch { name, args }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Model profiles. One `const` per model — add a model = add a labelled block.
// ─────────────────────────────────────────────────────────────────────────────

/// Qwen3-Coder-30B — native Qwen/Hermes tool calls:
/// `<tool_call>{ "name": …, "arguments": { … } }</tool_call>`, no separate
/// reasoning channel. This is the format the model is trained to emit and that
/// `programs/lib/tools.jde` consumes in production.
///
/// NOTE: the daemon's `profiles/qwen3-coder-30b.toml` currently describes a
/// *steered* `<tool>` / `tool_name` format instead; that profile is the odd one
/// out and should be reconciled with this native convention on the daemon side.
pub const QWEN3_CODER_30B: ModelProfile = ModelProfile {
    model: "Qwen3-Coder-30B",
    match_patterns: &["Qwen3-Coder*", "qwen3-coder*"],
    tool_call: ToolCall {
        open: "<tool_call>",
        close: "</tool_call>",
        name_field: "name",
    },
    spans: &[],
};

/// Every profile this build knows about. First match in [`select`] wins.
pub const PROFILES: &[ModelProfile] = &[QWEN3_CODER_30B];

/// The profile for `model_name` (exact name or any `match` glob), if known.
pub fn select(model_name: &str) -> Option<&'static ModelProfile> {
    PROFILES.iter().find(|p| {
        p.model == model_name || p.match_patterns.iter().any(|pat| glob_match(pat, model_name))
    })
}

/// `*`-glob match (no other metacharacters); ASCII byte comparison. Mirrors the
/// daemon's matcher so profile selection agrees on both sides.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
