//! ProviderPackageBackend — the VM's async facade over the shared provider
//! driver in [`jade_runtime::provider`].
//!
//! The driver itself (dlopen the active provider `.so`, drive its ABI, decode
//! frames) is single-sourced in `jade-runtime` so the VM and AOT-compiled
//! binaries drive providers identically — no second implementation to drift.
//! This file only adds the async/`spawn_blocking` shell and maps the driver's
//! `String` errors into catchable `JadeError`s carrying the `?p` span.

use jade_runtime::provider;
use ovata_infer_protocol::InferenceRequest;

use super::{InferenceBackend, InferenceResponse};
use crate::frontend::error::{JadeError, Result, Span};

pub struct ProviderPackageBackend;

impl ProviderPackageBackend {
    /// `Some` when an active provider is installed in the slot
    /// (`$HOME/.jade/provider/active/`); else `None`, so [`super::select_backend`]
    /// falls through to the daemon. A provider whose credentials are bad still
    /// returns `Some` and reports the failure at the first prompt (the driver
    /// loads lazily), so the user sees why rather than silently getting the daemon.
    pub fn from_registry() -> Option<Self> {
        if provider::is_active() {
            Some(ProviderPackageBackend)
        } else {
            None
        }
    }

    fn encode(req: &InferenceRequest, span: Span) -> Result<Vec<u8>> {
        req.encode_body().map_err(|e| JadeError::InferenceError {
            message: format!("failed to encode inference request: {e}"),
            span,
        })
    }
}

#[async_trait::async_trait]
impl InferenceBackend for ProviderPackageBackend {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let body = Self::encode(&req, span)?;
        tokio::task::spawn_blocking(move || provider::run(&body, None))
            .await
            .map_err(|e| JadeError::InferenceError {
                message: format!("provider task panic: {e}"),
                span,
            })?
            .map(|resp| InferenceResponse {
                text: String::from_utf8_lossy(&resp.body).into_owned(),
            })
            .map_err(|message| JadeError::InferenceError { message, span })
    }

    async fn infer_stream(
        &self,
        req: InferenceRequest,
        span: Span,
    ) -> Result<(tokio::sync::mpsc::Receiver<String>, tokio::task::JoinHandle<Result<()>>)> {
        let body = Self::encode(&req, span)?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let handle = tokio::task::spawn_blocking(move || {
            // `blocking_send` is safe on this dedicated blocking thread; a dropped
            // receiver (cancelled caller) is ignored.
            let mut forward = |bytes: &[u8]| {
                let _ = tx.blocking_send(String::from_utf8_lossy(bytes).into_owned());
            };
            provider::run(&body, Some(&mut forward))
                .map(|_| ())
                .map_err(|message| JadeError::InferenceError { message, span })
        });
        Ok((rx, handle))
    }
}
