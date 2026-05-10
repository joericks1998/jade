use std::sync::Arc;

use crate::interpreter::error::{JadeError, Result, Span};

pub mod anthropic;
pub mod jade_os;
pub mod openai;

/// A single message in a conversation history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// A request sent to an inference backend.
pub struct InferenceRequest {
    pub prompt: String,
    pub model: String,
    pub history: Vec<Message>,
    pub max_tokens: u32,
    pub system_prompt: Option<String>,
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
        "jade"      => Ok(Arc::new(jade_os::JadeOsBackend::new())),
        other => Err(JadeError::InferenceError {
            message: format!("unknown provider '{}' — expected 'anthropic', 'openai', or 'jade'", other),
            span: Span { line: 0, col: 0 },
        }),
    }
}

/// Select the inference backend automatically.
///
/// When `/tmp/jade/llm.sock` is present (jade-tree running on JADE OS),
/// `JadeOsBackend` is returned unconditionally — no API key needed.
///
/// Otherwise falls back to whatever provider is configured in `~/.jade/config.toml`.
/// Returns `None` if no socket exists and no API key has been configured.
pub fn select_backend(config: &crate::config::JadeConfig) -> Option<Arc<dyn InferenceBackend>> {
    // JADE_MOCK_LLM=1: return deterministic mock responses for CI / eval testing.
    if std::env::var("JADE_MOCK_LLM").as_deref() == Ok("1") {
        return Some(Arc::new(MockBackend::default()));
    }
    if std::path::Path::new("/tmp/jade/llm.sock").exists() {
        return Some(Arc::new(jade_os::JadeOsBackend::new()));
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
    span: crate::interpreter::error::Span,
) -> crate::interpreter::error::Result<InferenceResponse> {
    let fut = backend.infer(req, span);
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| crate::interpreter::error::JadeError::InferenceError {
                message: format!("failed to create tokio runtime for sync inference: {e}"),
                span: crate::interpreter::error::Span { line: 0, col: 0 },
            })?
            .block_on(fut),
    }
}

// ── Mock backend (JADE_MOCK_LLM=1 and tests) ─────────────────────────────────

/// Deterministic mock backend used for eval testing and CI.
///
/// Heuristics for response selection (sufficient to pass all fixture evals):
///   - Prompt asking for "true or false" / "yes or no" → "true"
///   - Prompt asking for "only the number" / arithmetic → "7"
///   - Otherwise → "mock response"
pub struct MockBackend {
    /// When non-empty, responses are consumed in FIFO order regardless of heuristics.
    /// Used by unit tests that need precise control.
    pub responses: std::sync::Mutex<std::collections::VecDeque<String>>,
}

impl Default for MockBackend {
    fn default() -> Self {
        MockBackend { responses: std::sync::Mutex::new(std::collections::VecDeque::new()) }
    }
}

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

#[async_trait::async_trait]
impl InferenceBackend for MockBackend {
    async fn infer(&self, req: InferenceRequest, _span: Span) -> Result<InferenceResponse> {
        let text = self.responses.lock().unwrap().pop_front()
            .unwrap_or_else(|| Self::mock_response(&req.prompt));
        Ok(InferenceResponse { text, tokens_used: 10_i64 })
    }
}
