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
pub trait InferenceBackend {
    fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse>;
}

/// Build the appropriate backend for the given provider string.
/// Returns an error for unrecognized provider names.
pub fn build_backend(provider: &str, api_key: &str, model: &str) -> Result<Box<dyn InferenceBackend>> {
    match provider {
        "openai"    => Ok(Box::new(openai::OpenAiBackend::new(api_key, model))),
        "anthropic" => Ok(Box::new(anthropic::AnthropicBackend::new(api_key, model))),
        "jade"      => Ok(Box::new(jade_os::JadeOsBackend::new())),
        other => Err(JadeError::InferenceError {
            message: format!("unknown provider '{}' — expected 'anthropic', 'openai', or 'jade'", other),
            span: Span { line: 0, col: 0 },
        }),
    }
}

// ── Mock backend for tests ───────────────────────────────────────────────────

#[cfg(test)]
pub struct MockBackend {
    pub responses: std::cell::RefCell<std::collections::VecDeque<String>>,
}

#[cfg(test)]
impl MockBackend {
    pub fn new(responses: Vec<&str>) -> Self {
        MockBackend {
            responses: std::cell::RefCell::new(
                responses.into_iter().map(|s| s.to_string()).collect()
            ),
        }
    }
}

#[cfg(test)]
impl InferenceBackend for MockBackend {
    fn infer(&self, _req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        if let Some(text) = self.responses.borrow_mut().pop_front() {
            Ok(InferenceResponse { text, tokens_used: 10_i64 })
        } else {
            Err(crate::interpreter::error::JadeError::InferenceError {
                message: "MockBackend ran out of responses".to_string(),
                span,
            })
        }
    }
}
