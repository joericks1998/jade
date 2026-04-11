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
