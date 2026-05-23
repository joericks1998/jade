#![cfg(unix)]
//! JadeOsBackend — native inference via Unix domain socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::frontend::error::{JadeError, Result, Span};
use super::{InferenceBackend, InferenceRequest, InferenceResponse};

fn jade_sock_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    format!("{home}/.jade/llm.sock")
}

pub struct JadeOsBackend {
    sock_path: String,
}

impl JadeOsBackend {
    pub fn new() -> Self {
        Self { sock_path: jade_sock_path() }
    }

    pub fn with_device(path: impl Into<String>) -> Self {
        Self { sock_path: path.into() }
    }
}

#[async_trait::async_trait]
impl InferenceBackend for JadeOsBackend {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let sock_path = self.sock_path.clone();
        tokio::task::spawn_blocking(move || {
            Self::infer_blocking(&sock_path, req, span)
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
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let handle = tokio::task::spawn_blocking(move || {
            Self::infer_blocking_stream(&sock_path, req, span, tx)
        });
        Ok((rx, handle))
    }
}

impl JadeOsBackend {
    fn infer_blocking(sock_path: &str, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
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
        let mut text = String::new();

        loop {
            match decode_frame(&buf) {
                FrameResult::Token(token, consumed) => {
                    text.push_str(&token);
                    buf.drain(..consumed);
                }
                FrameResult::Done(tokens_used, consumed) => {
                    buf.drain(..consumed);
                    return Ok(InferenceResponse {
                        text,
                        tokens_used: tokens_used as i64,
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

    fn infer_blocking_stream(
        sock_path: &str,
        req: InferenceRequest,
        span: Span,
        tx: tokio::sync::mpsc::Sender<String>,
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

        loop {
            match decode_frame(&buf) {
                FrameResult::Token(token, consumed) => {
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
}

// ── Wire protocol ────────────────────────────────────────────────────────────

fn encode_request(req: &InferenceRequest) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        prompt: &'a str,
        model: &'a str,
        max_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<&'a str>,
    }
    let json = serde_json::to_vec(&Wire {
        prompt: &req.prompt,
        model: &req.model,
        max_tokens: req.max_tokens,
        grammar: req.grammar.as_deref(),
        anchor: req.anchor.as_deref(),
    })?;
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

enum FrameResult {
    Token(String, usize),
    Done(u64, usize),
    Error(String, usize),
    Incomplete,
    UnknownType(u8),
}

fn decode_frame(buf: &[u8]) -> FrameResult {
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
        0x01 => match std::str::from_utf8(payload) {
            Ok(s) => FrameResult::Token(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in TOKEN frame".to_owned(), consumed),
        },
        0x02 => {
            if payload_len != 8 {
                return FrameResult::Error("malformed DONE frame".to_owned(), consumed);
            }
            let tokens_used = u64::from_le_bytes(
                payload.try_into().expect("invariant: payload_len was checked to be 8 above"),
            );
            FrameResult::Done(tokens_used, consumed)
        }
        0x03 => match std::str::from_utf8(payload) {
            Ok(s) => FrameResult::Error(s.to_owned(), consumed),
            Err(_) => FrameResult::Error("daemon sent invalid UTF-8 in ERROR frame".to_owned(), consumed),
        },
        other => FrameResult::UnknownType(other),
    }
}
