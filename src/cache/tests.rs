use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compiler::tir::TProgram;
use crate::frontend::ast::Program;

/// Monotonic counter making each test's temp directory name unique.  A
/// timestamp is not sufficient: two tests starting in the same nanosecond tick
/// would collide and share a cache root.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Restores the previous cache-root override and deletes the temp dir on drop —
/// including when the test body panics, so one failing assertion cannot leak a
/// redirect onto the thread or leave directories behind.
struct TempCacheRoot {
    home: std::path::PathBuf,
    prev: Option<std::path::PathBuf>,
}

impl Drop for TempCacheRoot {
    fn drop(&mut self) {
        set_cache_root_override(self.prev.take());
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Run `f` with the cache root redirected into a fresh temp dir private to this
/// thread.  `f` receives the simulated home directory, whose cache root is
/// `home/.jade/cache` — mirroring the real `HOME`-derived layout.
///
/// Deliberately does not touch the `HOME` env var: see the note on
/// `CACHE_ROOT_OVERRIDE` in `cache/mod.rs`.
fn with_temp_home<F: FnOnce(&std::path::Path)>(f: F) {
    let unique = format!(
        "jade-cache-test-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let home = std::env::temp_dir().join(unique);
    let root = cache_root_from_home(&home);
    std::fs::create_dir_all(&root).unwrap();

    let prev = set_cache_root_override(Some(root));
    let _guard = TempCacheRoot { home: home.clone(), prev };

    f(&home);
}

// ── file_hash ─────────────────────────────────────────────────────────────

#[test]
fn file_hash_is_deterministic_and_content_based() {
    let path = std::env::temp_dir().join(format!("jade-fh-{}.jde", std::process::id()));
    std::fs::write(&path, b"let x = 1").unwrap();
    let h1 = file_hash(&path).unwrap();
    let h2 = file_hash(&path).unwrap();
    assert_eq!(h1, h2);

    // Different content → different hash.
    std::fs::write(&path, b"let x = 2").unwrap();
    let h3 = file_hash(&path).unwrap();
    assert_ne!(h1, h3);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_hash_missing_file_is_none() {
    let path = std::env::temp_dir().join("jade-does-not-exist-xyzzy.jde");
    assert!(file_hash(&path).is_none());
}

#[test]
fn file_hash_matches_known_sha256() {
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    let path = std::env::temp_dir().join(format!("jade-empty-{}.jde", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    let h = file_hash(&path).unwrap();
    assert_eq!(hex(&h), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    let _ = std::fs::remove_file(&path);
}

// ── hex ───────────────────────────────────────────────────────────────────

#[test]
fn hex_produces_64_lowercase_chars() {
    let hash = [0u8; 32];
    let s = hex(&hash);
    assert_eq!(s.len(), 64);
    assert!(s.chars().all(|c| c == '0'));

    let mut h2 = [0u8; 32];
    h2[0] = 0xAB;
    h2[31] = 0x0F;
    let s2 = hex(&h2);
    assert!(s2.starts_with("ab"));
    assert!(s2.ends_with("0f"));
}

// ── cache_root layout ─────────────────────────────────────────────────────

#[test]
fn cache_root_is_derived_from_home() {
    // Pure logic — no env mutation needed to pin the layout.
    let root = cache_root_from_home(std::path::Path::new("/home/someone"));
    assert_eq!(root, std::path::Path::new("/home/someone/.jade/cache"));
}

#[test]
fn cache_root_honours_the_temp_redirect() {
    with_temp_home(|home| {
        assert_eq!(cache_root(), home.join(".jade").join("cache"));
    });
}

#[test]
fn cache_dir_uses_two_char_prefix() {
    with_temp_home(|_home| {
        let mut hash = [0u8; 32];
        hash[0] = 0xde;
        hash[1] = 0xad;
        let dir = cache_dir(&hash);
        let full = hex(&hash);
        // .../cache/de/deadbeef...
        assert!(dir.ends_with(std::path::Path::new(&full)));
        assert_eq!(dir.parent().unwrap().file_name().unwrap(), "de");
    });
}

// ── AST cache round-trip ──────────────────────────────────────────────────

#[test]
fn ast_cache_write_then_read() {
    with_temp_home(|_home| {
        let hash = [7u8; 32];
        let prog = Program { stmts: vec![] };
        assert!(read_ast_cache(&hash).is_none(), "cold cache should miss");
        write_ast_cache(&hash, "test.jde", &prog);
        let loaded = read_ast_cache(&hash).expect("should hit after write");
        assert_eq!(loaded.stmts.len(), 0);
    });
}

#[test]
fn ast_cache_miss_for_unknown_hash() {
    with_temp_home(|_home| {
        let hash = [1u8; 32];
        write_ast_cache(&hash, "a.jde", &Program { stmts: vec![] });
        // Different hash → miss.
        let other = [2u8; 32];
        assert!(read_ast_cache(&other).is_none());
    });
}

// ── TIR cache round-trip ──────────────────────────────────────────────────

#[test]
fn tir_cache_write_then_read() {
    with_temp_home(|_home| {
        let hash = [9u8; 32];
        let tprog = TProgram { stmts: vec![] };
        assert!(read_tir_cache(&hash).is_none());
        write_tir_cache(&hash, "test.jde", &tprog);
        let loaded = read_tir_cache(&hash).expect("should hit after write");
        assert_eq!(loaded.stmts.len(), 0);
    });
}

// ── introspection: list / purge ───────────────────────────────────────────

#[test]
fn list_entries_reflects_written_cache() {
    with_temp_home(|_home| {
        assert_eq!(list_entries().len(), 0);
        write_ast_cache(&[3u8; 32], "x.jde", &Program { stmts: vec![] });
        write_ast_cache(&[4u8; 32], "y.jde", &Program { stmts: vec![] });
        let entries = list_entries();
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert_eq!(e.version, JADE_VERSION);
            assert!(e.size_bytes > 0);
        }
    });
}

#[test]
fn purge_dry_run_removes_nothing() {
    with_temp_home(|_home| {
        write_ast_cache(&[5u8; 32], "x.jde", &Program { stmts: vec![] });
        let (count, bytes) = purge_entries(|_| true, true);
        assert_eq!(count, 1);
        assert!(bytes > 0);
        // dry run → entry still present
        assert_eq!(list_entries().len(), 1);
    });
}

#[test]
fn purge_removes_matching_entries() {
    with_temp_home(|_home| {
        write_ast_cache(&[6u8; 32], "x.jde", &Program { stmts: vec![] });
        write_ast_cache(&[8u8; 32], "y.jde", &Program { stmts: vec![] });
        let (count, _) = purge_entries(|_| true, false);
        assert_eq!(count, 2);
        assert_eq!(list_entries().len(), 0);
    });
}

#[test]
fn purge_predicate_filters() {
    with_temp_home(|_home| {
        write_ast_cache(&[10u8; 32], "keep.jde", &Program { stmts: vec![] });
        // Predicate that never matches → nothing removed.
        let (count, _) = purge_entries(|_| false, false);
        assert_eq!(count, 0);
        assert_eq!(list_entries().len(), 1);
    });
}

// ── format version constants ──────────────────────────────────────────────

#[test]
fn cache_format_version_is_stable() {
    // Bumped to 11 in v1.4.1. `StructLiteral` gained a `base` field in both the
    // AST and the TIR, for the `...` of a copy-with literal. An older cache
    // holds the shape without it, and bincode has no field names to notice the
    // difference with — it would read the next value as the base and carry on.
    //
    // Bumped to 10 in v1.4.0. Three shapes changed at once: `Stmt::InterfaceDef`
    // and `TStmt::InterfaceDef` were removed from the middle of their enums, so
    // every later variant renumbers under bincode; `StructDef` swapped its
    // `decorators` field for `parents`; and `Instr::CatchMatches` is a new
    // opcode. A TIR cached by an older build would deserialize into something
    // this one cannot run, and would do it without complaining.
    assert_eq!(CACHE_FORMAT_VERSION, 11);
    assert!(!JADE_VERSION.is_empty());
}
