use std::sync::Arc;

use crate::frontend::error::{Result, Span};

pub mod provider_backend;

#[cfg(test)]
mod tests;

/// Everything the language asks of an inference call.
///
/// This used to be `ovata_infer_protocol::InferenceRequest`, a wire type shared
/// with the inference daemon and serialized onto a Unix socket. Inference is an
/// in-process call into a provider package now, so there is no wire and no
/// shared struct: what remains is the four things a Jade program can actually
/// express. The daemon-era fields are gone with the daemon — `model`,
/// `max_tokens`, `keep_anchors`, and `trust` were already pinned to fixed
/// defaults, the `count_only`/`stats_only`/`health_only` controls lost their
/// callers when the `llm` package was removed, and `rlm` was never set at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InferenceRequest {
    /// The prompt text to complete.
    pub prompt: String,
    /// A GBNF grammar constraining the reply, from a typed dereference
    /// (`?p |> Type`) or an explicit `Grammar.new`.
    pub grammar: Option<String>,
    /// Opens the constrained span; output before it is passed through.
    pub anchor: Option<String>,
    /// Closes the constrained span.
    pub stop_anchor: Option<String>,
}

/// A successful response from an inference provider.
pub struct InferenceResponse {
    pub text: String,
}

/// Interface to an inference provider.
///
/// There is one real implementation, [`provider_backend::ProviderPackageBackend`],
/// which drives the installed provider package in-process. The trait remains as
/// the seam the test [`MockBackend`] implements.
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

/// Select the inference backend: the provider package installed in the active
/// slot (`$HOME/.jade/provider/active/`), loaded in-process.
///
/// This was a two-step fallback — provider package, else the local inference
/// daemon over a Unix socket. The socket is gone: a provider package is a linked
/// library the engine calls directly, so the daemon was a second way to do the
/// same thing with a serialization boundary in the middle.
///
/// Returns `None` when no provider is installed; `?p` then raises
/// [`JadeError::NoInferenceBackend`](crate::frontend::error::JadeError), whose
/// message points at `jade register`.
pub fn select_backend() -> Option<Arc<dyn InferenceBackend>> {
    provider_backend::ProviderPackageBackend::from_registry()
        .map(|b| Arc::new(b) as Arc<dyn InferenceBackend>)
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
