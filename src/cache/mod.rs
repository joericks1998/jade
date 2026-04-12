use std::{fs, path::{Path, PathBuf}};

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};

use crate::interpreter::ast::Program;
use crate::compiler::tir::TProgram;

/// Jade version baked in at compile time — used to invalidate cached artifacts
/// when the AST format changes between releases.
pub const JADE_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Internal types ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct CacheMeta {
    version: String,
    source_path: String,
    hash: String,
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

fn cache_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".jade").join("cache")
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

    // Reject artifacts from a different Jade version; the AST shape may have
    // changed and bincode would silently produce garbage.
    if meta.version != JADE_VERSION {
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
    };

    if let Ok(meta_json) = serde_json::to_vec(&meta) {
        let _ = fs::write(dir.join("meta.json"), meta_json);
    }

    if let Ok(ast_bytes) = bincode::serialize(program) {
        let _ = fs::write(dir.join("ast.bin"), ast_bytes);
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
    if meta.version != JADE_VERSION {
        return None;
    }

    let tir_bytes = fs::read(dir.join("tir.bin")).ok()?;
    bincode::deserialize(&tir_bytes).ok()
}

// ── Cache introspection ───────────────────────────────────────────────────────

/// Summary of a single cache entry, used by `jade cache info/clean`.
pub struct CacheEntry {
    pub hash: String,
    pub version: String,
    pub source_path: String,
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
        if !prefix_entry.path().is_dir() { continue; }
        let hash_dirs = match fs::read_dir(prefix_entry.path()) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for hash_entry in hash_dirs.flatten() {
            let dir = hash_entry.path();
            if !dir.is_dir() { continue; }

            // Read meta.json to get version and source path.
            let meta_path = dir.join("meta.json");
            let (version, source_path) = if let Ok(bytes) = fs::read(&meta_path) {
                if let Ok(m) = serde_json::from_slice::<CacheMeta>(&bytes) {
                    (m.version, m.source_path)
                } else {
                    (String::new(), String::new())
                }
            } else {
                (String::new(), String::new())
            };

            // Sum the size of all files in this entry's directory.
            let size_bytes = fs::read_dir(&dir)
                .map(|iter| {
                    iter.flatten()
                        .filter_map(|e| e.metadata().ok())
                        .map(|m| m.len())
                        .sum()
                })
                .unwrap_or(0);

            let modified = fs::metadata(&meta_path)
                .ok()
                .and_then(|m| m.modified().ok());

            let hash = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            entries.push(CacheEntry { hash, version, source_path, dir, size_bytes, modified });
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

    // Write (or refresh) meta.json alongside the TIR.
    let meta = CacheMeta {
        version: JADE_VERSION.to_string(),
        source_path: source_path.to_string(),
        hash: hex(hash),
    };
    if let Ok(meta_json) = serde_json::to_vec(&meta) {
        let _ = fs::write(dir.join("meta.json"), meta_json);
    }

    if let Ok(tir_bytes) = bincode::serialize(tprogram) {
        let _ = fs::write(dir.join("tir.bin"), tir_bytes);
    }
}
