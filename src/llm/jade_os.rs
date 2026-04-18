//! JadeOsBackend — native inference via /dev/jade
//!
//! Connects to the jade-tree inference daemon through the jade_core kernel
//! module. Writes an InferenceRequest and reads streaming Frame responses
//! until a DONE or ERROR frame is received.
//!
//! ## Protocol (see jade-protocol crate in jade-os repo)
//!
//! Write:  [4 bytes LE: json_len][JSON InferenceRequest]
//! Read:   [1 byte: type][2 bytes LE: payload_len][payload]  (repeat)
//!   0x01 TOKEN  — UTF-8 token; accumulate into response text
//!   0x02 DONE   — 8-byte LE u64 tokens_used; inference complete
//!   0x03 ERROR  — UTF-8 error message

use std::fs::OpenOptions;
use std::io::{Read, Write};

use crate::interpreter::error::{JadeError, Result, Span};
use super::{InferenceBackend, InferenceRequest, InferenceResponse};

const DEV_JADE: &str = "/dev/jade";

pub struct JadeOsBackend {
    device_path: String,
}

impl JadeOsBackend {
    pub fn new() -> Self {
        Self { device_path: DEV_JADE.to_owned() }
    }

    /// Override the device path (useful for testing with a named pipe or socket).
    pub fn with_device(path: impl Into<String>) -> Self {
        Self { device_path: path.into() }
    }
}

#[async_trait::async_trait]
impl InferenceBackend for JadeOsBackend {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let device_path = self.device_path.clone();
        tokio::task::spawn_blocking(move || {
            Self::infer_blocking(&device_path, req, span)
        })
        .await
        .map_err(|e| JadeError::InferenceError {
            message: format!("spawn_blocking panic: {e}"),
            span,
        })?
    }
}

impl JadeOsBackend {
    fn infer_blocking(device_path: &str, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let mut dev = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .map_err(|e| JadeError::InferenceError {
                message: format!(
                    "could not open {} — is jade_core loaded and jade-tree running? ({e})",
                    device_path
                ),
                span,
            })?;

        let payload = encode_request(&req).map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })?;

        dev.write_all(&payload).map_err(|e| JadeError::InferenceError {
            message: format!("write to {} failed: {e}", device_path),
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
                    let n = dev.read(&mut read_tmp).map_err(|e| JadeError::InferenceError {
                        message: format!("read from {} failed: {e}", device_path),
                        span,
                    })?;
                    if n == 0 {
                        return Err(JadeError::InferenceError {
                            message: "device closed before DONE frame".to_owned(),
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

// ── Wire protocol (mirrors jade-protocol crate) ───────────────────────────────
// Duplicated here so jade has no build-time dependency on the jade-os repo.
// Keep in sync with jade-protocol/src/ manually, or replace with a crate dep
// once jade-protocol is published to crates.io.

fn encode_request(req: &InferenceRequest) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        prompt: &'a str,
        model: &'a str,
        history: &'a [super::Message],
        max_tokens: u32,
    }
    let json = serde_json::to_vec(&Wire {
        prompt: &req.prompt,
        model: &req.model,
        history: &req.history,
        max_tokens: req.max_tokens,
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
