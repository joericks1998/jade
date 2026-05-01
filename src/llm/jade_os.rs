//! JadeOsBackend — native inference via /dev/jade
//!
//! Connects to the jade-tree inference daemon through the jade_core kernel
//! module. Writes an InferenceRequest and reads streaming Frame responses
//! until a DONE or ERROR frame is received.
//!

//!






use std::fs::OpenOptions;
use std::io::{Read, Write};

use jade_protocol::{Frame, FrameError};

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

        // Build a jade_protocol InferenceRequest and encode it.
        let proto_req = jade_protocol::InferenceRequest {
            prompt: req.prompt,
            model: req.model,
            history: req.history.into_iter().map(|m| jade_protocol::Message {
                role: m.role,
                content: m.content,
            }).collect(),
            max_tokens: req.max_tokens,
        };
        let payload = proto_req.encode().map_err(|e| JadeError::InferenceError {
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
            match Frame::decode(&buf) {
                Ok((Frame::Token(token), consumed)) => {
                    text.push_str(&token);
                    buf.drain(..consumed);
                }
                Ok((Frame::Done { tokens_used }, consumed)) => {
                    buf.drain(..consumed);
                    return Ok(InferenceResponse {
                        text,
                        tokens_used: tokens_used as i64,
                    });
                }
                Ok((Frame::Error(msg), consumed)) => {
                    buf.drain(..consumed);
                    return Err(JadeError::InferenceError { message: msg, span });
                }
                Err(FrameError::Incomplete) => {
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
                Err(e) => {
                    return Err(JadeError::InferenceError {
                        message: format!("frame decode error: {e}"),
                        span,
                    });
                }
            }
        }
    }
}
