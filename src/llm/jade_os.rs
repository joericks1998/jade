#![cfg(unix)]
//! JadeOsBackend — native inference via Unix domain socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use crate::frontend::error::{JadeError, Result, Span};
use super::{InferenceBackend, InferenceRequest, InferenceResponse};

fn jade_sock_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    format!("{home}/.jade/llm.sock")
}

pub struct JadeOsBackend {
    sock_path: String,
    reported_model: Arc<Mutex<Option<String>>>,
}

impl JadeOsBackend {
    pub fn new() -> Self {
        Self {
            sock_path: jade_sock_path(),
            reported_model: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait::async_trait]
impl InferenceBackend for JadeOsBackend {
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
        tokio::task::spawn_blocking(move || {
            Self::count_tokens_blocking(&sock_path, &prompt, span)
        })
        .await
        .map_err(|e| JadeError::InferenceError {
            message: format!("spawn_blocking panic: {e}"),
            span,
        })?
    }

    async fn total_tokens(&self, span: Span) -> Result<i64> {
        let sock_path = self.sock_path.clone();
        tokio::task::spawn_blocking(move || {
            Self::total_tokens_blocking(&sock_path, span)
        })
        .await
        .map_err(|e| JadeError::InferenceError {
            message: format!("spawn_blocking panic: {e}"),
            span,
        })?
    }

    async fn health(&self, span: Span) -> Result<serde_json::Value> {
        let sock_path = self.sock_path.clone();
        tokio::task::spawn_blocking(move || {
            Self::health_blocking(&sock_path, span)
        })
        .await
        .map_err(|e| JadeError::InferenceError {
            message: format!("spawn_blocking panic: {e}"),
            span,
        })?
    }
}

impl JadeOsBackend {
    fn infer_blocking(sock_path: &str, req: InferenceRequest, span: Span, reported_model: Arc<Mutex<Option<String>>>) -> Result<InferenceResponse> {
        let mut stream = UnixStream::connect(sock_path)
            .map_err(|e| JadeError::InferenceError {
                message: format!(
                    "could not connect to {} — is the inference daemon running? ({e})",
                    sock_path
                ),
                span,
            })?;

        // Serialize and send the request as a length-prefixed JSON frame.
        let payload = encode_request(&req).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })?;

        stream.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", sock_path),
            span,
        })?;

        let mut buf: Vec<u8> = Vec::new();
        let mut read_tmp = [0u8; 4096];
        let mut text = String::new();

        // Read frames from the daemon, accumulating token text until DONE.
        // Each iteration either decodes a complete frame from `buf` or reads
        // more bytes from the socket when the buffer holds only a partial frame.
        loop {
            match decode_frame(&buf) {
                // Daemon announces the active model name before streaming tokens.
                FrameResult::Meta(model, consumed) => {
                    *reported_model.lock().unwrap() = Some(model);
                    buf.drain(..consumed);
                }
                // Generation ops don't expect structured JSON frames — ignore.
                FrameResult::Json(_, consumed) => { buf.drain(..consumed); }
                FrameResult::Token(token, consumed) => {
                    text.push_str(&token);
                    buf.drain(..consumed);
                }
                FrameResult::Done(tokens_used, consumed) => {
                    buf.drain(..consumed);
                    // Only strip a partial stop-anchor suffix when keep_anchors is
                    // false (legacy strip mode). In that mode jade-tree's depth
                    // tracker fires on the closing '}', but the token may also carry
                    // the start of the stop string (e.g. "</" from "</tool_call>"),
                    // which leaks into `text` because on_token fires before the
                    // depth tracker acts.
                    //
                    // When keep_anchors is true the daemon intentionally emits the
                    // stop_anchor as tokens and synthesizes it at span-close if the
                    // model didn't produce it — stripping here would remove the
                    // closing tag the caller needs for parsing.
                    if !req.keep_anchors {
                        if let Some(stop) = req.stop_anchor.as_deref() {
                            // Walk backwards through possible prefix lengths and trim
                            // the longest suffix of `text` that is a prefix of `stop`.
                            let mut tail = stop.len().min(text.len());
                            while tail > 0 {
                                if stop.starts_with(&text[text.len() - tail..]) {
                                    text.truncate(text.len() - tail);
                                    break;
                                }
                                tail -= 1;
                            }
                        }
                    }
                    return Ok(InferenceResponse {
                        text,
                        tokens_used: tokens_used as i64,
                    });
                }
                FrameResult::Error(msg, consumed) => {
                    buf.drain(..consumed);
                    return Err(JadeError::InferenceError { message: msg, span });
                }
                // Buffer holds an incomplete frame — read more bytes from the socket.
                FrameResult::Incomplete => {
                    let n = stream.read(&mut read_tmp).map_err(|e| JadeError::InferenceError {
                        message: format!("read from {} failed: {e}", sock_path),
                        span,
                    })?;
                    if n == 0 {
                        return Err(JadeError::InferenceError {
                            message: "socket closed before DONE frame".to_owned(),
                            span,
                        });
                    }
                    buf.extend_from_slice(&read_tmp[..n]);
                }
                FrameResult::UnknownType(t) => {
                    return Err(JadeError::InferenceError {
                        message: format!("unknown frame type from daemon: {t:#04x}"),
                        span,
                    });
                }
            }
        }
    }

    fn infer_blocking_stream(
        sock_path: &str,
        req: InferenceRequest,
        span: Span,
        tx: tokio::sync::mpsc::Sender<String>,
        reported_model: Arc<Mutex<Option<String>>>,
    ) -> Result<i64> {
        let mut stream = UnixStream::connect(sock_path)
            .map_err(|e| JadeError::InferenceError {
                message: format!(
                    "could not connect to {} — is the inference daemon running? ({e})",
                    sock_path
                ),
                span,
            })?;

        let payload = encode_request(&req).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })?;

        stream.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", sock_path),
            span,
        })?;

        let mut buf: Vec<u8> = Vec::new();
        let mut read_tmp = [0u8; 4096];

        // Same frame-drain loop as infer_blocking, but tokens are forwarded to
        // the async channel instead of accumulated.  `blocking_send` is safe here
        // because we're already on a dedicated blocking thread via spawn_blocking.
        loop {
            match decode_frame(&buf) {
                FrameResult::Meta(model, consumed) => {
                    *reported_model.lock().unwrap() = Some(model);
                    buf.drain(..consumed);
                }
                // Generation ops don't expect structured JSON frames — ignore.
                FrameResult::Json(_, consumed) => { buf.drain(..consumed); }
                FrameResult::Token(token, consumed) => {
                    // Silently drop the token if the receiver was dropped (caller cancelled).
                    let _ = tx.blocking_send(token);
                    buf.drain(..consumed);
                }
                FrameResult::Done(tokens_used, consumed) => {
                    buf.drain(..consumed);
                    return Ok(tokens_used as i64);
                }
                FrameResult::Error(msg, consumed) => {
                    buf.drain(..consumed);
                    return Err(JadeError::InferenceError { message: msg, span });
                }
                FrameResult::Incomplete => {
                    let n = stream.read(&mut read_tmp).map_err(|e| JadeError::InferenceError {
                        message: format!("read from {} failed: {e}", sock_path),
                        span,
                    })?;
                    if n == 0 {
                        return Err(JadeError::InferenceError {
                            message: "socket closed before DONE frame".to_owned(),
                            span,
                        });
                    }
                    buf.extend_from_slice(&read_tmp[..n]);
                }
                FrameResult::UnknownType(t) => {
                    return Err(JadeError::InferenceError {
                        message: format!("unknown frame type from daemon: {t:#04x}"),
                        span,
                    });
                }
            }
        }
    }


    fn count_tokens_blocking(sock_path: &str, prompt: &str, span: Span) -> Result<i64> {
        // `count_only` is a daemon-side flag not modelled in InferenceRequest, so
        // we build the JSON manually rather than going through encode_request.
        let json = serde_json::json!({
            "prompt": prompt,
            "model": "",
            "max_tokens": 0u32,
            "count_only": true,
        });
        let json_bytes = serde_json::to_vec(&json).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode count_tokens request: {e}"),
            span,
        })?;
        let len = json_bytes.len() as u32;
        let mut payload = Vec::with_capacity(4 + json_bytes.len());
        payload.extend_from_slice(&len.to_le_bytes());
        payload.extend_from_slice(&json_bytes);

        let mut stream = UnixStream::connect(sock_path).map_err(|e| JadeError::InferenceError {
            message: format!("could not connect to {} — is the inference daemon running? ({e})", sock_path),
            span,
        })?;
        stream.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", sock_path),
            span,
        })?;

        Self::drain_to_done(&mut stream, sock_path, span)
    }

    fn total_tokens_blocking(sock_path: &str, span: Span) -> Result<i64> {
        // `stats_only` asks the daemon to return cumulative session token usage
        // without running any inference.
        let json = serde_json::json!({
            "prompt": "",
            "model": "",
            "max_tokens": 0u32,
            "stats_only": true,
        });
        let json_bytes = serde_json::to_vec(&json).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode total_tokens request: {e}"),
            span,
        })?;
        let len = json_bytes.len() as u32;
        let mut payload = Vec::with_capacity(4 + json_bytes.len());
        payload.extend_from_slice(&len.to_le_bytes());
        payload.extend_from_slice(&json_bytes);

        let mut stream = UnixStream::connect(sock_path).map_err(|e| JadeError::InferenceError {
            message: format!("could not connect to {} — is the inference daemon running? ({e})", sock_path),
            span,
        })?;
        stream.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", sock_path),
            span,
        })?;

        Self::drain_to_done(&mut stream, sock_path, span)
    }

    // Request a daemon health snapshot (`health_only`) and accumulate the
    // `0x05 JSON` frames until DONE, then parse. Mirrors the wire contract in
    // design/llm-package-1.1.12.md §2.3.
    fn health_blocking(sock_path: &str, span: Span) -> Result<serde_json::Value> {
        let json = serde_json::json!({
            "prompt": "",
            "model": "",
            "max_tokens": 0u32,
            "health_only": true,
        });
        let json_bytes = serde_json::to_vec(&json).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode health request: {e}"),
            span,
        })?;
        let len = json_bytes.len() as u32;
        let mut payload = Vec::with_capacity(4 + json_bytes.len());
        payload.extend_from_slice(&len.to_le_bytes());
        payload.extend_from_slice(&json_bytes);

        let mut stream = UnixStream::connect(sock_path).map_err(|e| JadeError::InferenceError {
            message: format!("could not connect to {} — is the inference daemon running? ({e})", sock_path),
            span,
        })?;
        stream.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", sock_path),
            span,
        })?;

        let mut buf: Vec<u8> = Vec::new();
        let mut read_tmp = [0u8; 4096];
        let mut json_text = String::new();
        loop {
            match decode_frame(&buf) {
                FrameResult::Json(chunk, consumed) => {
                    json_text.push_str(&chunk);
                    buf.drain(..consumed);
                }
                FrameResult::Meta(_, consumed) | FrameResult::Token(_, consumed) => {
                    buf.drain(..consumed);
                }
                FrameResult::Done(_, consumed) => {
                    buf.drain(..consumed);
                    return serde_json::from_str(&json_text).map_err(|e| JadeError::InferenceError {
                        message: format!("daemon health response was not valid JSON: {e}"),
                        span,
                    });
                }
                FrameResult::Error(msg, consumed) => {
                    buf.drain(..consumed);
                    return Err(JadeError::InferenceError { message: msg, span });
                }
                FrameResult::Incomplete => {
                    let n = stream.read(&mut read_tmp).map_err(|e| JadeError::InferenceError {
                        message: format!("read from {} failed: {e}", sock_path),
                        span,
                    })?;
                    if n == 0 {
                        return Err(JadeError::InferenceError {
                            message: "socket closed before DONE frame".to_owned(),
                            span,
                        });
                    }
                    buf.extend_from_slice(&read_tmp[..n]);
                }
                FrameResult::UnknownType(t) => {
                    return Err(JadeError::InferenceError {
                        message: format!("unknown frame type from daemon: {t:#04x}"),
                        span,
                    });
                }
            }
        }
    }

    // Shared tail for count_tokens and total_tokens: drain all frames until DONE,
    // returning the token count embedded in the DONE frame. Token/Meta/JSON frames
    // are discarded — these ops don't produce text output.
    fn drain_to_done(stream: &mut UnixStream, sock_path: &str, span: Span) -> Result<i64> {
        let mut buf: Vec<u8> = Vec::new();
        let mut read_tmp = [0u8; 4096];
        loop {
            match decode_frame(&buf) {
                FrameResult::Meta(_, consumed) => { buf.drain(..consumed); }
                FrameResult::Json(_, consumed) => { buf.drain(..consumed); }
                FrameResult::Token(_, consumed) => { buf.drain(..consumed); }
                FrameResult::Done(tokens_used, consumed) => {
                    buf.drain(..consumed);
                    return Ok(tokens_used as i64);
                }
                FrameResult::Error(msg, consumed) => {
                    buf.drain(..consumed);
                    return Err(JadeError::InferenceError { message: msg, span });
                }
                FrameResult::Incomplete => {
                    let n = stream.read(&mut read_tmp).map_err(|e| JadeError::InferenceError {
                        message: format!("read from {} failed: {e}", sock_path),
                        span,
                    })?;
                    if n == 0 {
                        return Err(JadeError::InferenceError {
                            message: "socket closed before DONE frame".to_owned(),
                            span,
                        });
                    }
                    buf.extend_from_slice(&read_tmp[..n]);
                }
                FrameResult::UnknownType(t) => {
                    return Err(JadeError::InferenceError {
                        message: format!("unknown frame type from daemon: {t:#04x}"),
                        span,
                    });
                }
            }
        }
    }
}

// ── Wire protocol ────────────────────────────────────────────────────────────

// Serialize an InferenceRequest into the daemon wire format:
//   [payload_len: u32 LE][JSON bytes]
// Field order is stable (serde serializes struct fields in declaration order),
// which matters for the golden tests that pin the exact byte sequence.
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
    let json = serde_json::to_vec(&Wire {
        prompt: &req.prompt,
        model: &req.model,
        max_tokens: req.max_tokens,
        grammar: req.grammar.as_deref(),
        anchor: req.anchor.as_deref(),
        stop_anchor: req.stop_anchor.as_deref(),
        keep_anchors: req.keep_anchors,
        trust: req.trust,
    })?;
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

enum FrameResult {
    Meta(String, usize),
    Token(String, usize),
    /// `0x05 JSON` — a structured (non-token) result chunk. Accumulated until
    /// DONE by ops that expect it (health); ignored by token-streaming ops.
    Json(String, usize),
    Done(u64, usize),
    Error(String, usize),
    Incomplete,
    UnknownType(u8),
}

// Parse one framed message from the front of `buf`.
// Frame layout (daemon → client):
//   [type: u8][payload_len: u16 LE][payload: bytes…]
// Returns the decoded variant plus the number of bytes consumed so the caller
// can drain exactly that many bytes with `buf.drain(..consumed)`.
fn decode_frame(buf: &[u8]) -> FrameResult {
    // Need at least the 3-byte header before we know the payload length.
    if buf.len() < 3 {
        return FrameResult::Incomplete;
    }
    let frame_type = buf[0];
    let payload_len = u16::from_le_bytes([buf[1], buf[2]]) as usize;
    if buf.len() < 3 + payload_len {
        return FrameResult::Incomplete;
    }
    let payload = &buf[3..3 + payload_len];
    let consumed = 3 + payload_len;

    match frame_type {
        0x01 => match std::str::from_utf8(payload) { // TOKEN — a generated text chunk
            Ok(s) => FrameResult::Token(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in TOKEN frame".to_owned(), consumed),
        },
        0x02 => { // DONE — payload is tokens_used as u64 LE
            if payload_len != 8 {
                return FrameResult::Error("malformed DONE frame".to_owned(), consumed);
            }
            let tokens_used = u64::from_le_bytes(
                payload.try_into().expect("invariant: payload_len was checked to be 8 above"),
            );
            FrameResult::Done(tokens_used, consumed)
        }
        0x03 => match std::str::from_utf8(payload) { // ERROR — human-readable message
            Ok(s) => FrameResult::Error(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in ERROR frame".to_owned(), consumed),
        },
        0x04 => match std::str::from_utf8(payload) { // META — model name string
            Ok(s) => FrameResult::Meta(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in META frame".to_owned(), consumed),
        },
        0x05 => match std::str::from_utf8(payload) { // JSON — structured result chunk (e.g. health)
            Ok(s) => FrameResult::Json(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in JSON frame".to_owned(), consumed),
        },
        other => FrameResult::UnknownType(other),
    }
}
