use super::*;
use std::sync::Mutex;

use crate::frontend::ast::Program;
use crate::compiler::tir::TProgram;

// Tests that mutate the process-global `HOME` env var (to redirect the cache
// root into a temp dir) must not run concurrently. Serialize them behind this
// lock. Pure-logic tests (hashing, path shape) don't touch HOME and run freely.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with HOME pointed at a fresh unique temp dir, restoring the previous
/// HOME afterwards. Serialized via HOME_LOCK.
fn with_temp_home<F: FnOnce(&std::path::Path)>(f: F) {
    let _guard = HOME_LOCK.lock().unwrap();
    let prev = std::env::var("HOME").ok();

    let unique = format!(
        "jade-cache-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let home = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: guarded by HOME_LOCK so no other cache test races on HOME.
    unsafe { std::env::set_var("HOME", &home) };

    f(&home);

    // restore + cleanup
    // SAFETY: still holding HOME_LOCK.
    unsafe {
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
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
    assert_eq!(
        hex(&h),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
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
fn cache_root_ends_with_jade_cache() {
    with_temp_home(|home| {
        let root = cache_root();
        assert_eq!(root, home.join(".jade").join("cache"));
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
    assert_eq!(CACHE_FORMAT_VERSION, 3);
    assert!(!JADE_VERSION.is_empty());
}
