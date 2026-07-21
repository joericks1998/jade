use std::sync::Arc;

use crate::frontend::error::{JadeError, Result, Span};

pub mod anthropic;
pub mod jaded;
pub mod model_profile;
pub mod openai;
/// The `use llm` built-in package (`LLM_PKG`) — registered in `crate::builtins`,
/// but housed here beside the inference backend it wraps.
pub mod pkg;

#[cfg(test)]
mod tests;

/// A request sent to an inference backend — the wire type itself.
///
/// This was a hand-written struct whose doc comment claimed it "mirrors the
/// daemon's `jade-protocol::InferenceRequest` field-for-field". It did not: it
/// was missing `count_only`, `stats_only`, `health_only`, and `rlm`, so the
/// three operations needing those flags built their JSON ad hoc and bypassed
/// the struct entirely — which is exactly the drift the comment denied.
///
/// The protocol now lives in one repository that both this language and the
/// inference daemon depend on, so there is nothing left to keep in sync. Fields
/// are documented on the type itself.
pub use ovata_infer_protocol::InferenceRequest;

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
    /// Only implemented by `JadedBackend`; API backends return `None`.
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
        // Same field set as the shared `ovata_infer_protocol::Health` so `llm.health()`
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
        "jade"      => Ok(Arc::new(jaded::JadedBackend::new())),
        other => Err(JadeError::InferenceError {
            message: format!("unknown provider '{}' — expected 'anthropic', 'openai', or 'jade'", other),
            span: Span { line: 0, col: 0 },
        }),
    }
}

/// Select the inference backend automatically.
///
/// Priority: the daemon socket (`JADE_LLM_SOCK`, else `$HOME/.jade/llm.sock`) →
/// API key from `~/.jade/config.toml`. Returns `None` if neither is available.
///
/// The socket path comes from [`jaded::sock_path`], the same function the client
/// connects through. It used to be spelled out again here, so `JADE_LLM_SOCK`
/// was honoured when *connecting* but not when deciding whether a daemon
/// existed at all: pointing the VM at a custom socket left it reporting "no
/// inference backend available" while the compiled path talked to that socket
/// happily. On a developer machine with a real `~/.jade/llm.sock` lying around
/// the mistake is invisible.
pub fn select_backend(config: &crate::config::JadeConfig) -> Option<Arc<dyn InferenceBackend>> {
    if std::path::Path::new(&jaded::sock_path()).exists() {
        return Some(Arc::new(jaded::JadedBackend::new()));
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
