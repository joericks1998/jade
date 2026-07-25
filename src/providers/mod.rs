//! The provider registry — the CLI-side management of inference provider
//! packages. This is the *only* writer of the active slot the runtime reads.
//!
//! Layout (all per-user, under `$HOME/.jade/`):
//!
//! | Path | Contents |
//! |---|---|
//! | `provider/<name>.<ext>` | the installed pool — every provider the user has added |
//! | `provider/active/<name>.<ext>` | exactly ONE `.so`: the active provider the runtime loads |
//! | `provider/active/config.json` | the active provider's opaque credential blob |
//! | `credentials/<name>.json` | per-provider key backups, so switching doesn't re-prompt |
//!
//! Provider libraries can also be discovered from where the toolchain ships them
//! (`<prefix>/lib/jade/providers/`) and from `JADE_PROVIDERS_DIR` (dev). Selecting
//! a provider copies it into the pool and then into `active/`, so the runtime
//! only ever sees the one active library — it never learns a provider's name.
//!
//! The active-slot *paths* live in [`jade_runtime::provider`] (the runtime reads
//! them too); everything about *choosing* and *configuring* a provider is here.

use std::io;
use std::path::{Path, PathBuf};

use jade_runtime::provider::{active_dir, jade_home, LIB_EXT};

/// Extra directory to search for provider `.so`s (dev/testing), highest priority.
const ENV_PROVIDERS_DIR: &str = "JADE_PROVIDERS_DIR";

/// The installed-pool directory, `$HOME/.jade/provider/`.
pub fn pool_dir() -> PathBuf {
    jade_home().join("provider")
}

/// The credential-backup file for one provider, `~/.jade/credentials/<name>.json`.
pub fn credential_path(name: &str) -> PathBuf {
    jade_home().join("credentials").join(format!("{name}.json"))
}

/// `<prefix>/lib/jade/providers/`, derived from the running `jade` binary
/// (`<prefix>/bin/jade` → `<prefix>/lib/jade/providers`). `None` if unresolvable.
fn bundled_provider_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?;
    Some(prefix.join("lib").join("jade").join("providers"))
}

/// Directories a provider library can be sourced from, in priority order: the
/// `JADE_PROVIDERS_DIR` override, the installed pool, then the shipped bundle.
/// First match by name wins.
fn source_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var(ENV_PROVIDERS_DIR) {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs.push(pool_dir());
    if let Some(bundle) = bundled_provider_dir() {
        dirs.push(bundle);
    }
    dirs
}

/// An installable provider package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledProvider {
    /// The provider name — the library's file stem (`anthropic.so` → `anthropic`).
    pub name: String,
    /// Absolute path to the loadable library.
    pub path: PathBuf,
}

/// Enumerate the installable providers, deduped by name (first source wins).
pub fn installed() -> Vec<InstalledProvider> {
    let mut found: Vec<InstalledProvider> = Vec::new();
    for dir in source_dirs() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(LIB_EXT) {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if found.iter().any(|p| p.name == name) {
                continue;
            }
            found.push(InstalledProvider { name: name.to_owned(), path });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The source library path for a provider (pool/bundle/override), or `None`.
fn source_lib(name: &str) -> Option<PathBuf> {
    installed().into_iter().find(|p| p.name == name).map(|p| p.path)
}

/// The active provider's name — the stem of the single `.so` in the active slot,
/// or `None` if nothing is active. The runtime doesn't need the name (it loads
/// whatever library is there); the CLI reads it to display and validate.
pub fn active_provider() -> Option<String> {
    jade_runtime::provider::active_lib_path()
        .and_then(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
}

// ── activation (the one thing that writes the active slot) ─────────────────────

/// Make `name` the active provider: copy its library into the pool (if not
/// already there) and then into `active/` as the sole `.so`, and materialize its
/// credential to `active/config.json`. Enforces "one active provider" by clearing
/// the slot first.
pub fn activate(name: &str) -> Result<(), String> {
    let src = source_lib(name)
        .ok_or_else(|| format!("provider '{name}' is not installed"))?;

    let map_io = |e: io::Error| format!("activating '{name}': {e}");

    // Ensure a pool copy exists (the persistent installed set).
    let pool = pool_dir();
    std::fs::create_dir_all(&pool).map_err(map_io)?;
    let pooled = pool.join(format!("{name}.{LIB_EXT}"));
    if src != pooled {
        std::fs::copy(&src, &pooled).map_err(map_io)?;
    }

    // Clear the active slot (so exactly one provider is ever active), then place
    // this one plus its credential.
    let active = active_dir();
    clear_active().map_err(map_io)?;
    std::fs::create_dir_all(&active).map_err(map_io)?;
    std::fs::copy(&pooled, active.join(format!("{name}.{LIB_EXT}"))).map_err(map_io)?;

    // Materialize the credential from the stored file (a key passed as an env var
    // and never stored stays off disk — the provider reads it itself at runtime).
    let config = active.join("config.json");
    match std::fs::read(credential_path(name)) {
        Ok(bytes) => write_private(&config, &bytes).map_err(map_io)?,
        Err(_) => {
            let _ = std::fs::remove_file(&config); // no stored key → leave no stale one
        }
    }
    Ok(())
}

/// Clear the active slot — remove any provider `.so` and the config blob.
pub fn deactivate() -> io::Result<()> {
    clear_active()
}

fn clear_active() -> io::Result<()> {
    let dir = active_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_lib = path.extension().and_then(|e| e.to_str()) == Some(LIB_EXT);
        let is_config = path.file_name().and_then(|n| n.to_str()) == Some("config.json");
        if is_lib || is_config {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

// ── credentials ───────────────────────────────────────────────────────────────

/// The environment variable that supplies a provider's key, e.g.
/// `ANTHROPIC_API_KEY` for `anthropic`.
pub fn credential_env_var(name: &str) -> String {
    format!("{}_API_KEY", name.to_ascii_uppercase())
}

/// Wrap a bare API key in the credential envelope the provider packages read:
/// `{"api_key":"…"}`. Uses `serde_json`, so every JSON metacharacter — quotes,
/// backslashes, control bytes — is escaped and the key round-trips exactly.
pub fn envelope(api_key: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "api_key": api_key }))
        .expect("serializing a one-key string map cannot fail")
}

/// Store a provider's key (`0600`), then re-materialize the active slot if this
/// provider is the active one, so the new key takes effect immediately.
pub fn store_credential(name: &str, api_key: &str) -> io::Result<()> {
    let path = credential_path(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private(&path, &envelope(api_key))?;
    if active_provider().as_deref() == Some(name) {
        let config = active_dir().join("config.json");
        write_private(&config, &envelope(api_key))?;
    }
    Ok(())
}

/// Remove a provider's stored key (no error if it was never written).
pub fn remove_credential(name: &str) -> io::Result<()> {
    match std::fs::remove_file(credential_path(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether a credential is reachable for a provider (env var or stored file).
pub fn has_credential(name: &str) -> bool {
    if let Ok(key) = std::env::var(credential_env_var(name)) {
        if !key.is_empty() {
            return true;
        }
    }
    credential_path(name).exists()
}

/// Write `bytes` to `path` with owner-only permissions (`0600`).
///
/// Writes to a sibling temp file created `create_new` + `mode(0o600)` — a fresh
/// inode we own — then `rename`s over the target. `.mode()` only applies to a
/// newly created file, so opening an existing credential directly would keep its
/// old (possibly loose) permissions; the fresh inode + atomic rename enforces
/// `0600` regardless of any prior file and can't leave a partial credential.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("cred");
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    if let Err(e) = file.write_all(bytes).and_then(|()| file.flush()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(file);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
