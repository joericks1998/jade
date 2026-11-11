//! Artifact acquisition and integrity, behind a trait so tests never touch the
//! network.

use sha2::{Digest, Sha256};

/// Fetches the bytes at a URL.
///
/// Package resolution and materialization take a `&dyn Fetcher` rather than
/// calling out directly, so the whole of [`crate::pkg`] is testable offline
/// against a map of canned responses.
pub trait Fetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// The real one: HTTPS via `reqwest` with rustls.
pub struct HttpFetcher {
    user_agent: String,
}

impl HttpFetcher {
    pub fn new() -> Self {
        HttpFetcher { user_agent: format!("jade/{}", env!("CARGO_PKG_VERSION")) }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        HttpFetcher::new()
    }
}

impl Fetcher for HttpFetcher {
    /// Download `url`, following redirects.
    ///
    /// The request runs on a dedicated thread. `main` drives the whole CLI
    /// inside a multi-threaded tokio runtime (`src/main.rs:161`), and
    /// `reqwest::blocking` builds a runtime of its own — starting one from
    /// inside another panics. Handing the call to a plain thread sidesteps that
    /// and, unlike `block_in_place`, still works when there is no runtime at
    /// all, which is how the unit tests call it. Downloads are a handful per
    /// install, so a thread apiece costs nothing.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let owned = url.to_string();
        let user_agent = self.user_agent.clone();

        let started = std::thread::Builder::new().name("jade-fetch".to_string()).spawn(
            move || -> Result<Vec<u8>, String> {
                let client = reqwest::blocking::Client::builder()
                    .user_agent(user_agent)
                    .build()
                    .map_err(|e| format!("could not build http client: {e}"))?;

                let resp = client
                    .get(&owned)
                    .send()
                    .map_err(|e| format!("could not fetch {owned}: {e}"))?;

                let status = resp.status();
                if !status.is_success() {
                    return Err(format!("could not fetch {owned}: HTTP {status}"));
                }

                resp.bytes()
                    .map(|b| b.to_vec())
                    .map_err(|e| format!("could not read response body from {owned}: {e}"))
            },
        );
        // `Builder` rather than `thread::spawn`, which unwraps: on a machine out
        // of threads an install would abort with a Rust panic instead of saying
        // which download failed and why.
        started
            .map_err(|e| format!("could not start a download thread for {url}: {e}"))?
            .join()
            .map_err(|_| format!("download thread panicked fetching {url}"))?
    }
}

// ── Integrity ─────────────────────────────────────────────────────────────────

/// Lowercase hex SHA-256 of `bytes`.
///
/// Same primitive as [`crate::cache::file_hash`], but over bytes already in
/// memory (a fresh download) and returning the hex form the lockfile stores.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Platforms ─────────────────────────────────────────────────────────────────

/// Platform tag used in release asset names and `jade.lock` — e.g.
/// `darwin-aarch64`.
///
/// This lives here rather than in `cli/upgrade.rs` (its original home) so there
/// is one table: `jade upgrade` picks a toolchain build with it, and the package
/// manager picks a dependency's artifact with it. Two copies would drift.
///
/// Reports the **host**. When `jade build --target` exists, dependency
/// selection must key off the target instead — the daemon protocol already
/// carries a `target` field for exactly that.
pub fn platform_tag() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        _ => None,
    }
}

/// Every platform a dependency may publish an artifact for.
///
/// `jade pkg add` walks this list to expand a `{platform}` URL template, so a lock
/// generated on one machine still describes the others. Kept in sync with
/// [`platform_tag`] — a tag that platform_tag can return but which is absent
/// here would be unresolvable at install time.
pub const SUPPORTED_PLATFORMS: &[&str] =
    &["darwin-aarch64", "darwin-x86_64", "linux-aarch64", "linux-x86_64"];

/// Substitute `{platform}` in a dependency URL.
///
/// A URL without the placeholder is returned unchanged — that is the
/// single-artifact case, which is legitimate for a dependency that only ships
/// one build.
pub fn expand_platform(url: &str, platform: &str) -> String {
    url.replace(crate::project::PLATFORM_PLACEHOLDER, platform)
}

#[cfg(test)]
mod tests;
