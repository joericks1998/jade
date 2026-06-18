use std::sync::Arc;

use crate::frontend::error::{JadeError, Result, Span};

pub mod anthropic;
#[cfg(unix)]
pub mod jade_os;
pub mod model_profile;
pub mod openai;

/// A request sent to an inference backend.
///
/// Mirrors the daemon's `jade-protocol::InferenceRequest` field-for-field (the
/// socket is the contract — see `design/llm-package-1.1.12.md`). `Default` lets
/// construction sites set only the fields they care about and lets the wire
/// struct grow without churning every call site.
#[derive(Clone, Default)]
pub struct InferenceRequest {
    pub prompt: String,
    pub model: String,
    pub max_tokens: u32,
    /// GBNF grammar string for constrained decoding; `None` = unconstrained.
    /// Passed through to the inference daemon; ignored by API-based backends.
    pub grammar: Option<String>,
    /// Anchor string; if set, grammar enforcement begins only after the model emits this string.
    /// Passed through to the inference daemon; ignored by API-based backends.
    pub anchor: Option<String>,
    /// Stop anchor; if set, generation stops and the stop string is stripped when it appears.
    /// Passed through to the inference daemon; ignored by API-based backends.
    pub stop_anchor: Option<String>,
    /// When true, the daemon makes the `stop_anchor` boundary observable in-band
    /// (it is not stripped, and is synthesized at span close if the model retired
    /// the grammar without emitting it) — letting a client delimit an anchored
    /// span by pure string parsing. `false` = legacy strip behavior.
    /// Passed through to the inference daemon; ignored by API-based backends.
    pub keep_anchors: bool,
    /// Prompt provenance: `0` TRUSTED (program-author-controlled), `1` TAINTED
    /// (derived from an LLM, network, or other untrusted source). The daemon logs
    /// it; runtime enforcement lives in the C runtime. Defaults to TRUSTED.
    pub trust: u8,
}

/// A successful response from an inference backend.
pub struct InferenceResponse {
    pub text: String,
    pub tokens_used: i64,
}

/// Shared interface for any LLM inference provider.
#[async_trait::async_trait]
pub trait InferenceBackend: Send + Sync {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse>;

    /// Returns the model name reported by the backend after inference, if available.
    /// Only implemented by `JadeOsBackend`; API backends return `None`.
    fn reported_model_name(&self) -> Option<String> { None }

    /// Count the tokens in `prompt` without running generation.
    /// Falls back to character-count estimate for backends that don't support it.
    async fn count_tokens(&self, prompt: &str, _span: Span) -> Result<i64> {
        Ok((prompt.len() / 4) as i64)
    }

    /// Return the daemon-wide cumulative token counter.
    /// Returns 0 for backends that don't track it (API backends).
    async fn total_tokens(&self, _span: Span) -> Result<i64> {
        Ok(0)
    }

    /// Return a daemon health snapshot as a JSON object (see
    /// `design/llm-package-1.1.12.md` §2.3 — `{status, model, model_loaded, …}`).
    /// Backends that aren't the jade daemon synthesize a minimal `ok` snapshot.
    async fn health(&self, _span: Span) -> Result<serde_json::Value> {
        // Same field set as the daemon's `jade-protocol::Health` so `llm.health()`
        // returns a consistent dict shape regardless of backend.
        Ok(serde_json::json!({
            "status": "ok",
            "model": self.reported_model_name().unwrap_or_default(),
            "model_loaded": true,
            "uptime_secs": 0,
            "protocol_version": 0,
        }))
    }

    /// Stream tokens as they arrive. Returns a channel receiver and a join handle
    /// that resolves to `tokens_used` when the stream is exhausted.
    /// Default: calls `infer` and sends the full response as one token.
    async fn infer_stream(
        &self,
        req: InferenceRequest,
        span: Span,
    ) -> Result<(tokio::sync::mpsc::Receiver<String>, tokio::task::JoinHandle<Result<i64>>)> {
        let resp = self.infer(req, span).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let tokens = resp.tokens_used;
        let handle = tokio::spawn(async move {
            let _ = tx.send(resp.text).await;
            Ok(tokens)
        });
        Ok((rx, handle))
    }
}

/// Build the appropriate backend for the given provider string.
/// Returns an error for unrecognized provider names.
pub fn build_backend(
    provider: &str,
    api_key: &str,
    model: &str,
    max_parallel: Option<usize>,
) -> Result<Arc<dyn InferenceBackend>> {
    match provider {
        "openai"    => Ok(Arc::new(openai::OpenAiBackend::new(api_key, model, max_parallel)?)),
        "anthropic" => Ok(Arc::new(anthropic::AnthropicBackend::new(api_key, model, max_parallel)?)),
        "jade"      => {
            #[cfg(unix)]
            { return Ok(Arc::new(jade_os::JadeOsBackend::new())); }
            #[cfg(not(unix))]
            return Err(JadeError::InferenceError {
                message: "jade-os backend is not supported on Windows".to_owned(),
                span: Span { line: 0, col: 0 },
            });
        }
        other => Err(JadeError::InferenceError {
            message: format!("unknown provider '{}' — expected 'anthropic', 'openai', or 'jade'", other),
            span: Span { line: 0, col: 0 },
        }),
    }
}

/// Select the inference backend automatically.
///
/// Priority: `$HOME/.jade/llm.sock` (inference daemon) → API key from `~/.jade/config.toml`.
/// Returns `None` if neither is available.
pub fn select_backend(config: &crate::config::JadeConfig) -> Option<Arc<dyn InferenceBackend>> {
    #[cfg(unix)]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        if std::path::Path::new(&format!("{home}/.jade/llm.sock")).exists() {
            return Some(Arc::new(jade_os::JadeOsBackend::new()));
        }
    }
    config.api_key.as_ref()
        .and_then(|key| build_backend(&config.provider, key, &config.model, config.max_parallel).ok())
}

// ── Mock backend (test builds only) ──────────────────────────────────────────

/// Deterministic mock backend for unit tests. Not available at runtime.
///
/// Heuristics for response selection (sufficient to pass all fixture evals):
///   - Prompt asking for "true or false" / "yes or no" → "true"
///   - Prompt asking for "only the number" / arithmetic → "7"
///   - Otherwise → "mock response"
#[cfg(test)]
pub struct MockBackend {
    /// When non-empty, responses are consumed in FIFO order regardless of heuristics.
    /// Used by unit tests that need precise control.
    pub responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    /// All requests sent to this backend, in order. Lets tests verify grammar constraints.
    pub captured: std::sync::Mutex<Vec<InferenceRequest>>,
}

#[cfg(test)]
impl Default for MockBackend {
    fn default() -> Self {
        MockBackend {
            responses: std::sync::Mutex::new(std::collections::VecDeque::new()),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl MockBackend {
    pub fn new(responses: Vec<&str>) -> Self {
        MockBackend {
            responses: std::sync::Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect()
            ),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn mock_response(prompt: &str) -> String {
        let lower = prompt.to_lowercase();
        if lower.contains("true or false") || lower.contains("yes or no") {
            "true".to_string()
        } else if lower.contains("only the number") || lower.contains("respond with only the number") {
            "7".to_string()
        } else {
            "mock response".to_string()
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl InferenceBackend for MockBackend {
    async fn infer(&self, req: InferenceRequest, _span: Span) -> Result<InferenceResponse> {
        self.captured.lock().unwrap().push(req.clone());
        let text = self.responses.lock().unwrap().pop_front()
            .unwrap_or_else(|| Self::mock_response(&req.prompt));
        Ok(InferenceResponse { text, tokens_used: 10_i64 })
    }
}
