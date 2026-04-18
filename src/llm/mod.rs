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
        "openai"    => Ok(Arc::new(openai::OpenAiBackend::new(api_key, model, max_parallel))),
        "anthropic" => Ok(Arc::new(anthropic::AnthropicBackend::new(api_key, model, max_parallel))),
        "jade"      => Ok(Arc::new(jade_os::JadeOsBackend::new())),
        other => Err(JadeError::InferenceError {
            message: format!("unknown provider '{}' — expected 'anthropic', 'openai', or 'jade'", other),
            span: Span { line: 0, col: 0 },
        }),
    }
}

/// Synchronous bridge: run an async `infer` call from a sync context.
///
/// Uses the current tokio runtime if one exists (production, post–Phase 6),
/// otherwise spins up a temporary single-threaded runtime (tests, early phases).
/// This helper is removed in Phase 4 when the VM is fully async.
pub fn infer_sync(
    backend: &dyn InferenceBackend,
    req: InferenceRequest,
    span: crate::interpreter::error::Span,
) -> crate::interpreter::error::Result<InferenceResponse> {
    let fut = backend.infer(req, span);
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(fut),
    }
}

// ── Mock backend for tests ───────────────────────────────────────────────────

#[cfg(test)]
pub struct MockBackend {
    pub responses: std::sync::Mutex<std::collections::VecDeque<String>>,
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
}

#[cfg(test)]
#[async_trait::async_trait]
impl InferenceBackend for MockBackend {
    async fn infer(&self, _req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        if let Some(text) = self.responses.lock().unwrap().pop_front() {
            Ok(InferenceResponse { text, tokens_used: 10_i64 })
        } else {
            Err(crate::interpreter::error::JadeError::InferenceError {
                message: "MockBackend ran out of responses".to_string(),
                span,
            })
        }
    }
}
