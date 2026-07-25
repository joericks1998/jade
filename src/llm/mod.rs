use std::sync::Arc;

use crate::frontend::error::{Result, Span};

pub mod jaded;
pub mod provider_backend;

#[cfg(test)]
mod tests;

/// A request sent to the inference daemon — the wire type itself.
///
/// The protocol lives in one repository (`ovata-infer-protocol`) that both this
/// language and the inference daemon depend on, so there is nothing to keep in
/// sync. Fields are documented on the type itself.
pub use ovata_infer_protocol::InferenceRequest;

/// A successful response from the inference daemon.
pub struct InferenceResponse {
    pub text: String,
}

/// Interface to an inference provider.
///
/// The language speaks to exactly one provider — the inference daemon, over a
/// Unix socket (see [`jaded::JadedBackend`]). The OpenAI and Anthropic HTTP
/// backends that used to live here moved into the daemon: all providers now sit
/// behind the daemon, and the language is a pure wire-protocol client. The trait
/// remains as the seam the test [`MockBackend`] implements.
#[async_trait::async_trait]
pub trait InferenceBackend: Send + Sync {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse>;

    /// Stream tokens as they arrive. Returns a channel receiver and a join handle
    /// that resolves when the stream is exhausted (carrying any transport error).
    /// Default: calls `infer` and sends the full response as one token.
    async fn infer_stream(
        &self,
        req: InferenceRequest,
        span: Span,
    ) -> Result<(tokio::sync::mpsc::Receiver<String>, tokio::task::JoinHandle<Result<()>>)> {
        let resp = self.infer(req, span).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let handle = tokio::spawn(async move {
            let _ = tx.send(resp.text).await;
            Ok(())
        });
        Ok((rx, handle))
    }
}

/// Select the inference backend, in priority order:
///
/// 1. **A configured provider package** — cloud inference (Anthropic/OpenAI) with
///    no daemon and no special hardware. Active when `~/.jade/config.toml` names a
///    provider whose `.so` is installed (see [`crate::providers`]). This is the
///    default door for most machines, which is why it comes first.
/// 2. **The local inference daemon** — the optional upgrade, available when its
///    socket exists (`JADE_LLM_SOCK`, else `$HOME/.jade/llm.sock`).
///
/// Returns `None` when neither is present; `?p` then raises
/// [`JadeError::NoInferenceBackend`](crate::frontend::error::JadeError), whose
/// message points at `jade register`.
pub fn select_backend() -> Option<Arc<dyn InferenceBackend>> {
    if let Some(backend) = provider_backend::ProviderPackageBackend::from_registry() {
        return Some(Arc::new(backend));
    }
    if std::path::Path::new(&jaded::sock_path()).exists() {
        return Some(Arc::new(jaded::JadedBackend::new()));
    }
    None
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
        Ok(InferenceResponse { text })
    }
}
