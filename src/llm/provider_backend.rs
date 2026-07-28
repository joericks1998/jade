//! ProviderPackageBackend — the VM's inference backend that drives an installed
//! provider *package*: a compiled Jade `--lib` (e.g. dovata's `anthropic`/`openai`)
//! that speaks HTTP to the vendor's API itself. Since the inference daemon and its
//! Unix socket were removed, this is the only way the VM reaches a model.
//!
//! A provider package conforms to dovata's model-profile shape (see
//! `anthropic.jde`): it exports `infer(request) -> [Frame]` and, optionally,
//! `configure(opts)`. We load it through the native-package machinery, hand it the
//! stored config via `configure` (the API key, etc.), then call `infer({prompt})`
//! and decode the returned array of frame dicts —
//! `{"type":"Token","text":…}` / `{"type":"Done",…}` / `{"type":"Error","message":…}`
//! — into the response text. The package does the HTTP; the language never learns
//! a vendor detail.
//!
//! Constrained decoding rides the same path: a typed dereference (`?p |> Type`)
//! or an explicit `Grammar.new` puts `grammar`/`anchor`/`stop_anchor` in the
//! request dict, and enforcing them is the package's job. A package that cannot
//! honour a grammar says so with an `Error` frame, which surfaces as a catchable
//! Jade error rather than an unconstrained reply.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jade_runtime::coll::StructObj;

use super::{InferenceBackend, InferenceRequest, InferenceResponse};
use crate::frontend::error::{JadeError, Result, Span};
use crate::native::{load_native_package, NativeLibFn};
use crate::vm::VmValue;

pub struct ProviderPackageBackend {
    lib_path: PathBuf,
    config_json: Vec<u8>,
    /// The package's `infer` binding, loaded + configured on first use and cached
    /// (loading is a `dlopen`; configuring sets the package's runtime state once).
    infer_fn: Arc<Mutex<Option<Arc<NativeLibFn>>>>,
}

impl ProviderPackageBackend {
    /// `Some` when a provider package is installed in the active slot
    /// (`$HOME/.jade/provider/active/`); else `None`, so [`super::select_backend`]
    /// falls through to the daemon.
    pub fn from_registry() -> Option<Self> {
        let lib_path = jade_runtime::provider::active_lib_path()?;
        Some(ProviderPackageBackend {
            lib_path,
            config_json: jade_runtime::provider::active_config(),
            infer_fn: Arc::new(Mutex::new(None)),
        })
    }
}

fn err(message: impl Into<String>, span: Span) -> JadeError {
    JadeError::InferenceError { message: message.into(), span }
}

/// Pull a package export out as a callable, if it exists and is one.
fn export(exports: &HashMap<String, VmValue>, name: &str) -> Option<Arc<NativeLibFn>> {
    match exports.get(name) {
        Some(VmValue::NativeLibFn(f)) => Some(f.clone()),
        _ => None,
    }
}

/// Load the provider package, call `configure(config)` once (if it takes one and
/// we have stored config), and return its `infer` binding.
fn load_and_configure(lib_path: &Path, config_json: &[u8], span: Span) -> Result<Arc<NativeLibFn>> {
    let exports = load_native_package(lib_path, span)?;
    let infer = export(&exports, "infer")
        .ok_or_else(|| err("active provider package exports no `infer` function", span))?;

    // Stored config (`{"api_key":…}` and any extra params) → the package's
    // `configure`. If there's no stored config, the package resolves the key
    // itself (its own env var), so we skip configuring.
    if !config_json.is_empty() {
        if let Some(configure) = export(&exports, "configure") {
            let cfg = config_value(config_json, span)?;
            configure.call(&[cfg], span)?;
        }
    }
    Ok(infer)
}

/// Parse the stored `config.json` into a `VmValue` dict for `configure`.
fn config_value(config_json: &[u8], span: Span) -> Result<VmValue> {
    let v: serde_json::Value = serde_json::from_slice(config_json)
        .map_err(|e| err(format!("provider config.json is not valid JSON: {e}"), span))?;
    crate::vm::coerce::json_to_vm_value(&v).map_err(|e| err(format!("provider config: {e}"), span))
}

/// Build the `InferRequest` the package receives.
///
/// Every field is always present; an unset one is `nil`. That is the difference
/// from the dict this used to be — a struct has a fixed shape and a type name,
/// so a package can tell an `InferRequest` from something that merely has the
/// right keys, and the receiver of a renamed field gets a type mismatch instead
/// of a silent nil. The definition is
/// `protocol/jade/infer.jde`; [`super::REQUEST_FIELDS`] is
/// the compiler's copy of it, tripwired in `llm/tests.rs`.
pub(super) fn request_value(req: &InferenceRequest) -> VmValue {
    let opt = |v: &Option<String>| match v {
        Some(s) => VmValue::Str(s.clone().into()),
        None => VmValue::Nil,
    };
    let mut obj = StructObj::<VmValue>::new(super::REQUEST_TYPE);
    obj.set_field(super::REQUEST_FIELDS[0], VmValue::Str(req.prompt.clone().into()));
    obj.set_field(super::REQUEST_FIELDS[1], opt(&req.grammar));
    obj.set_field(super::REQUEST_FIELDS[2], opt(&req.anchor));
    obj.set_field(super::REQUEST_FIELDS[3], opt(&req.stop_anchor));
    VmValue::Struct(Arc::new(parking_lot::Mutex::new(obj)))
}

/// Decode the `[Frame]` array the package returns into response text. Success is
/// `Token*` (plus an optional `Json` tool-call frame and a `Done`); a failure is a
/// single `Error` frame, which we surface as a catchable inference error.
fn decode_frames(result: VmValue, span: Span) -> Result<String> {
    let VmValue::Array(arr) = result else {
        return Err(err("provider `infer` did not return a frame array", span));
    };
    let arr = arr.lock();
    let mut text = String::new();
    for frame in arr.as_slice() {
        let VmValue::Dict(d) = frame else { continue };
        let ty = match d.get("type") {
            Some(VmValue::Str(s)) => s.as_str().to_owned(),
            _ => continue,
        };
        match ty.as_str() {
            "Token" => {
                if let Some(VmValue::Str(t)) = d.get("text") {
                    text.push_str(t.as_str());
                }
            }
            "Error" => {
                let msg = match d.get("message") {
                    Some(VmValue::Str(m)) => m.as_str().to_owned(),
                    _ => "provider returned an error".to_owned(),
                };
                return Err(err(msg, span));
            }
            // `Done` carries only the token count; `Json` carries tool calls —
            // neither contributes to the plain-text response.
            _ => {}
        }
    }
    Ok(text)
}

#[async_trait::async_trait]
impl InferenceBackend for ProviderPackageBackend {
    async fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let lib_path = self.lib_path.clone();
        let config_json = self.config_json.clone();
        let cache = self.infer_fn.clone();
        tokio::task::spawn_blocking(move || {
            // Load + configure once, then reuse the `infer` binding.
            let infer = {
                let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
                match guard.as_ref() {
                    Some(f) => f.clone(),
                    None => {
                        let f = load_and_configure(&lib_path, &config_json, span)?;
                        *guard = Some(f.clone());
                        f
                    }
                }
            };
            let result = infer.call(&[request_value(&req)], span)?;
            let text = decode_frames(result, span)?;
            Ok(InferenceResponse { text })
        })
        .await
        .map_err(|e| err(format!("provider task panic: {e}"), span))?
    }
}
