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
    std::fs::write(tmp.path().join("vendor/libz.so"), b"\x7fELFfake so").unwrap();

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
    assert_eq!(
        artifact.file, "zlib.so",
        "installed under the dependency name, not the upstream one"
    );
    assert_eq!(artifact.sha256, fetch::sha256_hex(b"\x7fELFfake so"));
    assert!(artifact.url.is_none(), "a local dep has nothing to download");
}

#[test]
fn resolve_path_dependency_errors_when_the_file_is_absent() {
    let tmp = TempDir::new("resolvemissing");
    let m = manifest("[project]\nname = \"app\"\n[dependencies.gone]\npath = \"nope.so\"\n");

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
    let fetcher = MockFetcher::new().with_all_platforms(template, b"\x7fELFtok bytes");

    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\nurl = \"{template}\"\n"
    ));

    let lock = resolve(tmp.path(), &m, &fetcher).unwrap();
    let pkg = &lock.packages[0];

    assert_eq!(pkg.artifacts.len(), fetch::SUPPORTED_PLATFORMS.len());
    for p in fetch::SUPPORTED_PLATFORMS {
        let a = pkg.artifacts.get(*p).unwrap_or_else(|| panic!("missing platform {p}"));
        assert_eq!(a.sha256, fetch::sha256_hex(b"\x7fELFtok bytes"));
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
        .with(&fetch::expand_platform(template, "linux-x86_64"), b"\x7fELFlinux build");

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
    let fetcher = MockFetcher::new().with(url, b"\x7fELFone build");

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
    let (lock, fetcher) = one_remote_package(b"\x7fELFtok bytes");

    materialize(tmp.path(), &lock, &fetcher).unwrap();

    let installed = tmp.path().join("libs/tok-1.0.0/tok.so");
    assert_eq!(std::fs::read(&installed).unwrap(), b"\x7fELFtok bytes");
    assert_eq!(fetcher.calls(), 1);
}

#[test]
fn materialize_is_idempotent() {
    let tmp = TempDir::new("matidem");
    let (lock, fetcher) = one_remote_package(b"\x7fELFtok bytes");

    materialize(tmp.path(), &lock, &fetcher).unwrap();
    materialize(tmp.path(), &lock, &fetcher).unwrap();

    assert_eq!(fetcher.calls(), 1, "a verified artifact must not be re-downloaded");
}

#[test]
fn materialize_reverifies_an_existing_artifact() {
    // Presence is not trust: a `.so` in libs/ is dlopen'd, so a swapped or
    // corrupted file has to be caught rather than used because it is there.
    let tmp = TempDir::new("matreverify");
    let (lock, fetcher) = one_remote_package(b"\x7fELFtok bytes");

    let dir = tmp.path().join("libs/tok-1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tok.so"), b"\x7fELFcorrupted").unwrap();

    materialize(tmp.path(), &lock, &fetcher).unwrap();

    assert_eq!(std::fs::read(dir.join("tok.so")).unwrap(), b"\x7fELFtok bytes");
    assert_eq!(fetcher.calls(), 1, "the corrupted file must trigger a re-fetch");
}

#[test]
fn materialize_rejects_a_checksum_mismatch() {
    let tmp = TempDir::new("matmismatch");
    let (lock, _) = one_remote_package(b"\x7fELFexpected bytes");
    // Serve something other than what the lock pins.
    let fetcher = MockFetcher::new().with("https://example.com/tok.so", b"\x7fELFmalicious bytes");

    let err = materialize(tmp.path(), &lock, &fetcher).unwrap_err();

    assert!(err.contains("checksum mismatch"), "unexpected message: {err}");
    assert!(err.contains("tok"), "error should name the dependency: {err}");
    assert!(
        err.contains(&fetch::sha256_hex(b"\x7fELFexpected bytes")),
        "should show expected digest"
    );
    assert!(
        err.contains(&fetch::sha256_hex(b"\x7fELFmalicious bytes")),
        "should show actual digest"
    );
    assert!(
        !tmp.path().join("libs/tok-1.0.0/tok.so").exists(),
        "a mismatched artifact must never be written to libs/"
    );
}

// Every artifact in these tests opens with `\x7fELF` because `materialize`
// refuses anything the dynamic loader could not open. The bytes after it are
// arbitrary — nothing here loads a library — but the four in front have to be
// there, and a new fixture without them fails on the shape check rather than on
// whatever it meant to test.

#[test]
fn materialize_rejects_something_that_is_not_a_library() {
    // The failure this prevents: a header compiled by mistake installs cleanly,
    // links, builds, and is first refused by `dlopen` in the finished program.
    let (lock, _) = one_remote_package(b"\x7fELFtok bytes");
    let fetcher = MockFetcher::new().with("https://example.com/tok.so", b"not an object file");
    let tmp = TempDir::new("materialize_not_a_library");

    let err = materialize(tmp.path(), &lock, &fetcher).unwrap_err();

    assert!(err.contains("is not a shared library"), "unexpected message: {err}");
    assert!(err.contains("tok"), "error should name the dependency: {err}");
    assert!(
        !tmp.path().join("libs/tok-1.0.0/tok.so").exists(),
        "an unloadable artifact must never be written to libs/"
    );
}

#[test]
fn the_object_check_accepts_every_shape_a_platform_can_load() {
    use super::bindgen::bytes_are_loadable_object;
    // Mach-O 64-bit, both byte orders; a universal binary; and ELF.
    assert!(bytes_are_loadable_object(b"\xcf\xfa\xed\xfe rest"));
    assert!(bytes_are_loadable_object(b"\xfe\xed\xfa\xcf rest"));
    assert!(bytes_are_loadable_object(b"\xca\xfe\xba\xbe rest"));
    assert!(bytes_are_loadable_object(b"\x7fELF rest"));

    // A precompiled header, a static archive, C source, and a file too short to
    // have a magic number at all.
    assert!(!bytes_are_loadable_object(b"CPCH rest"));
    assert!(!bytes_are_loadable_object(b"!<arch>\n"));
    assert!(!bytes_are_loadable_object(b"int add(int a, int b);"));
    assert!(!bytes_are_loadable_object(b"\x7fEL"));
    assert!(!bytes_are_loadable_object(b""));
}

#[test]
fn materialize_copies_a_local_dependency() {
    let tmp = TempDir::new("matlocal");
    std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
    std::fs::write(tmp.path().join("vendor/libz.so"), b"\x7fELFlocal bytes").unwrap();

    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.zlib]\nversion = \"1.3.1\"\n\
         path = \"vendor/libz.so\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();

    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    let installed = tmp.path().join("libs/zlib-1.3.1/zlib.so");
    assert_eq!(std::fs::read(installed).unwrap(), b"\x7fELFlocal bytes");
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
    let (lock, fetcher) = one_remote_package(b"\x7fELFtok bytes");
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
    let (tmp, _m, mut lock) = installed_local("localrebuild", b"\x7fELFold engine");
    std::fs::write(tmp.path().join("vendor/libengine.so"), b"\x7fELFnew engine").unwrap();

    let changed = refresh_local(tmp.path(), &mut lock);

    assert_eq!(changed, vec!["engine".to_string()], "the rebuild must be reported");
    assert_eq!(
        lock.packages[0].artifacts[ANY_PLATFORM].sha256,
        fetch::sha256_hex(b"\x7fELFnew engine"),
        "the lock must pin the source as it is now"
    );
}

#[test]
fn a_rebuilt_local_dependency_is_reinstalled() {
    // The whole bug in one test: rebuild the library, install again, and the
    // new bytes must be what sits in libs/.
    let (tmp, _m, mut lock) = installed_local("localreinstall", b"\x7fELFold engine");
    let installed = tmp.path().join("libs/engine-1.0.0/engine.so");
    assert_eq!(std::fs::read(&installed).unwrap(), b"\x7fELFold engine");

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"\x7fELFnew engine").unwrap();
    refresh_local(tmp.path(), &mut lock);
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();

    assert_eq!(
        std::fs::read(&installed).unwrap(),
        b"\x7fELFnew engine",
        "installing after a rebuild must replace the copy in libs/"
    );
}

#[test]
fn an_unchanged_local_dependency_is_left_alone() {
    let (tmp, _m, mut lock) = installed_local("localstable", b"\x7fELFengine");
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty(), "nothing changed");
    assert_eq!(lock, before, "an untouched source must not rewrite the lock");
}

#[test]
fn a_remote_dependency_is_never_re_pinned() {
    // Only a local path is mutable at its source. A URL either serves what the
    // lock pins or it does not, and re-pinning it would defeat the lock.
    let tmp = TempDir::new("remotepin");
    let (mut lock, _) = one_remote_package(b"\x7fELFtok bytes");
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty());
    assert_eq!(lock, before);
}

#[test]
fn a_local_source_that_disappeared_keeps_its_pin() {
    // libs/ still holds a verified copy, so a source that has moved away is no
    // reason to stop working — the pin just stands until it comes back.
    let (tmp, _m, mut lock) = installed_local("localgone", b"\x7fELFengine");
    std::fs::remove_file(tmp.path().join("vendor/libengine.so")).unwrap();
    let before = lock.clone();

    assert!(refresh_local(tmp.path(), &mut lock).is_empty());
    assert_eq!(lock, before);
    materialize(tmp.path(), &lock, &MockFetcher::new())
        .expect("an already-installed dependency must still materialize");
}

#[test]
fn local_drift_reports_a_rebuilt_source() {
    let (tmp, _m, lock) = installed_local("localdrift", b"\x7fELFold engine");
    assert!(!local_drift(tmp.path(), &lock.packages[0]));

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"\x7fELFnew engine").unwrap();
    assert!(local_drift(tmp.path(), &lock.packages[0]), "a rebuild is drift");
}

#[test]
fn locked_mode_rejects_a_rebuilt_local_dependency() {
    // The CI half: --locked must not quietly fix up the lock, and must not
    // install the stale digest either.
    let (tmp, _m, lock) = installed_local("localci", b"\x7fELFold engine");
    assert!(verify_local_unchanged(tmp.path(), &lock).is_ok());

    std::fs::write(tmp.path().join("vendor/libengine.so"), b"\x7fELFnew engine").unwrap();

    let err = verify_local_unchanged(tmp.path(), &lock).unwrap_err();
    assert!(err.contains("engine"), "error should name the dependency: {err}");
    assert!(err.contains("has changed"), "unexpected message: {err}");
    assert!(err.contains(&fetch::sha256_hex(b"\x7fELFold engine")), "should show the pin");
    assert!(err.contains(&fetch::sha256_hex(b"\x7fELFnew engine")), "should show what is on disk");
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

#[test]
fn verify_in_sync_reports_an_abi_the_lock_disagrees_with() {
    // The two can name the same dependency and disagree about what it *is*.
    // Comparing names alone let a lock saying "jade" outlive a manifest
    // corrected to "c": the build read the lock, skipped the binding shim, and
    // loaded a plain C library as though it were a Jade package — which the
    // dynamic loader refused, in the finished program, for a missing symbol.
    let tmp = TempDir::new("syncabi");
    std::fs::write(tmp.path().join("a.so"), b"\x7fELFx").unwrap();
    // `resolve` refuses a C dependency with no symbols, so the manifest carries
    // one; the disagreement under test is the ABI, not the table.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\"]\nret = \"int\"\n",
    );
    let mut lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    lock.packages[0].abi = "jade".to_string();

    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("different ABI"), "unexpected message: {err}");
    assert!(err.contains("jade.toml says c"), "should say what the manifest wants: {err}");
    assert!(err.contains("jade.lock says jade"), "should say what the lock has: {err}");
    assert!(err.contains("jade pkg install"), "error should say how to recover: {err}");
}

#[test]
fn a_c_dependency_with_no_symbols_is_refused_rather_than_installed_raw() {
    // Skipping it installed the bare C library, reported success, and left the
    // program to fail at run time on a missing `jade_pkg_init` — a message that
    // names a symbol rather than the fact that nothing was ever bound.
    // The state this guards is a lock and a manifest written at different
    // times: `resolve` refuses a symbol-less C dependency, but `ensure_ready`
    // reads an *existing* lock rather than re-resolving, so a lock produced
    // when the table was there outlives a manifest edit that removed it.
    let tmp = TempDir::new("noshim");
    std::fs::write(tmp.path().join("a.so"), b"\x7fELFx").unwrap();
    let with_symbols = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\"]\nret = \"int\"\n",
    );
    let lock = resolve(tmp.path(), &with_symbols, &MockFetcher::new()).unwrap();
    let m = manifest("[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n");

    let err = build_c_shims(tmp.path(), &lock, &m).unwrap_err();
    assert!(err.contains("no symbols"), "unexpected message: {err}");
    assert!(err.contains("--header"), "error should name the fix: {err}");
}

// ── unresolved symbols ────────────────────────────────────────────────────────

#[test]
fn a_symbol_may_be_written_as_a_bare_question_mark() {
    // `jade pkg add` writes the names it read out of the export table with `"?"`
    // where the prototype goes, so a library with no header still produces a
    // manifest listing every function rather than nothing at all.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols]\nadd = \"?\"\nscale = \"?\"\n",
    );
    let entry = &m.dependencies.as_ref().unwrap()["a"];

    assert_eq!(entry.unresolved_symbols(), vec!["add", "scale"]);
    assert!(entry.symbols.as_ref().unwrap()["add"].is_unresolved());
}

#[test]
fn a_table_and_a_placeholder_can_sit_in_one_symbols_table() {
    // Binding a large header a piece at a time with `--only` leaves exactly
    // this: some symbols filled in, the rest still blank.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols]\nscale = \"?\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\", \"int\"]\nret = \"int\"\n",
    );
    let entry = &m.dependencies.as_ref().unwrap()["a"];

    assert_eq!(entry.unresolved_symbols(), vec!["scale"]);
    assert!(!entry.symbols.as_ref().unwrap()["add"].is_unresolved());
}

#[test]
fn a_placeholder_in_an_argument_counts_as_unresolved() {
    // A half-corrected entry is caught here rather than by `cc` failing on `?`
    // as a type name, several stages further on and in a generated file.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\", \"?\"]\nret = \"int\"\n",
    );

    assert_eq!(m.dependencies.as_ref().unwrap()["a"].unresolved_symbols(), vec!["add"]);
}

#[test]
fn a_symbol_string_that_is_not_the_placeholder_is_rejected() {
    // `add = "int"` is someone guessing at a shorthand that does not exist, and
    // the message has to say what the two accepted forms are.
    let err = toml::from_str::<ProjectManifest>(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols]\nadd = \"int\"\n",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("args"), "should name the table form: {err}");
    assert!(err.contains('?'), "should name the placeholder form: {err}");
}

#[test]
fn a_malformed_symbol_table_still_names_the_missing_field() {
    // The custom deserializer replaced a derived one. Written by hand rather
    // than with `#[serde(untagged)]` precisely so this message survives:
    // untagged reports every failure as "data did not match any variant".
    let err = toml::from_str::<ProjectManifest>(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\"]\n",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("ret"), "should still name the missing field: {err}");
}

#[test]
fn a_dependency_with_placeholder_symbols_is_refused_before_the_shim() {
    // The whole point of writing `"?"` is to have something concrete to refuse.
    // Passing it through would reach `cc` as a type name, in a generated file
    // the user never wrote, with nothing pointing back at the manifest.
    let tmp = TempDir::new("unresolved");
    std::fs::write(tmp.path().join("a.so"), b"\x7fELFx").unwrap();
    // `resolve` only asks that the table is non-empty, which a placeholder
    // satisfies — the refusal under test is the one further on.
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols]\nadd = \"?\"\nscale = \"?\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();

    let err = build_c_shims(tmp.path(), &lock, &m).unwrap_err();
    assert!(err.contains("no signature yet"), "unexpected message: {err}");
    assert!(err.contains("add, scale"), "should name the symbols: {err}");
    assert!(err.contains("[dependencies.a.symbols.add]"), "should show the shape to write: {err}");
    assert!(err.contains("jade pkg bind"), "should name the other way out: {err}");
}

#[test]
fn a_fully_bound_dependency_reports_nothing_unresolved() {
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.a]\npath = \"a.so\"\nabi = \"c\"\n\
         [dependencies.a.symbols.add]\nargs = [\"int\"]\nret = \"int\"\n",
    );

    assert!(check_symbols_resolved(&m).is_ok());
    assert!(unresolved_report("a", &m.dependencies.as_ref().unwrap()["a"]).is_none());
}

// ── dependency_libraries ──────────────────────────────────────────────────────

#[test]
fn dependency_libraries_produces_a_lib_entry_per_package() {
    let (lock, _) = one_remote_package(b"\x7fELFtok bytes");
    let libs = dependency_libraries(&lock);

    let entry = libs.get("tok").expect("dependency exposed as a library");
    assert_eq!(entry.path, "libs/tok-1.0.0");
    assert_eq!(entry.files.as_ref().unwrap(), &vec!["tok.so".to_string()]);
}

#[test]
fn dependency_libraries_resolve_as_bare_name_imports() {
    // The end-to-end claim: a dependency reaches the compiler as an ordinary
    // [lib] entry, so `use tok` resolves through the unchanged shared resolver.
    let (lock, _) = one_remote_package(b"\x7fELFtok bytes");
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
    let (lock, _) = one_remote_package(b"\x7fELFtok bytes");
    lock::write(tmp.path(), &lock).unwrap();

    let m = manifest("[project]\nname = \"app\"\n[lib.utils]\npath = \"src/utils\"\n");

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 2);
    assert_eq!(libs["utils"].path, "src/utils");
    assert_eq!(libs["tok"].path, "libs/tok-1.0.0");
}

#[test]
fn resolved_libraries_lets_a_manifest_lib_shadow_a_dependency() {
    let tmp = TempDir::new("shadow");
    let (lock, _) = one_remote_package(b"\x7fELFtok bytes");
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
    std::process::Command::new("cc").arg("--version").output().is_ok_and(|o| o.status.success())
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
    assert!(result.status.success(), "cc failed: {}", String::from_utf8_lossy(&result.stderr));
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
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"scalar:int64_t\"]\n\
         ret = \"scalar:int64_t\"\n"
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
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"scalar:int64_t\"]\n\
         ret = \"scalar:int64_t\"\n"
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
         abi = \"c\"\n[dependencies.plain.symbols.square]\nargs = [\"scalar:int64_t\"]\n\
         ret = \"scalar:int64_t\"\n"
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

// ── Bundling a dependency beside an artifact ──────────────────────────────

#[test]
fn bundling_copies_a_whole_install_directory() {
    // A C dependency's install dir holds two files — the generated shim and the
    // library it wraps — and the shim finds the second through a loader-relative
    // reference. Copying only the importable one leaves that pointing at nothing.
    let tmp = TempDir::new("bundle_whole");
    let libs = tmp.path().join("libs");
    let src = libs.join("zlib-1.3.1");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("zlib.dylib"), b"shim").unwrap();
    std::fs::write(src.join("zlib_native.dylib"), b"target").unwrap();

    let ship = tmp.path().join("out");
    std::fs::create_dir_all(&ship).unwrap();
    let written = bundle_beside_artifact(&ship.join("app"), &libs).unwrap();

    assert_eq!(written, ["zlib-1.3.1"]);
    let dest = ship.join("libs").join("zlib-1.3.1");
    assert!(dest.join("zlib.dylib").exists(), "the shim should be bundled");
    assert!(dest.join("zlib_native.dylib").exists(), "so should what it wraps");
}

#[test]
fn bundling_is_a_no_op_when_the_artifact_is_already_beside_its_libs() {
    // `jade build --lib` at the project root. Copying a directory onto itself
    // would truncate every file in it.
    let tmp = TempDir::new("bundle_inplace");
    let libs = tmp.path().join("libs");
    let src = libs.join("dep-local");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("dep.dylib"), b"contents").unwrap();

    let written = bundle_beside_artifact(&tmp.path().join("main.dylib"), &libs).unwrap();
    assert!(written.is_empty(), "nothing to copy: {written:?}");
    assert_eq!(std::fs::read(src.join("dep.dylib")).unwrap(), b"contents");
}

#[test]
fn bundling_refuses_a_directory_already_holding_a_different_build() {
    // Two artifacts built into one directory share one libs/, which is the
    // point. Two *different* builds of one dependency landing there is not — the
    // second would silently replace the first for both.
    let tmp = TempDir::new("bundle_conflict");
    let libs = tmp.path().join("libs");
    let src = libs.join("dep-local");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("dep.dylib"), b"new build").unwrap();

    let ship = tmp.path().join("out");
    std::fs::create_dir_all(ship.join("libs").join("dep-local")).unwrap();
    std::fs::write(ship.join("libs").join("dep-local").join("dep.dylib"), b"old build").unwrap();

    let err = bundle_beside_artifact(&ship.join("app"), &libs).unwrap_err();
    assert!(err.contains("different build"), "should say why: {err}");
    assert!(err.contains("dep-local"), "should name it: {err}");
}

// ── A package that says what it needs ─────────────────────────────────────
//
// The repo's standing claim was that a `.so` carries no manifest, so nothing
// could discover what a package depends on. A package can answer for itself.

/// Build `src` as a Jade package inside a project carrying `lock`.
fn package_with_lock(tag: &str, src: &str, lock: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new(tag);
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    std::fs::write(tmp.path().join("jade.toml"), "[project]\nname = \"p\"\n").unwrap();
    std::fs::write(tmp.path().join("jade.lock"), lock).unwrap();
    let main = tmp.path().join("main.jde");
    std::fs::write(&main, src).unwrap();

    let out = tmp.path().join(format!("p.{ext}"));
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let tir = crate::compiler::type_infer::infer(program).expect("infer");
    crate::aot::compile_with_mode(
        tir,
        Some(&main),
        &out,
        false,
        crate::aot::CompileMode::SharedLib { exports: vec![] },
    )
    .expect("the package should build");
    (tmp, out)
}

const ONE_DEP_LOCK: &str = r#"version = 1

[[package]]
name = "fastmath"
version = "1.2.0"
source = "url+https://example.invalid/fastmath.dylib"
abi = "jade"

[package.artifacts.any]
url = "https://example.invalid/fastmath.dylib"
file = "fastmath.dylib"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#;

#[test]
fn a_package_reports_its_own_dependencies() {
    let (_tmp, artifact) = package_with_lock("deps", "fn wrapped() { return 1 }\n", ONE_DEP_LOCK);

    let declared = declared_dependencies(&artifact).expect("the package should carry its lock");
    assert_eq!(declared.len(), 1, "{declared:?}");
    assert_eq!(declared[0].name, "fastmath");
    assert_eq!(declared[0].version, "1.2.0");
    assert!(declared[0].source.starts_with("url+"), "{:?}", declared[0].source);
}

#[test]
fn a_package_with_no_dependencies_carries_no_record() {
    // Which is also what every package published before this looks like, so the
    // absent symbol has to be an ordinary answer rather than a failure.
    let (_tmp, artifact) =
        package_with_lock("nodeps", "fn wrapped() { return 1 }\n", "version = 1\n");
    assert!(declared_dependencies(&artifact).is_none());
}

#[test]
fn a_plain_c_library_is_never_opened_to_ask() {
    // The symbol-table check comes first, so a library that is not a Jade
    // package is never loaded at all — `jade pkg add` is in the middle of
    // deciding whether to trust it.
    let tmp = TempDir::new("notjade");
    let f = tmp.path().join("libnotjade.dylib");
    std::fs::write(&f, b"not an object file").unwrap();
    assert!(declared_dependencies(&f).is_none());
}

#[test]
fn opening_a_package_to_read_its_dependencies_runs_none_of_its_code() {
    // Load-bearing, and easy to break: a Jade package runs its module top level
    // from `jade_mod_init`, which `jade_pkg_init` calls and nothing else. If a
    // constructor were ever added, `jade pkg add` would start executing the
    // package it is being asked to add — before the user has agreed to run it.
    //
    // Proved by side effect rather than by reading the source, because reading
    // the source is exactly what would stop being true.
    let marker = std::env::temp_dir().join(format!("jade_ranit_{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let src = format!(
        "use std::fs\n\nfs.write(\"{}\", \"ran\")\n\nfn wrapped() {{ return 1 }}\n",
        marker.display()
    );

    let (_tmp, artifact) = package_with_lock("noexec", &src, ONE_DEP_LOCK);
    let declared = declared_dependencies(&artifact);

    assert!(declared.is_some(), "the record should still be readable");
    assert!(
        !marker.exists(),
        "opening a package to read its dependencies must not run its top level"
    );
    let _ = std::fs::remove_file(&marker);
}

// ── Choosing between two versions ─────────────────────────────────────────

#[test]
fn versions_order_by_number_not_by_spelling() {
    use std::cmp::Ordering;
    assert_eq!(compare_versions("1.10.0", "1.2.0"), Some(Ordering::Greater));
    assert_eq!(compare_versions("1.2.0", "1.10.0"), Some(Ordering::Less));
    assert_eq!(compare_versions("2.0.0", "2.0.0"), Some(Ordering::Equal));
}

#[test]
fn a_version_written_short_equals_the_same_one_written_long() {
    use std::cmp::Ordering;
    assert_eq!(compare_versions("1.2", "1.2.0"), Some(Ordering::Equal));
    assert_eq!(compare_versions("1.2.1", "1.2"), Some(Ordering::Greater));
}

#[test]
fn a_version_that_is_not_dotted_numbers_cannot_be_ordered() {
    // Refusing is the point: this decides which version a program loads, and
    // inventing an order for a spelling nobody defined picks a winner on a coin
    // toss. `local` is what a path dependency carries, so path dependencies
    // never take part in the choice.
    assert_eq!(compare_versions("2.0-beta", "2.0"), None);
    assert_eq!(compare_versions(LOCAL_VERSION, "1.0.0"), None);
    assert_eq!(compare_versions("1.0.0", LOCAL_VERSION), None);
}

/// A capstone-shaped C library: a call that writes a row of structs and returns
/// how many, and structs whose interesting fields are fixed-size char arrays.
///
/// Hermetic — no capstone on the machine — and it exercises every piece of the
/// v1.3.10 work at once. Before that work this library bound "cleanly" and a
/// caller could learn that three instructions existed and nothing about them.
const ROW_C_SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stddef.h>

typedef struct { unsigned int id; unsigned long long address; char mnemonic[32]; } insn;

/* Returns a count, not a status — the shape that used to be read as one, so a
 * successful call of three raised. */
size_t d_disasm(const unsigned char* code, size_t n, insn** out) {
    static const char* names[3] = { "push", "mov", "ret" };
    size_t made = n < 3 ? n : 3;
    insn* row = (insn*)calloc(made ? made : 1, sizeof(insn));
    for (size_t i = 0; i < made; i++) {
        row[i].id = (unsigned int)(i + 1);
        row[i].address = 0x1000 + i;
        snprintf(row[i].mnemonic, sizeof row[i].mnemonic, "%s", names[i]);
    }
    *out = row;
    return made;
}

void d_free(insn* p) { free(p); }
"#;

#[test]
fn a_row_of_structs_is_readable_end_to_end() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let tmp = TempDir::new("rowstructs");
    let built = compile_lib(tmp.path(), "rowlib", ROW_C_SOURCE);
    let rel = built.file_name().unwrap().to_str().unwrap().to_string();

    // Bound from the library's own header, so the shapes come from clang rather
    // than from a hand-written table that could agree with the bug.
    let header = tmp.path().join("rowlib.h");
    std::fs::write(
        &header,
        "#include <stddef.h>\n\
         typedef struct { unsigned int id; unsigned long long address; char mnemonic[32]; } insn;\n\
         size_t d_disasm(const unsigned char* code, size_t n, insn** out);\n\
         void d_free(insn* p);\n",
    )
    .unwrap();

    let b = crate::pkg::bindgen::from_header(&header, &[], &[], None, None).expect("bind");

    // The count must not have been read as a status, or every successful call
    // raises.
    let disasm_sym = b
        .symbols
        .get("d_disasm")
        .unwrap_or_else(|| panic!("d_disasm did not bind: {:?}", b.skipped));
    assert!(
        disasm_sym.fails_when.is_none(),
        "a count beside a handle is not a status: {:?}",
        disasm_sym.fails_when
    );
    // And the mnemonic must have survived as characters.
    let fields: Vec<&str> = b.structs["insn"].fields.iter().map(|(f, _)| f.as_str()).collect();
    assert!(fields.contains(&"mnemonic"), "the text field was dropped: {fields:?}");

    let mut toml = format!(
        "[project]\nname = \"app\"\n[dependencies.rowlib]\nversion = \"1.0.0\"\npath = \"{rel}\"\n\
         abi = \"c\"\nheaders = [\"rowlib.h\"]\ninclude_dirs = [\"{}\"]\n",
        tmp.path().display()
    );
    for (name, sym) in &b.symbols {
        toml.push_str(&format!("[dependencies.rowlib.symbols.{name}]\n"));
        let args: Vec<String> = sym.args.iter().map(|a| format!("\"{a}\"")).collect();
        toml.push_str(&format!("args = [{}]\nret = \"{}\"\n", args.join(", "), sym.ret));
    }
    for (name, def) in &b.structs {
        toml.push_str(&format!("[dependencies.rowlib.structs.{name}]\n"));
        let fs: Vec<String> =
            def.fields.iter().map(|(f, t)| format!("[\"{f}\", \"{t}\"]")).collect();
        toml.push_str(&format!("fields = [{}]\n", fs.join(", ")));
        if def.held {
            toml.push_str("held = true\n");
        }
    }

    let m = manifest(&toml);
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    lock::write(tmp.path(), &lock).unwrap();
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();
    build_c_shims(tmp.path(), &lock, &m).expect("the shim must compile");

    let libs = resolved_libraries(tmp.path(), &m);
    let span = crate::frontend::error::Span { line: 0, col: 0 };
    let resolved =
        crate::project::resolve_library_import(&libs, "rowlib", tmp.path()).unwrap().unwrap();
    let pkg = crate::native::load_native_package(&resolved.path, span).expect("load");

    // Call it, rather than only loading it. Nothing in the suite did that
    // before, which is why a binding that raised on success went unnoticed.
    let callable = |name: &str| match pkg.get(name) {
        Some(crate::vm::VmValue::NativeLibFn(f)) => f.clone(),
        other => panic!("{name} should be callable, got {other:?}"),
    };
    let disasm = callable("d_disasm");
    let at = callable("insn_at");

    let code = crate::builtins::make_trusted_bytes(vec![0x55, 0x48, 0xc3]);
    let result = disasm.call(&[code], span).expect("the call must not raise on success");

    // Two things back: how many, and the row.
    let (count, handle) = match &result {
        crate::vm::VmValue::Struct(s) => {
            let g = s.lock();
            (g.get_field("ret").cloned().unwrap(), g.get_field("out").cloned().unwrap())
        }
        other => panic!("expected a count beside the row, got {other:?}"),
    };
    assert!(matches!(count, crate::vm::VmValue::Int(3)), "wrong count: {count:?}");

    // And every one of them is readable, not just the first.
    let mut seen = Vec::new();
    for i in 0..3 {
        let one = at.call(&[handle.clone(), crate::vm::VmValue::Int(i)], span).expect("at");
        let crate::vm::VmValue::Struct(s) = one else { panic!("expected a struct") };
        let g = s.lock();
        let crate::vm::VmValue::Array(row) = g.get_field("mnemonic").cloned().unwrap() else {
            panic!("mnemonic should be a row of characters")
        };
        let text: String = row
            .lock()
            .iter()
            .filter_map(|v| match v {
                crate::vm::VmValue::Char(c) if c.ch() != '\0' => Some(c.ch()),
                _ => None,
            })
            .collect();
        seen.push(text);
    }
    assert_eq!(seen, ["push", "mov", "ret"], "every instruction should be readable");
}
