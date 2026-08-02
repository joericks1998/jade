use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

// Mirrors the helper in src/project/tests.rs — there is no `tempfile`
// dev-dependency in this crate.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("jade_lock_test_{tag}_{pid}_{n}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn artifact(file: &str, sha: &str) -> LockedArtifact {
    LockedArtifact {
        url: Some(format!("https://example.com/{file}")),
        file: file.to_string(),
        sha256: sha.to_string(),
    }
}

fn package(name: &str, version: &str) -> LockedPackage {
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        "darwin-aarch64".to_string(),
        artifact(&format!("{name}.dylib"), &"a".repeat(64)),
    );
    artifacts.insert(
        "linux-x86_64".to_string(),
        artifact(&format!("{name}.so"), &"b".repeat(64)),
    );
    LockedPackage {
        name: name.to_string(),
        version: version.to_string(),
        source: source_url("https://example.com/{platform}"),
        abi: "jade".to_string(),
        artifacts,
    }
}

// ── Round-tripping ────────────────────────────────────────────────────────────

#[test]
fn roundtrip_preserves_every_field() {
    let tmp = TempDir::new("roundtrip");
    let mut lock = Lockfile::new();
    lock.packages.push(package("fastmath", "1.2.0"));

    write(tmp.path(), &lock).unwrap();
    let read_back = read(tmp.path()).unwrap().expect("lock should exist");

    assert_eq!(read_back, lock);
}

#[test]
fn write_is_byte_stable_across_runs() {
    let tmp = TempDir::new("stable");
    let mut lock = Lockfile::new();
    lock.packages.push(package("beta", "0.1.0"));
    lock.packages.push(package("alpha", "2.0.0"));

    write(tmp.path(), &lock).unwrap();
    let first = std::fs::read(path(tmp.path())).unwrap();

    // Re-serializing the parsed lock must reproduce the same bytes, or the file
    // would churn in git on every install.
    let parsed = read(tmp.path()).unwrap().unwrap();
    write(tmp.path(), &parsed).unwrap();
    let second = std::fs::read(path(tmp.path())).unwrap();

    assert_eq!(first, second, "lockfile serialization must be deterministic");
}

#[test]
fn write_sorts_packages_by_name() {
    let tmp = TempDir::new("sorted");
    let mut lock = Lockfile::new();
    lock.packages.push(package("zeta", "1.0.0"));
    lock.packages.push(package("alpha", "1.0.0"));
    lock.packages.push(package("mid", "1.0.0"));

    write(tmp.path(), &lock).unwrap();
    let names: Vec<String> =
        read(tmp.path()).unwrap().unwrap().packages.iter().map(|p| p.name.clone()).collect();

    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
}

#[test]
fn every_platform_artifact_survives_the_roundtrip() {
    // The portability guarantee: a lock written on one platform must still
    // describe the others, or CI on a different OS has nothing to verify.
    let tmp = TempDir::new("platforms");
    let mut lock = Lockfile::new();
    lock.packages.push(package("tok", "3.1.4"));

    write(tmp.path(), &lock).unwrap();
    let pkg = read(tmp.path()).unwrap().unwrap().packages.remove(0);

    assert_eq!(pkg.artifacts.len(), 2);
    assert_eq!(pkg.artifact("linux-x86_64").unwrap().sha256, "b".repeat(64));
    assert_eq!(pkg.artifact("darwin-aarch64").unwrap().file, "tok.dylib");
    assert!(pkg.artifact("windows-x86_64").is_none());
}

// ── Absence, corruption, and version skew ─────────────────────────────────────

#[test]
fn missing_lockfile_is_not_an_error() {
    let tmp = TempDir::new("absent");
    assert!(read(tmp.path()).unwrap().is_none());
}

#[test]
fn malformed_lockfile_is_an_error_not_a_silent_reset() {
    let tmp = TempDir::new("malformed");
    std::fs::write(path(tmp.path()), "this is not toml {{{").unwrap();

    let err = read(tmp.path()).unwrap_err();
    assert!(err.contains("invalid"), "unexpected message: {err}");
    assert!(err.contains(LOCK_FILE), "error should name the file: {err}");
}

#[test]
fn future_lock_version_is_rejected() {
    let tmp = TempDir::new("future");
    std::fs::write(
        path(tmp.path()),
        format!("version = {}\n", LOCK_VERSION + 1),
    )
    .unwrap();

    let err = read(tmp.path()).unwrap_err();
    assert!(
        err.contains(&(LOCK_VERSION + 1).to_string()),
        "error should name the version it found: {err}"
    );
    assert!(err.contains("jade pkg install"), "error should say how to recover: {err}");
}

#[test]
fn empty_lockfile_parses_as_no_packages() {
    let tmp = TempDir::new("empty");
    std::fs::write(path(tmp.path()), format!("version = {LOCK_VERSION}\n")).unwrap();

    let lock = read(tmp.path()).unwrap().unwrap();
    assert!(lock.packages.is_empty());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[test]
fn source_tags_are_distinguishable() {
    assert_eq!(source_path("vendor/libz.so"), "path+vendor/libz.so");
    assert_eq!(source_url("https://x/y.so"), "url+https://x/y.so");
}

#[test]
fn install_dir_joins_name_and_version() {
    assert_eq!(package("fastmath", "1.2.0").install_dir(), "fastmath-1.2.0");
}

#[test]
fn get_finds_a_package_by_name() {
    let mut lock = Lockfile::new();
    lock.packages.push(package("a", "1.0.0"));
    lock.packages.push(package("b", "2.0.0"));

    assert_eq!(lock.get("b").unwrap().version, "2.0.0");
    assert!(lock.get("missing").is_none());
}
