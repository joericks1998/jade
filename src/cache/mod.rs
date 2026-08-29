use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::compiler::tir::TProgram;
use crate::frontend::ast::Program;

/// Jade version baked in at compile time — used to invalidate cached artifacts
/// when the AST format changes between releases.
pub const JADE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Independent format version for cached AST/TIR/bytecode schemas.
/// Increment this whenever the shape of any serialized type changes so that
/// caches from prior builds are unconditionally rejected without needing a
/// version bump in Cargo.toml.
/// v3: async migration added JadeType::AsyncFn/Future, TStmt::AsyncFnDef,
///     TExprKind::Await, and Instr::Spawn/Await/Join — all serde-serialized.
/// v4: Expr::PromptDeref gained a `style` field recording whether the source
///     spelled it `?p`, `x.(?f)`, or `x~>f`.
/// v5: `|>` survives parsing as `Expr::Pipe` instead of being desugared into
///     `Expr::Call` or stored on `PromptDeref.constraint`. A v4 cache holds the
///     old shape, which infers to the same thing but cannot express a chain.
/// v6: the 1.2.x types. `JadeType` gained `Char` (1.2.1), `Bytes` (1.2.2), and
///     `Stream` (1.2.3); `Stmt`/`TStmt` gained `Yield`. Adding a variant in the
///     middle of a serde enum renumbers every variant after it, so a v5 cache
///     deserializes into the *wrong* types rather than failing loudly — which
///     showed up as an imported struct losing its field defaults.
/// v7: `JadeType` gained `Handle` (1.3.0), inserted after `Struct` and so ahead
///     of `Fn`/`AsyncFn`/`Future`/`Unknown` — the same mid-enum renumbering that
///     made v6 necessary, with the same silent-misread failure if not bumped.
pub const CACHE_FORMAT_VERSION: u32 = 10;

// ── Internal types ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct CacheMeta {
    version: String,
    source_path: String,
    hash: String,
    /// Missing in caches written before format versioning was added; defaults
    /// to 0, which always fails the CACHE_FORMAT_VERSION check.
    #[serde(default)]
    format_version: u32,
}

// ── Hashing ───────────────────────────────────────────────────────────────────

/// Returns the SHA-256 digest of a file's raw bytes, or `None` if the file
/// cannot be read.  Content-based (not mtime) so the hash survives git
/// checkouts and NFS-mounted filesystems.
pub fn file_hash(path: &Path) -> Option<[u8; 32]> {
    let contents = fs::read(path).ok()?;
    Some(Sha256::digest(contents).into())
}

fn hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

// ── Cache directory layout ────────────────────────────────────────────────────
//
//  ~/.jade/cache/
//    <2-char prefix>/
//      <64-char full hex>/
//        meta.json
//        ast.bin
//
// The two-level layout bounds per-directory entry counts on filesystems that
// slow down with large directories (HFS+, older ext4 configs, etc.).

/// The cache root for a given home directory.  Split out from `cache_root` so it
/// can be tested without touching the process environment.
fn cache_root_from_home(home: &Path) -> PathBuf {
    home.join(".jade").join("cache")
}

#[cfg(test)]
thread_local! {
    /// Test-only cache-root override.
    ///
    /// Tests must NOT redirect the cache by setting `HOME`: `setenv` mutates
    /// process-global state, and a concurrent `getenv` on any of the other test
    /// threads is a data race (hence `set_var` being `unsafe` since the 2024
    /// edition).  In practice that raced badly enough to make cache tests fail
    /// intermittently under the full suite.  A thread-local keeps each test's
    /// redirection private to its own thread, so no locking is needed and the
    /// tests run in parallel with everything else.
    static CACHE_ROOT_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the calling thread's cache-root override, returning the previous value.
#[cfg(test)]
pub(crate) fn set_cache_root_override(root: Option<PathBuf>) -> Option<PathBuf> {
    CACHE_ROOT_OVERRIDE.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), root))
}

pub(crate) fn cache_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = CACHE_ROOT_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return root;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    cache_root_from_home(Path::new(&home))
}

fn cache_dir(hash: &[u8; 32]) -> PathBuf {
    let h = hex(hash);
    cache_root().join(&h[..2]).join(&h)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Try to load a cached AST for the given content hash.
///
/// Returns `None` on any cache miss — file not found, version mismatch,
/// or corrupt data — so callers can unconditionally fall through to a
/// full lex + parse.
pub fn read_ast_cache(hash: &[u8; 32]) -> Option<Program> {
    let dir = cache_dir(hash);

    let meta_bytes = fs::read(dir.join("meta.json")).ok()?;
    let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;

    if meta.version != JADE_VERSION || meta.format_version != CACHE_FORMAT_VERSION {
        return None;
    }

    let ast_bytes = fs::read(dir.join("ast.bin")).ok()?;
    bincode::deserialize(&ast_bytes).ok()
}

/// Persist a parsed `Program` to the cache keyed by content hash.
///
/// All I/O errors are silently swallowed — a cache write failure must never
/// cause `jade run` to error out.
pub fn write_ast_cache(hash: &[u8; 32], source_path: &str, program: &Program) {
    let dir = cache_dir(hash);

    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let meta = CacheMeta {
        version: JADE_VERSION.to_string(),
        source_path: source_path.to_string(),
        hash: hex(hash),
        format_version: CACHE_FORMAT_VERSION,
    };

    if let Ok(meta_json) = serde_json::to_vec(&meta)
        && let Err(e) = fs::write(dir.join("meta.json"), meta_json)
    {
        #[cfg(debug_assertions)]
        eprintln!("jade cache: failed to write meta.json: {e}");
        let _ = e;
    }

    match bincode::serialize(program) {
        Ok(ast_bytes) => {
            if let Err(e) = fs::write(dir.join("ast.bin"), ast_bytes) {
                #[cfg(debug_assertions)]
                eprintln!("jade cache: failed to write ast.bin: {e}");
                let _ = e;
            }
        }
        #[cfg(debug_assertions)]
        Err(e) => eprintln!("jade cache: failed to serialize AST: {e}"),
        #[cfg(not(debug_assertions))]
        Err(_) => {}
    }
}

/// Try to load a cached `TProgram` for the given content hash.
///
/// Returns `None` on any miss (file absent, version mismatch, corrupt data).
/// The TIR cache is only written on a successful type-check run, so a hit
/// guarantees the source previously passed `jade check`.
pub fn read_tir_cache(hash: &[u8; 32]) -> Option<TProgram> {
    let dir = cache_dir(hash);

    let meta_bytes = fs::read(dir.join("meta.json")).ok()?;
    let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;
    if meta.version != JADE_VERSION || meta.format_version != CACHE_FORMAT_VERSION {
        return None;
    }

    let tir_bytes = fs::read(dir.join("tir.bin")).ok()?;
    bincode::deserialize(&tir_bytes).ok()
}

// ── Cache introspection ───────────────────────────────────────────────────────

/// Summary of a single cache entry, used by `jade cache info/clean`.
pub struct CacheEntry {
    pub version: String,
    pub dir: PathBuf,
    pub size_bytes: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// Walk `~/.jade/cache/` and return a summary of every entry found.
pub fn list_entries() -> Vec<CacheEntry> {
    let root = cache_root();
    let mut entries = Vec::new();

    let prefix_dirs = match fs::read_dir(&root) {
        Ok(d) => d,
        Err(_) => return entries,
    };

    for prefix_entry in prefix_dirs.flatten() {
        if !prefix_entry.path().is_dir() {
            continue;
        }
        let hash_dirs = match fs::read_dir(prefix_entry.path()) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for hash_entry in hash_dirs.flatten() {
            let dir = hash_entry.path();
            if !dir.is_dir() {
                continue;
            }

            // Read meta.json to get version.
            let meta_path = dir.join("meta.json");
            let version = if let Ok(bytes) = fs::read(&meta_path) {
                if let Ok(m) = serde_json::from_slice::<CacheMeta>(&bytes) {
                    m.version
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // Sum the size of all files in this entry's directory.
            let size_bytes = fs::read_dir(&dir)
                .map(|iter| iter.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
                .unwrap_or(0);

            let modified = fs::metadata(&meta_path).ok().and_then(|m| m.modified().ok());

            entries.push(CacheEntry { version, dir, size_bytes, modified });
        }
    }

    entries
}

/// Remove cache entries that match `predicate(entry)`.
/// Returns `(count_removed, bytes_freed)`.
pub fn purge_entries<F>(predicate: F, dry_run: bool) -> (usize, u64)
where
    F: Fn(&CacheEntry) -> bool,
{
    let entries = list_entries();
    let mut count = 0;
    let mut bytes = 0;
    for entry in &entries {
        if predicate(entry) {
            count += 1;
            bytes += entry.size_bytes;
            if !dry_run {
                let _ = fs::remove_dir_all(&entry.dir);
                // Clean up empty prefix dir.
                if let Some(parent) = entry.dir.parent() {
                    let _ = fs::remove_dir(parent); // silently fails if non-empty
                }
            }
        }
    }
    (count, bytes)
}

/// Persist a `TProgram` to `~/.jade/cache/<prefix>/<hash>/tir.bin`.
///
/// All I/O errors are silently swallowed — a cache write failure must never
/// cause `jade check` to error out.
pub fn write_tir_cache(hash: &[u8; 32], source_path: &str, tprogram: &TProgram) {
    let dir = cache_dir(hash);

    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let meta = CacheMeta {
        version: JADE_VERSION.to_string(),
        source_path: source_path.to_string(),
        hash: hex(hash),
        format_version: CACHE_FORMAT_VERSION,
    };
    if let Ok(meta_json) = serde_json::to_vec(&meta)
        && let Err(e) = fs::write(dir.join("meta.json"), meta_json)
    {
        #[cfg(debug_assertions)]
        eprintln!("jade cache: failed to write meta.json: {e}");
        let _ = e;
    }

    match bincode::serialize(tprogram) {
        Ok(tir_bytes) => {
            if let Err(e) = fs::write(dir.join("tir.bin"), tir_bytes) {
                #[cfg(debug_assertions)]
                eprintln!("jade cache: failed to write tir.bin: {e}");
                let _ = e;
            }
        }
        #[cfg(debug_assertions)]
        Err(e) => eprintln!("jade cache: failed to serialize TIR: {e}"),
        #[cfg(not(debug_assertions))]
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests;
