//! The active-provider slot — where the CLI installs the one provider package the
//! engines load, and how both engines find it.
//!
//! A provider is a compiled Jade `--lib` package (dovata's `anthropic`/`openai`)
//! that exposes `infer(request) -> [Frame]` / `configure(opts)` and does its own
//! HTTP to the vendor API. This module only resolves the *slot* — it does not load
//! or drive the package. The VM loads it through `crate::native` (the jade crate);
//! AOT-compiled binaries load it through the C runtime's `jrt_native_*` — both via
//! the small C surface in [`ffi`].
//!
//! The CLI (`jade register` / `jade use`) is the sole writer of the slot: it keeps
//! exactly one `.so` under `$HOME/.jade/provider/active/` plus one opaque
//! `config.json` credential blob. Because every provider is a package with the same
//! `infer` shape, neither engine learns a vendor detail — it loads whatever library
//! is there.

use std::path::{Path, PathBuf};

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

/// `$HOME/.jade` — the per-user Jade home, shared with the package cache and
/// the credential store.
pub fn jade_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    PathBuf::from(home).join(".jade")
}

/// The active-provider slot dir, `$HOME/.jade/provider/active/`. The CLI keeps
/// exactly one provider `.so` here. `JADE_PROVIDER_ACTIVE` overrides the whole
/// path (dev/testing), which is what lets the parity gate point both engines at
/// a stand-in provider.
pub fn active_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JADE_PROVIDER_ACTIVE")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    jade_home().join("provider").join("active")
}

/// The single provider `.so` in the active slot, or `None` if none is installed.
pub fn active_lib_path() -> Option<PathBuf> {
    std::fs::read_dir(active_dir()).ok()?.flatten().map(|e| e.path()).find(|p| is_provider_lib(p))
}

/// The opaque credential blob the active provider is configured with
/// (`active/config.json`), or empty if there is none.
pub fn active_config() -> Vec<u8> {
    std::fs::read(active_dir().join("config.json")).unwrap_or_default()
}

/// Whether an active provider is installed (a `.so` sits in the slot). This is
/// the daemon-free inference path's availability check.
pub fn is_active() -> bool {
    active_lib_path().is_some()
}
