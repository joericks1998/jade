use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

// ── Scaffolding ───────────────────────────────────────────────────────────────

// Mirrors the helper in src/project/tests.rs — there is no `tempfile`
// dev-dependency in this crate.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("jade_pkg_test_{tag}_{pid}_{n}"));
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

/// Serves canned responses and counts calls, so no test touches the network.
struct MockFetcher {
    responses: HashMap<String, Vec<u8>>,
    calls: AtomicUsize,
}

impl MockFetcher {
    fn new() -> Self {
        MockFetcher { responses: HashMap::new(), calls: AtomicUsize::new(0) }
    }

    fn with(mut self, url: &str, body: &[u8]) -> Self {
        self.responses.insert(url.to_string(), body.to_vec());
        self
    }

    /// Register the same body for every supported platform of a template URL.
    fn with_all_platforms(mut self, template: &str, body: &[u8]) -> Self {
        for p in fetch::SUPPORTED_PLATFORMS {
            let url = fetch::expand_platform(template, p);
            self.responses.insert(url, body.to_vec());
        }
        self
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Fetcher for MockFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| format!("could not fetch {url}: HTTP 404 Not Found"))
    }
}

fn manifest(src: &str) -> ProjectManifest {
    toml::from_str(src).expect("valid manifest toml")
}

/// The platform this test run resolves against, skipping tests that depend on
/// a supported host when run somewhere jade does not ship.
fn host() -> &'static str {
    fetch::platform_tag().expect("tests assume a supported host platform")
}

// ── resolve: local path dependencies ──────────────────────────────────────────

#[test]
fn resolve_path_dependency_hashes_the_local_file() {
    let tmp = TempDir::new("resolvepath");
    std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
    std::fs::write(tmp.path().join("vendor/libz.so"), b"fake so").unwrap();

    let m = manifest(
        r#"
        [project]
        name = "app"
        [dependencies.zlib]
        path = "vendor/libz.so"
        "#,
    );

    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    let pkg = &lock.packages[0];

    assert_eq!(pkg.name, "zlib");
    assert_eq!(pkg.version, LOCAL_VERSION, "an unversioned path dep is labelled local");
    assert_eq!(pkg.source, "path+vendor/libz.so");
    assert_eq!(pkg.abi, "jade");

    let artifact = pkg.artifacts.get(ANY_PLATFORM).expect("local artifact keyed as any");
    assert_eq!(artifact.file, "zlib.so", "installed under the dependency name, not the upstream one");
    assert_eq!(artifact.sha256, fetch::sha256_hex(b"fake so"));
    assert!(artifact.url.is_none(), "a local dep has nothing to download");
}

#[test]
fn resolve_path_dependency_errors_when_the_file_is_absent() {
    let tmp = TempDir::new("resolvemissing");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.gone]\npath = \"nope.so\"\n",
    );

    let err = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("gone"), "error should name the dependency: {err}");
    assert!(err.contains("nope.so"), "error should name the path: {err}");
}

// ── resolve: remote dependencies ──────────────────────────────────────────────

#[test]
fn resolve_platform_template_records_every_platform() {
    // The portability guarantee: adding on one machine must produce a lock the
    // others can install and verify from.
    let tmp = TempDir::new("resolvetmpl");
    let template = "https://example.com/tok-{platform}.so";
    let fetcher = MockFetcher::new().with_all_platforms(template, b"tok bytes");

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\nurl = \"{template}\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &fetcher).unwrap();
    let pkg = &lock.packages[0];

    assert_eq!(pkg.artifacts.len(), fetch::SUPPORTED_PLATFORMS.len());
    for p in fetch::SUPPORTED_PLATFORMS {
        let a = pkg.artifacts.get(*p).unwrap_or_else(|| panic!("missing platform {p}"));
        assert_eq!(a.sha256, fetch::sha256_hex(b"tok bytes"));
        assert_eq!(a.file, "tok.so", "platform tag is stripped from the installed name");
        assert_eq!(a.url.as_deref(), Some(fetch::expand_platform(template, p).as_str()));
    }
    // The template, not an expansion, is what the manifest said.
    assert_eq!(pkg.source, format!("url+{template}"));
}

#[test]
fn resolve_records_the_platforms_that_exist_and_skips_the_rest() {
    // Plenty of packages ship for a subset of platforms; that is not an error.
    let tmp = TempDir::new("resolvepartial");
    let template = "https://example.com/tok-{platform}.so";
    let fetcher = MockFetcher::new()
        .with(&fetch::expand_platform(template, "linux-x86_64"), b"linux build");

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\nurl = \"{template}\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &fetcher).unwrap();
    let pkg = &lock.packages[0];

    assert_eq!(pkg.artifacts.len(), 1);
    assert!(pkg.artifacts.contains_key("linux-x86_64"));
}

#[test]
fn resolve_errors_when_no_platform_yields_an_artifact() {
    let tmp = TempDir::new("resolvenone");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.ghost]\nversion = \"1.0.0\"\n\
         url = \"https://example.com/ghost-{platform}.so\"\n",
    );

    let err = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("ghost"), "error should name the dependency: {err}");
    assert!(err.contains("no artifact"), "unexpected message: {err}");
    // The per-platform failures are included so the user can see what was tried.
    assert!(err.contains("linux-x86_64"), "error should list attempts: {err}");
}

#[test]
fn resolve_plain_url_records_a_single_any_artifact() {
    let tmp = TempDir::new("resolveplain");
    let url = "https://example.com/tok.so";
    let fetcher = MockFetcher::new().with(url, b"one build");

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\nurl = \"{url}\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &fetcher).unwrap();
    let pkg = &lock.packages[0];

    assert_eq!(pkg.artifacts.len(), 1, "a non-template url is fetched once, not per platform");
    assert_eq!(fetcher.calls(), 1);
    assert_eq!(pkg.artifacts[ANY_PLATFORM].file, "tok.so");
}

#[test]
fn resolve_propagates_validation_errors() {
    let tmp = TempDir::new("resolveinvalid");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.bad]\nversion = \"^1.0\"\n\
         url = \"https://x/a.so\"\n",
    );

    let err = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("ranges are not supported"), "unexpected message: {err}");
}

#[test]
fn resolve_of_an_empty_manifest_is_an_empty_lock() {
    let tmp = TempDir::new("resolveempty");
    let m = manifest("[project]\nname = \"app\"\n");
    assert!(resolve(tmp.path(), &m, &MockFetcher::new()).unwrap().packages.is_empty());
}

// ── materialize ───────────────────────────────────────────────────────────────

/// A lock with one remote package whose artifact is registered for this host.
fn one_remote_package(body: &[u8]) -> (Lockfile, MockFetcher) {
    let url = "https://example.com/tok.so";
    let mut lock = Lockfile::new();
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ANY_PLATFORM.to_string(),
        LockedArtifact {
            url: Some(url.to_string()),
            file: "tok.so".to_string(),
            sha256: fetch::sha256_hex(body),
        },
    );
    lock.packages.push(LockedPackage {
        name: "tok".to_string(),
        version: "1.0.0".to_string(),
        source: lock::source_url(url),
        abi: "jade".to_string(),
        artifacts,
    });
    (lock, MockFetcher::new().with(url, body))
}

#[test]
fn materialize_downloads_into_libs() {
    let tmp = TempDir::new("matdownload");
    let (lock, fetcher) = one_remote_package(b"tok bytes");

    materialize(tmp.path(), &lock, &fetcher).unwrap();

    let installed = tmp.path().join("libs/tok-1.0.0/tok.so");
    assert_eq!(std::fs::read(&installed).unwrap(), b"tok bytes");
    assert_eq!(fetcher.calls(), 1);
}

#[test]
fn materialize_is_idempotent() {
    let tmp = TempDir::new("matidem");
    let (lock, fetcher) = one_remote_package(b"tok bytes");

    materialize(tmp.path(), &lock, &fetcher).unwrap();
    materialize(tmp.path(), &lock, &fetcher).unwrap();

    assert_eq!(fetcher.calls(), 1, "a verified artifact must not be re-downloaded");
}

#[test]
fn materialize_reverifies_an_existing_artifact() {
    // Presence is not trust: a `.so` in libs/ is dlopen'd, so a swapped or
    // corrupted file has to be caught rather than used because it is there.
    let tmp = TempDir::new("matreverify");
    let (lock, fetcher) = one_remote_package(b"tok bytes");

    let dir = tmp.path().join("libs/tok-1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tok.so"), b"corrupted").unwrap();

    materialize(tmp.path(), &lock, &fetcher).unwrap();

    assert_eq!(std::fs::read(dir.join("tok.so")).unwrap(), b"tok bytes");
    assert_eq!(fetcher.calls(), 1, "the corrupted file must trigger a re-fetch");
}

#[test]
fn materialize_rejects_a_checksum_mismatch() {
    let tmp = TempDir::new("matmismatch");
    let (lock, _) = one_remote_package(b"expected bytes");
    // Serve something other than what the lock pins.
    let fetcher = MockFetcher::new().with("https://example.com/tok.so", b"malicious bytes");

    let err = materialize(tmp.path(), &lock, &fetcher).unwrap_err();

    assert!(err.contains("checksum mismatch"), "unexpected message: {err}");
    assert!(err.contains("tok"), "error should name the dependency: {err}");
    assert!(err.contains(&fetch::sha256_hex(b"expected bytes")), "should show expected digest");
    assert!(err.contains(&fetch::sha256_hex(b"malicious bytes")), "should show actual digest");
    assert!(
        !tmp.path().join("libs/tok-1.0.0/tok.so").exists(),
        "a mismatched artifact must never be written to libs/"
    );
}

#[test]
fn materialize_copies_a_local_dependency() {
    let tmp = TempDir::new("matlocal");
    std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
    std::fs::write(tmp.path().join("vendor/libz.so"), b"local bytes").unwrap();

    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.zlib]\nversion = \"1.3.1\"\n\
         path = \"vendor/libz.so\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();

    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    let installed = tmp.path().join("libs/zlib-1.3.1/zlib.so");
    assert_eq!(std::fs::read(installed).unwrap(), b"local bytes");
}

#[test]
fn materialize_errors_when_this_platform_has_no_artifact() {
    let tmp = TempDir::new("matnoplat");
    let mut lock = Lockfile::new();
    let mut artifacts = BTreeMap::new();
    // A platform tag that is never the host.
    artifacts.insert(
        "some-other-platform".to_string(),
        LockedArtifact {
            url: Some("https://x/a.so".to_string()),
            file: "a.so".to_string(),
            sha256: fetch::sha256_hex(b"x"),
        },
    );
    lock.packages.push(LockedPackage {
        name: "tok".to_string(),
        version: "1.0.0".to_string(),
        source: lock::source_url("https://x/a.so"),
        abi: "jade".to_string(),
        artifacts,
    });

    let err = materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("tok"), "error should name the dependency: {err}");
    assert!(
        err.contains("some-other-platform"),
        "error should list what the lock does provide: {err}"
    );
}

#[test]
fn materialize_leaves_no_temp_file_behind() {
    let tmp = TempDir::new("mattmp");
    let (lock, fetcher) = one_remote_package(b"tok bytes");
    materialize(tmp.path(), &lock, &fetcher).unwrap();

    let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join("libs/tok-1.0.0"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".jade-install"))
        .collect();
    assert!(leftovers.is_empty(), "temp files must be renamed, not left in libs/");
}

// ── Local sources that change ─────────────────────────────────────────────────
//
// A `path` dependency points at a file the user rebuilds. The lock pins what it
// hashed to when it was added, and materialize is satisfied by anything in
// `libs/` matching that pin — so without the refresh below, a rebuilt library
// keeps loading as the copy it was on the day it was added, with no warning.

/// A project with one local dependency, resolved, locked and installed.
fn installed_local(tag: &str, bytes: &[u8]) -> (TempDir, ProjectManifest, Lockfile) {
    let tmp = TempDir::new(tag);
    std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
    std::fs::write(tmp.path().join("vendor/libengine.so"), bytes).unwrap();

    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.engine]\nversion = \"1.0.0\"\n\
         path = \"vendor/libengine.so\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    lock::write(tmp.path(), &lock).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    (tmp, m, lock)
}

#[test]
fn a_rebuilt_local_dependency_is_re_pinned() {
    let (tmp, _m, mut lock) = installed_local("localrebuild", b"old engine");
    std::fs::write(tmp.path().join("vendor/libengine.so"), b"new engine").unwrap();

    let changed = refresh_local(tmp.path(), &mut lock);

    assert_eq!(changed, vec!["engine".to_string()], "the rebuild must be reported");
    assert_eq!(
        lock.packages[0].artifacts[ANY_PLATFORM].sha256,
        fetch::sha256_hex(b"new engine"),
        "the lock must pin the source as it is now"
    );
}

#[test]
fn a_rebuilt_local_dependency_is_reinstalled() {
    // The whole bug in one test: rebuild the library, install again, and the
    // new bytes must be what sits in libs/.
    let (tmp, _m, mut lock) = installed_local("localreinstall", b"old engine");
    let installed = tmp.path().join("libs/engine-1.0.0/engine.so");
    assert_eq!(std::fs::read(&installed).unwrap(), b"old engine");

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"new engine").unwrap();
    refresh_local(tmp.path(), &mut lock);
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    assert_eq!(
        std::fs::read(&installed).unwrap(),
        b"new engine",
        "installing after a rebuild must replace the copy in libs/"
    );
}

#[test]
fn an_unchanged_local_dependency_is_left_alone() {
    let (tmp, _m, mut lock) = installed_local("localstable", b"engine");
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty(), "nothing changed");
    assert_eq!(lock, before, "an untouched source must not rewrite the lock");
}

#[test]
fn a_remote_dependency_is_never_re_pinned() {
    // Only a local path is mutable at its source. A URL either serves what the
    // lock pins or it does not, and re-pinning it would defeat the lock.
    let tmp = TempDir::new("remotepin");
    let (mut lock, _) = one_remote_package(b"tok bytes");
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty());
    assert_eq!(lock, before);
}

#[test]
fn a_local_source_that_disappeared_keeps_its_pin() {
    // libs/ still holds a verified copy, so a source that has moved away is no
    // reason to stop working — the pin just stands until it comes back.
    let (tmp, _m, mut lock) = installed_local("localgone", b"engine");
    std::fs::remove_file(tmp.path().join("vendor/libengine.so")).unwrap();
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty());
    assert_eq!(lock, before);
    materialize(tmp.path(), &lock, &MockFetcher::new())
        .expect("an already-installed dependency must still materialize");
}

#[test]
fn local_drift_reports_a_rebuilt_source() {
    let (tmp, _m, lock) = installed_local("localdrift", b"old engine");
    assert!(!local_drift(tmp.path(), &lock.packages[0]));

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"new engine").unwrap();
    assert!(local_drift(tmp.path(), &lock.packages[0]), "a rebuild is drift");
}

#[test]
fn locked_mode_rejects_a_rebuilt_local_dependency() {
    // The CI half: --locked must not quietly fix up the lock, and must not
    // install the stale digest either.
    let (tmp, _m, lock) = installed_local("localci", b"old engine");
    assert!(verify_local_unchanged(tmp.path(), &lock).is_ok());

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"new engine").unwrap();

    let err = verify_local_unchanged(tmp.path(), &lock).unwrap_err();
    assert!(err.contains("engine"), "error should name the dependency: {err}");
    assert!(err.contains("has changed"), "unexpected message: {err}");
    assert!(err.contains(&fetch::sha256_hex(b"old engine")), "should show the pin");
    assert!(err.contains(&fetch::sha256_hex(b"new engine")), "should show what is on disk");
    assert!(err.contains("jade pkg install"), "error should say how to recover: {err}");
}

// ── verify_in_sync ────────────────────────────────────────────────────────────

#[test]
fn verify_in_sync_accepts_a_matching_pair() {
    let tmp = TempDir::new("syncok");
    std::fs::write(tmp.path().join("a.so"), b"x").unwrap();
    let m = manifest("[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\n");
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();

    assert!(verify_in_sync(&m, &lock).is_ok());
}

#[test]
fn verify_in_sync_reports_a_manifest_only_dependency() {
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.new]\nversion = \"1.0.0\"\n\
         url = \"https://x/a.so\"\n",
    );
    let err = verify_in_sync(&m, &Lockfile::new()).unwrap_err();

    assert!(err.contains("not locked"), "unexpected message: {err}");
    assert!(err.contains("new"), "error should name the dependency: {err}");
    assert!(err.contains("jade pkg install"), "error should say how to recover: {err}");
}

#[test]
fn verify_in_sync_reports_a_lock_only_dependency() {
    let m = manifest("[project]\nname = \"app\"\n");
    let (lock, _) = one_remote_package(b"x");

    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("not in jade.toml"), "unexpected message: {err}");
    assert!(err.contains("tok"), "error should name the dependency: {err}");
}

#[test]
fn verify_in_sync_reports_both_directions_at_once() {
    // Editing a manifest by hand should not become a sequence of one-error runs.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.added]\nversion = \"1.0.0\"\n\
         url = \"https://x/a.so\"\n",
    );
    let (lock, _) = one_remote_package(b"x"); // locks "tok"

    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("added"), "should report the manifest-only dep: {err}");
    assert!(err.contains("tok"), "should report the lock-only dep: {err}");
}

// ── dependency_libraries ──────────────────────────────────────────────────────

#[test]
fn dependency_libraries_produces_a_lib_entry_per_package() {
    let (lock, _) = one_remote_package(b"tok bytes");
    let libs = dependency_libraries(&lock);

    let entry = libs.get("tok").expect("dependency exposed as a library");
    assert_eq!(entry.path, "libs/tok-1.0.0");
    assert_eq!(entry.files.as_ref().unwrap(), &vec!["tok.so".to_string()]);
}

#[test]
fn dependency_libraries_resolve_as_bare_name_imports() {
    // The end-to-end claim: a dependency reaches the compiler as an ordinary
    // [lib] entry, so `use tok` resolves through the unchanged shared resolver.
    let (lock, _) = one_remote_package(b"tok bytes");
    let libs = dependency_libraries(&lock);

    let resolved = crate::project::resolve_library_import(&libs, "tok", Path::new("/root"))
        .unwrap()
        .expect("bare name resolves to the dependency");

    assert_eq!(resolved.kind, crate::project::ImportKind::Native);
    assert_eq!(resolved.path, PathBuf::from("/root/libs/tok-1.0.0/tok.so"));
}

#[test]
fn dependency_libraries_picks_this_platform() {
    let mut lock = Lockfile::new();
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        host().to_string(),
        LockedArtifact {
            url: Some("https://x/native.so".to_string()),
            file: "native.so".to_string(),
            sha256: fetch::sha256_hex(b"x"),
        },
    );
    artifacts.insert(
        "some-other-platform".to_string(),
        LockedArtifact {
            url: Some("https://x/other.so".to_string()),
            file: "other.so".to_string(),
            sha256: fetch::sha256_hex(b"y"),
        },
    );
    lock.packages.push(LockedPackage {
        name: "multi".to_string(),
        version: "2.0.0".to_string(),
        source: lock::source_url("https://x/{platform}.so"),
        abi: "jade".to_string(),
        artifacts,
    });

    let libs = dependency_libraries(&lock);
    assert_eq!(
        libs["multi"].files.as_ref().unwrap(),
        &vec!["native.so".to_string()],
        "must select the host's artifact, not an arbitrary one"
    );
}

#[test]
fn dependency_libraries_skips_a_package_with_no_artifact_here() {
    // The useful error for this case comes from materialize; on the import path
    // the module is simply unknown.
    let mut lock = Lockfile::new();
    lock.packages.push(LockedPackage {
        name: "elsewhere".to_string(),
        version: "1.0.0".to_string(),
        source: lock::source_url("https://x/a.so"),
        abi: "jade".to_string(),
        artifacts: BTreeMap::new(),
    });

    assert!(dependency_libraries(&lock).is_empty());
}

// ── resolved_libraries ────────────────────────────────────────────────────────

#[test]
fn resolved_libraries_unions_manifest_libs_and_dependencies() {
    let tmp = TempDir::new("union");
    let (lock, _) = one_remote_package(b"tok bytes");
    lock::write(tmp.path(), &lock).unwrap();

    let m = manifest(
        "[project]\nname = \"app\"\n[lib.utils]\npath = \"src/utils\"\n",
    );

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 2);
    assert_eq!(libs["utils"].path, "src/utils");
    assert_eq!(libs["tok"].path, "libs/tok-1.0.0");
}

#[test]
fn resolved_libraries_lets_a_manifest_lib_shadow_a_dependency() {
    let tmp = TempDir::new("shadow");
    let (lock, _) = one_remote_package(b"tok bytes");
    lock::write(tmp.path(), &lock).unwrap();

    // A [lib.tok] declared locally must win over the locked dependency `tok`.
    let m = manifest("[project]\nname = \"app\"\n[lib.tok]\npath = \"src/mytok\"\n");

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 1);
    assert_eq!(libs["tok"].path, "src/mytok", "the local library must win");
}

#[test]
fn resolved_libraries_without_a_lock_is_just_the_manifest() {
    let tmp = TempDir::new("nolock");
    let m = manifest("[project]\nname = \"app\"\n[lib.utils]\npath = \"src/utils\"\n");

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 1);
    assert!(libs.contains_key("utils"));
}

#[test]
fn resolved_libraries_survives_a_corrupt_lock() {
    // The import path degrades to the manifest; `jade pkg install` is where a bad
    // lock produces a real error.
    let tmp = TempDir::new("badlock");
    std::fs::write(lock::path(tmp.path()), "not toml {{{").unwrap();
    let m = manifest("[project]\nname = \"app\"\n[lib.utils]\npath = \"src/utils\"\n");

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 1);
    assert!(libs.contains_key("utils"));
}

// ── Artifact naming ───────────────────────────────────────────────────────────

#[test]
fn artifacts_install_under_the_dependency_name() {
    // `use fastmath` resolves by stem, so an artifact left as
    // `libfastmath.dylib` would be unreachable under the name it was added as.
    assert_eq!(artifact_filename("fastmath", "vendor/libfastmath.dylib", "jade"), "fastmath.dylib");
    assert_eq!(artifact_filename("tok", "tok-linux-x86_64.so", "jade"), "tok.so");
    assert_eq!(artifact_filename("tok", "https_asset.so", "jade"), "tok.so");
    // A C dependency steps aside so its generated shim can own the import name.
    assert_eq!(artifact_filename("zlib", "libz.so", "c"), "zlib_native.so");
}

#[test]
fn artifact_filename_without_an_extension_is_the_bare_name() {
    assert_eq!(artifact_filename("thing", "some-artifact", "jade"), "thing");
}

#[test]
fn a_renamed_artifact_resolves_as_a_bare_import() {
    // The whole chain: resolve names the file after the dependency, and the
    // shared resolver then finds it from a bare `use`.
    let tmp = TempDir::new("renameresolve");
    std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
    std::fs::write(tmp.path().join("vendor/libfastmath.dylib"), b"so bytes").unwrap();

    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.fastmath]\nversion = \"1.0.0\"\n\
         path = \"vendor/libfastmath.dylib\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    let libs = dependency_libraries(&lock);

    let resolved = crate::project::resolve_library_import(&libs, "fastmath", tmp.path())
        .unwrap()
        .expect("bare import resolves");
    assert_eq!(resolved.kind, crate::project::ImportKind::Native);
    assert!(resolved.path.ends_with("libs/fastmath-1.0.0/fastmath.dylib"));
}

// ── Integration: a real dlopen ────────────────────────────────────────────────
//
// Everything above stops at the filesystem. These tests compile an actual
// shared library with `cc` and load it, closing the gap noted in
// src/native/tests.rs — without them, a regression anywhere in the install →
// resolve → dlopen chain is invisible to `cargo test`.

/// A minimal Jade-ABI package: one `triple` function.
const JADE_ABI_SOURCE: &str = r#"
#include <stdint.h>
#include <stddef.h>
typedef union { int64_t as_int; double as_float; uint8_t as_bool; const char* as_str; uint64_t as_nil; } JadeValData;
typedef struct { uint8_t tag; uint8_t _pad[7]; JadeValData data; } JadeVal;
typedef int (*JadeNativeFnPtr)(size_t, const JadeVal*, JadeVal*);
typedef struct { const char* name; JadeNativeFnPtr func; } JadeBinding;
typedef struct { const char* name; const JadeBinding* bindings; size_t binding_count; } JadeNativePkg;

static int fn_triple(size_t argc, const JadeVal* argv, JadeVal* out) {
    if (argc != 1 || argv[0].tag != 1) return 1;
    out->tag = 1;
    out->data.as_int = argv[0].data.as_int * 3;
    return 0;
}
static const JadeBinding BINDINGS[] = { { "triple", fn_triple } };
int jade_pkg_init(JadeNativePkg* out) {
    out->name = "probe";
    out->bindings = BINDINGS;
    out->binding_count = 1;
    return 0;
}
"#;

/// Whether a C compiler is usable here. CI images without one skip rather than
/// fail — these tests verify the loader, not the toolchain.
fn have_cc() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compile `source` into a shared library at `dir/<name>.<ext>`.
fn compile_lib(dir: &Path, name: &str, source: &str) -> PathBuf {
    let src = dir.join(format!("{name}.c"));
    std::fs::write(&src, source).unwrap();
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let out = dir.join(format!("{name}.{ext}"));

    let mut cc = std::process::Command::new("cc");
    if cfg!(target_os = "macos") {
        cc.arg("-dynamiclib");
    } else {
        cc.arg("-shared");
    }
    let result = cc.arg("-fPIC").arg(&src).arg("-o").arg(&out).output().unwrap();
    assert!(
        result.status.success(),
        "cc failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    out
}

#[test]
fn a_installed_jade_abi_package_loads_and_calls() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let tmp = TempDir::new("dlopenjade");
    let built = compile_lib(tmp.path(), "probe", JADE_ABI_SOURCE);
    let rel = built.file_name().unwrap().to_str().unwrap().to_string();

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.probe]\nversion = \"1.0.0\"\npath = \"{rel}\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    lock::write(tmp.path(), &lock).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    // Resolve the way an import would, then actually load it.
    let libs = resolved_libraries(tmp.path(), &m);
    let resolved = crate::project::resolve_library_import(&libs, "probe", tmp.path())
        .unwrap()
        .expect("dependency resolves as a bare import");
    assert_eq!(resolved.kind, crate::project::ImportKind::Native);

    let span = crate::frontend::error::Span { line: 0, col: 0 };
    let pkg = crate::native::load_native_package(&resolved.path, span)
        .expect("installed artifact must dlopen and expose jade_pkg_init");
    assert!(pkg.contains_key("triple"), "binding missing: {:?}", pkg.keys().collect::<Vec<_>>());
}

/// A plain C library — no `jade_pkg_init` anywhere in it.
const PLAIN_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t square(int64_t x) { return x * x; }
"#;

#[test]
fn a_generated_shim_makes_a_plain_c_library_loadable() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let tmp = TempDir::new("dlopenc");
    let built = compile_lib(tmp.path(), "plain", PLAIN_C_SOURCE);
    let rel = built.file_name().unwrap().to_str().unwrap().to_string();

    // The raw library must NOT be loadable as a package on its own — that is
    // the whole reason the shim exists.
    let span = crate::frontend::error::Span { line: 0, col: 0 };
    assert!(
        crate::native::load_native_package(&built, span).is_err(),
        "a plain C library should not satisfy the Jade package ABI"
    );

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.plain]\nversion = \"1.0.0\"\npath = \"{rel}\"\n\
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"int\"]\nret = \"int\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    lock::write(tmp.path(), &lock).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();
    build_c_shims(tmp.path(), &lock, &m).unwrap();

    let libs = resolved_libraries(tmp.path(), &m);
    let resolved = crate::project::resolve_library_import(&libs, "plain", tmp.path())
        .unwrap()
        .expect("c dependency resolves as a bare import");

    let pkg = crate::native::load_native_package(&resolved.path, span)
        .expect("the generated shim must dlopen and expose jade_pkg_init");
    assert!(pkg.contains_key("square"), "shim did not bind the declared symbol");
}

#[test]
fn a_shim_rebuild_is_skipped_when_nothing_changed() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let tmp = TempDir::new("shimcache");
    let built = compile_lib(tmp.path(), "plain", PLAIN_C_SOURCE);
    let rel = built.file_name().unwrap().to_str().unwrap().to_string();

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.plain]\nversion = \"1.0.0\"\npath = \"{rel}\"\n\
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"int\"]\nret = \"int\"\n"
    ));
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    build_c_shims(tmp.path(), &lock, &m).unwrap();
    let shim = tmp.path().join(LIBS_DIR).join("plain-1.0.0").join(shim_filename("plain"));
    let first = std::fs::metadata(&shim).unwrap().modified().unwrap();

    build_c_shims(tmp.path(), &lock, &m).unwrap();
    let second = std::fs::metadata(&shim).unwrap().modified().unwrap();

    assert_eq!(first, second, "an unchanged shim must not be recompiled");
}

/// The same plain C library, rebuilt with a different body.
const PLAIN_C_REBUILT: &str = r#"
#include <stdint.h>
int64_t square(int64_t x) { return x * x + 1; }
"#;

#[test]
fn a_shim_is_relinked_when_its_library_is_rebuilt() {
    // The shim's own source is identical either way — it is generated from the
    // declared symbols, which did not change. What changed is the library it
    // links against, so skipping on source alone would leave the shim bound to
    // the previous build.
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let tmp = TempDir::new("shimrelink");
    let built = compile_lib(tmp.path(), "plain", PLAIN_C_SOURCE);
    let rel = built.file_name().unwrap().to_str().unwrap().to_string();

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.plain]\nversion = \"1.0.0\"\npath = \"{rel}\"\n\
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"int\"]\nret = \"int\"\n"
    ));
    let mut lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();
    build_c_shims(tmp.path(), &lock, &m).unwrap();

    let shim = tmp.path().join(LIBS_DIR).join("plain-1.0.0").join(shim_filename("plain"));
    let first = std::fs::metadata(&shim).unwrap().modified().unwrap();

    // Rebuild the library, then install the way `jade pkg install` now does.
    compile_lib(tmp.path(), "plain", PLAIN_C_REBUILT);
    assert_eq!(refresh_local(tmp.path(), &mut lock), vec!["plain".to_string()]);
    lock::write(tmp.path(), &lock).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();
    build_c_shims(tmp.path(), &lock, &m).unwrap();

    let second = std::fs::metadata(&shim).unwrap().modified().unwrap();
    assert!(second > first, "a shim older than its library must be relinked");

    // And it still loads, against the new build.
    let span = crate::frontend::error::Span { line: 0, col: 0 };
    let libs = resolved_libraries(tmp.path(), &m);
    let resolved = crate::project::resolve_library_import(&libs, "plain", tmp.path())
        .unwrap()
        .expect("c dependency resolves as a bare import");
    let pkg = crate::native::load_native_package(&resolved.path, span)
        .expect("the relinked shim must dlopen");
    assert!(pkg.contains_key("square"));
}
