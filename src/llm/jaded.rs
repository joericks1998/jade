//! JadedBackend — native inference via Unix domain socket.
//!
//! The wire protocol itself lives in [`jade_runtime::infer`], shared with the
//! compiled runtime. What stays here is what is specific to running under the
//! VM: building request bodies, mapping transport failures into catchable
//! `JadeError`s with a source span, and the stop-anchor trimming that the
//! streaming contract requires.
//!
//! Each request gets its own connection. Compiled binaries hold one for the
//! process, but the VM runs `async` prompts concurrently and a single
//! serialized connection would turn those back into a sequence.

use std::sync::{Arc, Mutex};

use jade_runtime::infer::{conn::Conn, InferError, Mode};

use crate::frontend::error::{JadeError, Result, Span};
use super::{InferenceBackend, InferenceRequest, InferenceResponse};

pub(crate) fn sock_path() -> String {
    jade_runtime::infer::sock_path()
}

/// Map a transport-level failure into a Jade error carrying the source span.
///
/// The compiled runtime prints these and exits instead — it has no interpreter
/// to unwind into. Same failures, different reporting, which is why the shared
/// layer returns them rather than deciding.
fn to_jade_error(e: InferError, span: Span) -> JadeError {
    JadeError::InferenceError { message: e.to_string(), span }
}

pub struct JadedBackend {
    sock_path: String,
    reported_model: Arc<Mutex<Option<String>>>,
}

impl JadedBackend {
    pub fn new() -> Self {
        Self {
            sock_path: sock_path(),
            reported_model: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait::async_trait]
impl InferenceBackend for JadedBackend {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let sock_path = self.sock_path.clone();
        let reported_model = Arc::clone(&self.reported_model);
        tokio::task::spawn_blocking(move || {
            Self::infer_blocking(&sock_path, req, span, reported_model)
        })
        .await
        .map_err(|e| JadeError::InferenceError {
            message: format!("spawn_blocking panic: {e}"),
            span,
        })?
    }

    async fn infer_stream(
        &self,
        req: InferenceRequest,
        span: Span,
    ) -> Result<(tokio::sync::mpsc::Receiver<String>, tokio::task::JoinHandle<Result<i64>>)> {
        let sock_path = self.sock_path.clone();
        let reported_model = Arc::clone(&self.reported_model);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let handle = tokio::task::spawn_blocking(move || {
            Self::infer_blocking_stream(&sock_path, req, span, tx, reported_model)
        });
        Ok((rx, handle))
    }

    fn reported_model_name(&self) -> Option<String> {
        self.reported_model.lock().unwrap().clone()
    }

    async fn count_tokens(&self, prompt: &str, span: Span) -> Result<i64> {
        let sock_path = self.sock_path.clone();
        let prompt = prompt.to_owned();
        tokio::task::spawn_blocking(move || Self::count_tokens_blocking(&sock_path, &prompt, span))
            .await
            .map_err(|e| JadeError::InferenceError {
                message: format!("spawn_blocking panic: {e}"),
                span,
            })?
    }

    async fn total_tokens(&self, span: Span) -> Result<i64> {
        let sock_path = self.sock_path.clone();
        tokio::task::spawn_blocking(move || Self::total_tokens_blocking(&sock_path, span))
            .await
            .map_err(|e| JadeError::InferenceError {
                message: format!("spawn_blocking panic: {e}"),
                span,
            })?
    }

    async fn health(&self, span: Span) -> Result<serde_json::Value> {
        let sock_path = self.sock_path.clone();
        tokio::task::spawn_blocking(move || Self::health_blocking(&sock_path, span))
            .await
            .map_err(|e| JadeError::InferenceError {
                message: format!("spawn_blocking panic: {e}"),
                span,
            })?
    }
}

impl JadedBackend {
    /// Run one exchange, recording the model the daemon reported.
    fn exchange(
        sock_path: &str,
        body: &[u8],
        mode: Mode,
        span: Span,
        on_token: Option<&mut dyn FnMut(&[u8])>,
        reported_model: Option<&Arc<Mutex<Option<String>>>>,
    ) -> Result<jade_runtime::infer::Response> {
        let conn = Conn::new(sock_path);
        let resp = conn.request(body, mode, on_token).map_err(|e| to_jade_error(e, span))?;
        if let Some(slot) = reported_model {
            let model = conn.reported_model();
            if !model.is_empty() {
                *slot.lock().unwrap() = Some(model);
            }
        }
        Ok(resp)
    }

    fn infer_blocking(
        sock_path: &str,
        req: InferenceRequest,
        span: Span,
        reported_model: Arc<Mutex<Option<String>>>,
    ) -> Result<InferenceResponse> {
        let body = encode_request(&req).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })?;

        let resp = Self::exchange(sock_path, &body, Mode::Tokens, span, None, Some(&reported_model))?;

        let mut text = String::from_utf8(resp.body).map_err(|_| JadeError::InferenceError {
            message: "the daemon sent invalid UTF-8 in a token frame".to_owned(),
            span,
        })?;
        trim_partial_stop_anchor(&mut text, &req);

        Ok(InferenceResponse { text, tokens_used: resp.tokens_used as i64 })
    }

    fn infer_blocking_stream(
        sock_path: &str,
        req: InferenceRequest,
        span: Span,
        tx: tokio::sync::mpsc::Sender<String>,
        reported_model: Arc<Mutex<Option<String>>>,
    ) -> Result<i64> {
        let body = encode_request(&req).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })?;

        // `blocking_send` is safe here: this runs on a dedicated blocking thread
        // via spawn_blocking. A dropped receiver (cancelled caller) is ignored.
        let mut forward = |token: &[u8]| {
            let _ = tx.blocking_send(String::from_utf8_lossy(token).into_owned());
        };

        let resp = Self::exchange(
            sock_path,
            &body,
            Mode::Tokens,
            span,
            Some(&mut forward),
            Some(&reported_model),
        )?;
        Ok(resp.tokens_used as i64)
    }

    fn count_tokens_blocking(sock_path: &str, prompt: &str, span: Span) -> Result<i64> {
        let body = control_request(serde_json::json!({
            "prompt": prompt,
            "model": "",
            "max_tokens": 0u32,
            "count_only": true,
        }), "count_tokens", span)?;
        Ok(Self::exchange(sock_path, &body, Mode::Tokens, span, None, None)?.tokens_used as i64)
    }

    fn total_tokens_blocking(sock_path: &str, span: Span) -> Result<i64> {
        let body = control_request(serde_json::json!({
            "prompt": "",
            "model": "",
            "max_tokens": 0u32,
            "stats_only": true,
        }), "total_tokens", span)?;
        Ok(Self::exchange(sock_path, &body, Mode::Tokens, span, None, None)?.tokens_used as i64)
    }

    /// A daemon health snapshot (`health_only`), accumulated from `0x05 JSON`
    /// frames. See `design/llm-package-1.1.12.md` §2.3.
    fn health_blocking(sock_path: &str, span: Span) -> Result<serde_json::Value> {
        let body = control_request(serde_json::json!({
            "prompt": "",
            "model": "",
            "max_tokens": 0u32,
            "health_only": true,
        }), "health", span)?;
        let resp = Self::exchange(sock_path, &body, Mode::Json, span, None, None)?;
        serde_json::from_slice(&resp.body).map_err(|e| JadeError::InferenceError {
            message: format!("daemon health response was not valid JSON: {e}"),
            span,
        })
    }
}

/// Strip a partial stop-anchor left on the tail of a response.
///
/// In legacy strip mode the daemon's depth tracker fires on the closing `}`,
/// but the same token may already carry the start of the stop string (`"</"` of
/// `"</tool_call>"`), which reaches us because tokens are emitted before the
/// tracker acts.
///
/// With `keep_anchors` the daemon emits the stop anchor deliberately — and
/// synthesizes it at span close if the model never produced it — so trimming
/// would remove the closing tag the caller parses against.
fn trim_partial_stop_anchor(text: &mut String, req: &InferenceRequest) {
    if req.keep_anchors {
        return;
    }
    let Some(stop) = req.stop_anchor.as_deref() else { return };
    // Longest suffix of `text` that is a prefix of `stop`.
    let mut tail = stop.len().min(text.len());
    while tail > 0 {
        // Only consider suffixes that start on a character boundary — a
        // multi-byte character must not be sliced in half.
        if text.is_char_boundary(text.len() - tail) && stop.starts_with(&text[text.len() - tail..]) {
            text.truncate(text.len() - tail);
            return;
        }
        tail -= 1;
    }
}

// ── Wire protocol ────────────────────────────────────────────────────────────

/// Encode the JSON body of an inference request.
///
/// The 4-byte length prefix is not added here — framing belongs to the
/// transport, which adds it for every request including the control ones below.
///
/// Field order is stable (serde serializes in declaration order), which is what
/// the golden tests pin.
pub(crate) fn encode_request(req: &InferenceRequest) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        prompt: &'a str,
        model: &'a str,
        max_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_anchor: Option<&'a str>,
        // Always emitted (the daemon reads them with serde defaults, but being
        // explicit keeps the wire unambiguous and the golden test meaningful).
        keep_anchors: bool,
        trust: u8,
    }
    serde_json::to_vec(&Wire {
        prompt: &req.prompt,
        model: &req.model,
        max_tokens: req.max_tokens,
        grammar: req.grammar.as_deref(),
        anchor: req.anchor.as_deref(),
        stop_anchor: req.stop_anchor.as_deref(),
        keep_anchors: req.keep_anchors,
        trust: req.trust,
    })
}

/// Encode one of the daemon's control requests (`count_only`, `stats_only`,
/// `health_only`).
///
/// These are built ad hoc because jadelang's [`InferenceRequest`] does not model
/// those flags — the daemon's own `jade-protocol::InferenceRequest` does, and
/// adopting it collapses these three into ordinary requests.
fn control_request(json: serde_json::Value, what: &str, span: Span) -> Result<Vec<u8>> {
    serde_json::to_vec(&json).map_err(|e| JadeError::InferenceError {
        message: format!("failed to encode {what} request: {e}"),
        span,
    })
}
