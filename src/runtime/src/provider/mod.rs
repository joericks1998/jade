//! Provider-package driver — loads THE active inference provider `.so` and
//! drives its `ovata-infer-protocol` `Provider` ABI. This is the in-process,
//! daemon-free inference path, shared by both engines: the VM (rlib) wraps it in
//! an async backend; AOT-compiled binaries reach it through the `jrt_provider_*`
//! C entry points in [`ffi`].
//!
//! The runtime is deliberately provider-BLIND. It never enumerates providers,
//! matches a name, or parses a selection. The CLI (`jade register` / `jade use`)
//! is the sole writer of the active slot: it places exactly one provider `.so`
//! plus one opaque `config.json` credential blob under
//! `$HOME/.jade/provider/active/`. Because every provider is byte-identical at
//! the ABI, the runtime simply loads whatever single `.so` is there and hands it
//! the config bytes — it does not, and cannot, tell Anthropic from OpenAI.
//!
//! ## FFI safety
//!
//! The provider side wraps every ABI shim in `catch_unwind`, so a provider panic
//! becomes a negative return code, never an unwind across the boundary. The one
//! symmetric obligation here is that our [`FrameCallback`] must not unwind into
//! the provider's frame — so its body is wrapped in `catch_unwind` too. The
//! handle is created once and cached process-wide; the ABI permits sharing it
//! across concurrent `ovata_provider_infer` calls (`Provider: Sync`, shared
//! handle), and each call gets its own sink, so nothing is shared mutably.

use core::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use ovata_infer_protocol::provider::{ERR_BAD_REQUEST, ERR_NULL_ARG, ERR_PANIC, OK};
use ovata_infer_protocol::{Frame, PROVIDER_ABI_VERSION};

use crate::infer::Response;

pub mod ffi;
#[cfg(test)]
mod tests;

/// The canonical dynamic-library extension for provider packages on this
/// platform, used when the CLI names files it creates. Discovery is more lenient
/// (see [`is_provider_lib`]): providers actually ship as `.so` on every platform.
#[cfg(target_os = "macos")]
pub const LIB_EXT: &str = "dylib";
#[cfg(not(target_os = "macos"))]
pub const LIB_EXT: &str = "so";

/// Whether `path` looks like a loadable provider library. Providers are
/// distributed as `.so` on every platform (macOS `dlopen` ignores the
/// extension), so both `.so` and a hand-placed `.dylib` are accepted.
pub fn is_provider_lib(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("so") | Some("dylib"))
}

// ── active-slot paths ─────────────────────────────────────────────────────────

/// `$HOME/.jade` — the per-user Jade home, the same base the daemon socket uses.
pub fn jade_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    PathBuf::from(home).join(".jade")
}

/// The active-provider slot dir, `$HOME/.jade/provider/active/`. The CLI keeps
/// exactly one provider `.so` here. `JADE_PROVIDER_ACTIVE` overrides the whole
/// path (dev/testing), the same shape as `JADE_LLM_SOCK`.
pub fn active_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JADE_PROVIDER_ACTIVE") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    jade_home().join("provider").join("active")
}

/// The single provider `.so` in the active slot, or `None` if none is installed.
pub fn active_lib_path() -> Option<PathBuf> {
    std::fs::read_dir(active_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| is_provider_lib(p))
}

/// The opaque credential blob the active provider is built from
/// (`active/config.json`), or empty if there is none.
pub fn active_config() -> Vec<u8> {
    std::fs::read(active_dir().join("config.json")).unwrap_or_default()
}

/// Whether an active provider is installed (a `.so` sits in the slot). This is
/// the daemon-free inference path's availability check — cheap, no `dlopen`.
pub fn is_active() -> bool {
    active_lib_path().is_some()
}

// ── the loaded provider ───────────────────────────────────────────────────────

// The exported provider ABI (see `ovata_infer_protocol::provider`).
type AbiVersionFn = unsafe extern "C" fn() -> u32;
type NewFn = unsafe extern "C" fn(*const u8, usize) -> *mut c_void;
type InferFn =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, FrameCallback, *mut c_void) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type FrameCallback = extern "C" fn(*mut c_void, *const u8, usize);

/// A loaded provider `.so`: the mapped library, the live handle, and the two
/// entry points we call. Constructing it runs `ovata_provider_new`; dropping it
/// runs `ovata_provider_free`. The `Library` field is last so it drops last,
/// keeping the copied-out fn pointers valid for the whole lifetime.
struct ProviderLib {
    handle: *mut c_void,
    infer: InferFn,
    free: FreeFn,
    _lib: Library,
}

// SAFETY: the handle is a `Box::<P>::into_raw` from the provider, and the ABI
// requires `P: Provider: Send + Sync`. `ovata_provider_infer` takes a shared
// handle, so calling it from several threads at once is sound; `free` runs once,
// in `Drop`.
unsafe impl Send for ProviderLib {}
unsafe impl Sync for ProviderLib {}

impl Drop for ProviderLib {
    fn drop(&mut self) {
        // The provider's free shim already catches its own unwinds.
        unsafe { (self.free)(self.handle) };
    }
}

impl ProviderLib {
    fn load(path: &Path, config: &[u8]) -> Result<Self, String> {
        // SAFETY: loading an arbitrary `.so` runs its initializers; the active
        // provider is trusted (placed by the CLI). Same trust model as native
        // package loading.
        let lib = unsafe { Library::new(path) }
            .map_err(|e| format!("failed to load provider {}: {e}", path.display()))?;

        let abi: libloading::Symbol<AbiVersionFn> =
            unsafe { lib.get(b"ovata_provider_abi_version\0") }
                .map_err(|e| format!("active provider is not a provider package ({e})"))?;
        let version = unsafe { abi() };
        if version != PROVIDER_ABI_VERSION {
            return Err(format!(
                "active provider is ABI version {version}, this build speaks {PROVIDER_ABI_VERSION} — reinstall the provider"
            ));
        }

        let new_fn: libloading::Symbol<NewFn> = unsafe { lib.get(b"ovata_provider_new\0") }
            .map_err(|e| format!("provider missing ovata_provider_new ({e})"))?;
        let infer: libloading::Symbol<InferFn> = unsafe { lib.get(b"ovata_provider_infer\0") }
            .map_err(|e| format!("provider missing ovata_provider_infer ({e})"))?;
        let free: libloading::Symbol<FreeFn> = unsafe { lib.get(b"ovata_provider_free\0") }
            .map_err(|e| format!("provider missing ovata_provider_free ({e})"))?;

        // Copy the raw fn pointers out; `_lib` below keeps them valid.
        let infer = *infer;
        let free = *free;

        // A null handle means the provider rejected its config (bad/absent key).
        let handle = unsafe { new_fn(config.as_ptr(), config.len()) };
        if handle.is_null() {
            return Err(
                "provider rejected its credentials — check your API key or run `jade register`"
                    .to_string(),
            );
        }

        Ok(ProviderLib { handle, infer, free, _lib: lib })
    }
}

/// State the [`frame_callback`] writes into, one per in-flight request.
struct Sink<'a> {
    body: Vec<u8>,
    tokens_used: u64,
    error: Option<String>,
    decode_error: Option<String>,
    done: bool,
    on_token: Option<&'a mut dyn FnMut(&[u8])>,
}

/// The C callback the provider invokes once per encoded [`Frame`]. `ctx` points
/// at a [`Sink`]. Wrapped in `catch_unwind` because unwinding into the provider's
/// frame across the `cdylib` boundary would be UB.
///
/// Soundness of the `&mut *(ctx as *mut Sink)` reborrow rests on this callback
/// being invoked **serially within a single `ovata_provider_infer` call**, never
/// concurrently from provider-spawned threads — which the `FrameSink::emit(&mut
/// self)` shape guarantees for a Rust provider, and is part of the provider trust
/// model for a non-Rust one.
extern "C" fn frame_callback(ctx: *mut c_void, ptr: *const u8, len: usize) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() || ptr.is_null() {
            return;
        }
        // SAFETY: `ctx` is the `&mut Sink` we passed to `ovata_provider_infer`,
        // called only for the duration of that synchronous call on this thread.
        let sink = unsafe { &mut *(ctx as *mut Sink) };
        if sink.decode_error.is_some() {
            return; // already broken; ignore the rest of the stream
        }
        // SAFETY: the ABI guarantees `ptr..ptr+len` is one encoded frame, valid
        // for the call. Each callback delivers exactly one whole frame.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        match Frame::decode(bytes) {
            Ok((Frame::Token(t), _)) => {
                // Accumulate first, forward second: if a caller's `on_token`
                // panics, the caught unwind still leaves `body` complete.
                sink.body.extend_from_slice(t.as_bytes());
                if let Some(cb) = sink.on_token.as_mut() {
                    cb(t.as_bytes());
                }
            }
            Ok((Frame::Done { tokens_used }, _)) => {
                sink.tokens_used = tokens_used;
                sink.done = true;
            }
            Ok((Frame::Error(e), _)) => sink.error = Some(e),
            // Meta names the provider (we don't need it); Json is out-of-band.
            Ok((Frame::Meta { .. }, _)) | Ok((Frame::Json(_), _)) => {}
            Err(e) => sink.decode_error = Some(e.to_string()),
        }
    }));
}

/// Drive one request against a loaded provider, forwarding tokens to `on_token`
/// if given, returning the accumulated body + token count.
fn run_on(
    lib: &ProviderLib,
    req_json: &[u8],
    on_token: Option<&mut dyn FnMut(&[u8])>,
) -> Result<Response, String> {
    let mut sink = Sink {
        body: Vec::new(),
        tokens_used: 0,
        error: None,
        decode_error: None,
        done: false,
        on_token,
    };

    // SAFETY: `lib.handle` is live; `req_json` is valid for its len;
    // `frame_callback` + `&mut sink` are valid for the call. The provider shim
    // catches its own panics. This rests on the provider NOT retaining `ctx` or
    // the frame bytes past the call: `sink` lives on this stack for exactly the
    // call's duration, and we read it only after `infer` returns.
    let rc = unsafe {
        (lib.infer)(
            lib.handle,
            req_json.as_ptr(),
            req_json.len(),
            frame_callback,
            &mut sink as *mut Sink as *mut c_void,
        )
    };

    match rc {
        OK => {}
        ERR_NULL_ARG => return Err("provider rejected the request (null argument)".into()),
        ERR_BAD_REQUEST => return Err("provider could not decode the request".into()),
        ERR_PANIC => return Err("provider panicked while serving the request".into()),
        other => return Err(format!("provider returned error code {other}")),
    }

    if let Some(e) = sink.decode_error {
        return Err(format!("provider sent a malformed response frame: {e}"));
    }
    if let Some(e) = sink.error {
        return Err(e); // provider's own Error frame — a real generation failure
    }
    if !sink.done {
        return Err("provider ended the stream without completing".into());
    }
    Ok(Response { body: sink.body, tokens_used: sink.tokens_used })
}

/// The process-wide active provider, loaded from the slot on first use.
fn shared() -> Result<Arc<ProviderLib>, String> {
    static SLOT: OnceLock<Mutex<Option<Arc<ProviderLib>>>> = OnceLock::new();
    let slot = SLOT.get_or_init(|| Mutex::new(None));
    // Recover from a poisoned lock rather than bricking every future prompt: the
    // guard is set to `Some` only after `load` succeeds, so a prior load panic
    // leaves it logically `None` — safe to retry.
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lib) = guard.as_ref() {
        return Ok(lib.clone());
    }
    let path = active_lib_path()
        .ok_or_else(|| "no active inference provider (run `jade register`)".to_string())?;
    let lib = Arc::new(ProviderLib::load(&path, &active_config())?);
    *guard = Some(lib.clone());
    Ok(lib)
}

/// Run one request against the active provider (loading it on first use),
/// forwarding tokens to `on_token` if given.
pub fn run(req_json: &[u8], on_token: Option<&mut dyn FnMut(&[u8])>) -> Result<Response, String> {
    let lib = shared()?;
    run_on(&lib, req_json, on_token)
}
