use std::collections::{BTreeMap, HashMap};
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

// ── resolve ───────────────────────────────────────────────────────────────────

#[test]
fn resolve_propagates_validation_errors() {
    let tmp = TempDir::new("resolveinvalid");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.bad]\nversion = \"^1.0\"\n\
         url = \"https://x/a.tar.gz\"\nabi = \"source\"\n",
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

/// A Rust package is cargo's to pin, so it is validated and left out of the
/// lock, and nothing is fetched for it.
#[test]
fn a_rust_dependency_is_validated_and_not_locked() {
    let tmp = TempDir::new("resolverust");
    let fetcher = MockFetcher::new();
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"rust/geo\"\nabi = \"rust\"\n",
    );
    assert!(resolve(tmp.path(), &m, &fetcher).unwrap().packages.is_empty());
    assert_eq!(fetcher.calls(), 0);

    let bad = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\nurl = \"https://x/geo.tar.gz\"\n\
         version = \"1.0\"\nabi = \"rust\"\n",
    );
    let err = resolve(tmp.path(), &bad, &fetcher).unwrap_err();
    assert!(err.contains("cargo fetches it"), "{err}");
}

#[test]
fn a_dependency_with_no_abi_names_the_two_kinds() {
    let tmp = TempDir::new("resolvenoabi");
    let m = manifest("[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\n");
    let err = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("abi = \"source\"") && err.contains("abi = \"rust\""), "{err}");
}

/// The two kinds 2.0.5 removed are refused where the manifest is read, with the
/// replacement named, rather than as an unknown variant.
#[test]
fn a_removed_abi_points_at_its_replacement() {
    let c: Result<ProjectManifest, _> = toml::from_str(
        "[project]\nname = \"app\"\n[dependencies.z]\npath = \"libz.so\"\nabi = \"c\"\n",
    );
    let err = c.unwrap_err().to_string();
    assert!(err.contains("Rust crate that wraps it"), "{err}");
    let jade: Result<ProjectManifest, _> = toml::from_str(
        "[project]\nname = \"app\"\n[dependencies.z]\npath = \"z.so\"\nabi = \"jade\"\n",
    );
    assert!(jade.unwrap_err().to_string().contains("abi = \"source\""));
}

#[test]
fn a_lock_recording_a_prebuilt_library_is_refused_with_the_fix() {
    let tmp = TempDir::new("prebuiltlock");
    let mut lock = Lockfile::new();
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ANY_PLATFORM.to_string(),
        LockedArtifact { url: None, file: "z.so".into(), sha256: "00".into() },
    );
    lock.packages.push(LockedPackage {
        name: "z".into(),
        version: "1.0".into(),
        source: lock::source_path("z.so"),
        abi: "c".into(),
        artifacts,
    });
    let err = materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap_err();
    assert!(err.contains("jade pkg install"), "{err}");
}

// ── verify_in_sync ────────────────────────────────────────────────────────────

/// A project holding one path source package, resolved.
fn one_source_package(tag: &str) -> (TempDir, ProjectManifest, Lockfile) {
    let tmp = TempDir::new(tag);
    write_package(&tmp.path().join("geo"), None);
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    (tmp, m, lock)
}

#[test]
fn verify_in_sync_accepts_a_matching_pair() {
    let (_tmp, m, lock) = one_source_package("syncok");
    assert!(verify_in_sync(&m, &lock).is_ok());
}

#[test]
fn verify_in_sync_reports_a_manifest_only_dependency() {
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.new]\npath = \"new\"\nabi = \"source\"\n",
    );
    let err = verify_in_sync(&m, &Lockfile::new()).unwrap_err();

    assert!(err.contains("not locked"), "unexpected message: {err}");
    assert!(err.contains("new"), "error should name the dependency: {err}");
    assert!(err.contains("jade pkg install"), "error should say how to recover: {err}");
}

#[test]
fn verify_in_sync_reports_a_lock_only_dependency() {
    let (_tmp, _, lock) = one_source_package("synclockonly");
    let m = manifest("[project]\nname = \"app\"\n");
    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("not in jade.toml"), "unexpected message: {err}");
    assert!(err.contains("geo"), "error should name the dependency: {err}");
}

#[test]
fn verify_in_sync_reports_both_directions_at_once() {
    // Editing a manifest by hand should not become a sequence of one-error runs.
    let (_tmp, _, lock) = one_source_package("syncboth");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.added]\npath = \"a\"\nabi = \"source\"\n",
    );
    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("added"), "should report the manifest-only dep: {err}");
    assert!(err.contains("geo"), "should report the lock-only dep: {err}");
}

/// A Rust package is in neither the lock nor the comparison: adding one to the
/// manifest does not make a correct lock stale.
#[test]
fn verify_in_sync_leaves_rust_dependencies_out() {
    let (_tmp, _, lock) = one_source_package("syncrust");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n\
         [dependencies.fast]\npath = \"rust/fast\"\nabi = \"rust\"\n",
    );
    assert!(verify_in_sync(&m, &lock).is_ok());
}

#[test]
fn verify_in_sync_reports_a_kind_the_lock_disagrees_with() {
    let (_tmp, _, lock) = one_source_package("synckind");
    let m =
        manifest("[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"rust\"\n");
    let err = verify_in_sync(&m, &lock).unwrap_err();
    assert!(err.contains("different kind"), "{err}");
    assert!(err.contains("jade.toml says rust"), "{err}");
}

// ── resolved_libraries ────────────────────────────────────────────────────────

#[test]
fn resolved_libraries_unions_manifest_libs_source_packages_and_rust_packages() {
    let (tmp, _, lock) = one_source_package("union");
    lock::write(tmp.path(), &lock).unwrap();
    let m = manifest(
        "[project]\nname = \"app\"\n[lib.utils]\npath = \"src/utils\"\n\
         [dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n\
         [dependencies.fast]\npath = \"rust/fast\"\nabi = \"rust\"\n",
    );

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 3);
    assert_eq!(libs["utils"].path, "src/utils");
    assert_eq!(libs["geo"].path, "geo");
    assert_eq!(libs["fast"].rust.as_deref(), Some("fast"));

    // A Rust package resolves as one module, named by its dependency.
    let r = crate::project::resolve_library_import(&libs, "fast", tmp.path()).unwrap().unwrap();
    assert_eq!(r.kind, crate::project::ImportKind::Rust);
    assert_eq!(r.path, PathBuf::from("fast"));
}

#[test]
fn resolved_libraries_lets_a_manifest_lib_shadow_a_dependency() {
    let (tmp, _, lock) = one_source_package("shadow");
    lock::write(tmp.path(), &lock).unwrap();

    // A [lib.geo] declared locally must win over the locked dependency `geo`.
    let m = manifest("[project]\nname = \"app\"\n[lib.geo]\npath = \"src/mygeo\"\n");

    let libs = resolved_libraries(tmp.path(), &m);
    assert_eq!(libs.len(), 1);
    assert_eq!(libs["geo"].path, "src/mygeo", "the local library must win");
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

/// `ensure_ready` installs source packages; a project whose dependencies are all
/// Rust has nothing to lock, and must not be sent to `jade pkg install`.
#[test]
fn a_project_with_only_rust_dependencies_is_ready_without_a_lock() {
    let tmp = TempDir::new("readyrust");
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.fast]\npath = \"rust/fast\"\nabi = \"rust\"\n",
    );
    assert!(ensure_ready(tmp.path(), &m).is_ok());
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

// ── source packages ───────────────────────────────────────────────────────────

/// Write a small source package into `dir`.
fn write_package(dir: &Path, exports: Option<&str>) {
    std::fs::create_dir_all(dir).unwrap();
    let mut toml = String::from("[package]\nname = \"geo\"\n");
    if let Some(e) = exports {
        toml.push_str(&format!("exports = [{e}]\n"));
    }
    std::fs::write(dir.join("jade.toml"), toml).unwrap();
    std::fs::write(dir.join("geo.jde"), "fn area(w, h) {\n    return w * h\n}\n").unwrap();
}

#[test]
fn a_path_source_package_is_locked_by_its_tree_and_used_in_place() {
    let tmp = TempDir::new("srcpath");
    write_package(&tmp.path().join("geo"), Some("\"area\""));
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n",
    );
    let lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    let pkg = lock.get("geo").unwrap();
    assert_eq!(pkg.abi, "source");
    let art = &pkg.artifacts[ANY_PLATFORM];
    assert_eq!(art.file, "geo.jde");
    assert_eq!(art.sha256, source::tree_hash(&tmp.path().join("geo")).unwrap());

    // Nothing is copied: the package is read where it is.
    materialize(tmp.path(), &lock, &MockFetcher::new()).unwrap();
    assert!(!tmp.path().join(LIBS_DIR).join("geo-local").exists());

    let libs = dependency_libraries(tmp.path(), &lock);
    let resolved =
        crate::project::resolve_library_import(&libs, "geo", tmp.path()).unwrap().unwrap();
    assert_eq!(resolved.path, tmp.path().join("geo/geo.jde"));
    assert_eq!(resolved.exports, Some(vec!["area".to_string()]));
}

#[test]
fn a_source_tree_hash_covers_source_and_nothing_else() {
    let tmp = TempDir::new("srchash");
    let dir = tmp.path().join("geo");
    write_package(&dir, None);
    let before = source::tree_hash(&dir).unwrap();

    // A README and a git directory do not change what a program compiles to.
    std::fs::write(dir.join("README.md"), "hello").unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/HEAD"), "ref").unwrap();
    assert_eq!(source::tree_hash(&dir).unwrap(), before);

    // Source does.
    std::fs::write(dir.join("geo.jde"), "fn area(w, h) {\n    return w + h\n}\n").unwrap();
    assert_ne!(source::tree_hash(&dir).unwrap(), before);
}

#[test]
fn a_changed_path_package_is_repinned() {
    let tmp = TempDir::new("srcdrift");
    write_package(&tmp.path().join("geo"), None);
    let m = manifest(
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n",
    );
    let mut lock = resolve(tmp.path(), &m, &MockFetcher::new()).unwrap();
    std::fs::write(tmp.path().join("geo/extra.jde"), "fn x() {\n    return 1\n}\n").unwrap();
    assert!(local_drift(tmp.path(), lock.get("geo").unwrap()));
    assert!(verify_local_unchanged(tmp.path(), &lock).is_err());
    assert_eq!(refresh_local(tmp.path(), &mut lock), vec!["geo".to_string()]);
    assert!(verify_local_unchanged(tmp.path(), &lock).is_ok());
}

#[test]
fn an_archive_source_package_is_unpacked_into_libs_and_checked() {
    let tmp = TempDir::new("srcarchive");
    // GitHub wraps an archive's files in one directory, which is stepped into.
    let staging = TempDir::new("srcarchive_stage");
    write_package(&staging.path().join("geo-1.0"), None);
    let archive = staging.path().join("geo.tar.gz");
    let ok = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(staging.path())
        .arg("geo-1.0")
        .status()
        .unwrap()
        .success();
    assert!(ok, "tar must pack the fixture");
    let bytes = std::fs::read(&archive).unwrap();

    let url = "https://example.com/geo-1.0.tar.gz";
    let fetcher = MockFetcher::new().with(url, &bytes);
    let m = manifest(&format!(
        "[project]\nname = \"app\"\n[dependencies.geo]\nurl = \"{url}\"\nversion = \"1.0\"\n\
         abi = \"source\"\n"
    ));
    let lock = resolve(tmp.path(), &m, &fetcher).unwrap();
    materialize(tmp.path(), &lock, &fetcher).unwrap();
    let installed = tmp.path().join(LIBS_DIR).join("geo-1.0");
    assert!(installed.join("geo.jde").is_file());

    // A tampered install is noticed and replaced.
    std::fs::write(installed.join("geo.jde"), "fn area(w, h) {\n    return 0\n}\n").unwrap();
    materialize(tmp.path(), &lock, &fetcher).unwrap();
    let fixed = std::fs::read_to_string(installed.join("geo.jde")).unwrap();
    assert!(fixed.contains("w * h"));

    // An archive whose contents differ from the lock is refused.
    let other = MockFetcher::new().with(url, b"not an archive");
    std::fs::remove_dir_all(&installed).unwrap();
    assert!(materialize(tmp.path(), &lock, &other).is_err());
}

#[test]
fn a_package_names_its_own_dependencies_relative_to_the_project_adding_it() {
    let tmp = TempDir::new("srcdeps");
    write_package(&tmp.path().join("pkgs/units"), None);
    let physics = tmp.path().join("pkgs/physics");
    std::fs::create_dir_all(&physics).unwrap();
    std::fs::write(
        physics.join("jade.toml"),
        "[package]\nname = \"physics\"\n[dependencies.units]\npath = \"../units\"\n\
         abi = \"source\"\n[dependencies.remote]\ngit = \"https://x/r\"\ntag = \"v1\"\n\
         abi = \"source\"\n",
    )
    .unwrap();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(&app).unwrap();

    let (deps, unfollowed) = source::declared_dependencies(&physics, &app, true);
    let names: Vec<&str> = deps.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["remote", "units"]);
    assert_eq!(deps[1].1.path.as_deref(), Some("../pkgs/units"));
    assert!(unfollowed.is_empty());

    // From a package that came from elsewhere, a path means nothing here.
    let (deps, unfollowed) = source::declared_dependencies(&physics, &app, false);
    assert_eq!(deps.len(), 1);
    assert_eq!(unfollowed, vec!["units".to_string()]);
}
