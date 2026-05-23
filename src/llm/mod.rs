use std::sync::Arc;

use crate::frontend::error::{JadeError, Result, Span};

pub mod anthropic;
#[cfg(unix)]
pub mod jade_os;
pub mod openai;

/// A request sent to an inference backend.
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


/// Synchronous bridge: run an async `infer` call from the tree-walk REPL path.
///
/// Uses `block_in_place` when a multi-threaded tokio runtime is active (REPL under
/// `#[tokio::main]`), which yields the thread to the scheduler without panicking.
/// Falls back to a fresh single-threaded runtime in tests or bare sync contexts.
pub fn infer_sync(
    backend: &dyn InferenceBackend,
    req: InferenceRequest,
    span: crate::frontend::error::Span,
) -> crate::frontend::error::Result<InferenceResponse> {
    let fut = backend.infer(req, span);
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| crate::frontend::error::JadeError::InferenceError {
                message: format!("failed to create tokio runtime for sync inference: {e}"),
                span: crate::frontend::error::Span { line: 0, col: 0 },
            })?
            .block_on(fut),
    }
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
}

#[cfg(test)]
impl Default for MockBackend {
    fn default() -> Self {
        MockBackend { responses: std::sync::Mutex::new(std::collections::VecDeque::new()) }
    }
}

#[cfg(test)]
impl MockBackend {
    pub fn new(responses: Vec<&str>) -> Self {
        MockBackend {
            responses: std::sync::Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect()
            ),
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
        let text = self.responses.lock().unwrap().pop_front()
            .unwrap_or_else(|| Self::mock_response(&req.prompt));
        Ok(InferenceResponse { text, tokens_used: 10_i64 })
    }
}
