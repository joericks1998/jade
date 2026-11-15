use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::project::ProjectManifest;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("jade_manifest_test_{tag}_{pid}_{n}"));
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

fn project(tag: &str, body: &str) -> TempDir {
    let tmp = TempDir::new(tag);
    std::fs::write(tmp.path().join("jade.toml"), body).unwrap();
    tmp
}

fn read(root: &Path) -> String {
    std::fs::read_to_string(root.join("jade.toml")).unwrap()
}

fn parsed(root: &Path) -> ProjectManifest {
    toml::from_str(&read(root)).expect("edited manifest must still parse")
}

// ── add_dependency ────────────────────────────────────────────────────────────

#[test]
fn add_writes_a_source_archive_dependency() {
    let tmp = project("addurl", "[project]\nname = \"app\"\n");

    add_dependency(
        tmp.path(),
        "geo",
        Source::Url("https://x/geo-1.2.0.tar.gz"),
        Some("1.2.0"),
        Abi::Source,
        None,
    )
    .unwrap();

    let m = parsed(tmp.path());
    let dep = &m.dependencies.unwrap()["geo"];
    assert_eq!(dep.version.as_deref(), Some("1.2.0"));
    assert_eq!(dep.url.as_deref(), Some("https://x/geo-1.2.0.tar.gz"));
    assert!(dep.path.is_none());
    assert_eq!(dep.abi, Some(Abi::Source));
    assert!(dep.validate("geo").is_ok());
}

#[test]
fn add_writes_a_rust_path_dependency_with_its_package_name() {
    let tmp = project("addrust", "[project]\nname = \"app\"\n");

    add_dependency(
        tmp.path(),
        "zlib",
        Source::Path("rust/zlib"),
        None,
        Abi::Rust,
        Some("zlib-jade"),
    )
    .unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["zlib"];
    assert_eq!(dep.path.as_deref(), Some("rust/zlib"));
    assert_eq!(dep.package.as_deref(), Some("zlib-jade"));
    assert_eq!(dep.abi, Some(Abi::Rust));
    assert!(dep.validate("zlib").is_ok());
}

#[test]
fn add_writes_a_crates_io_rust_dependency() {
    let tmp = project("addreg", "[project]\nname = \"app\"\n");

    add_dependency(tmp.path(), "hex", Source::Registry, Some("0.4.3"), Abi::Rust, None).unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["hex"];
    assert_eq!(dep.version.as_deref(), Some("0.4.3"));
    assert!(dep.path.is_none() && dep.git.is_none() && dep.url.is_none());
    assert!(dep.validate("hex").is_ok());
}

#[test]
fn add_preserves_comments_and_unrelated_sections() {
    // The reason this module uses toml_edit at all: a parse-and-reserialize
    // round-trip would silently delete everything asserted here.
    let tmp = project(
        "preserve",
        "# my project\n[project]\nname = \"app\"  # inline note\n\n\
         [scripts]\nbuild = \"jade build main.jde\"\n\n\
         [lib.utils]\npath = \"src/utils\"\n",
    );

    add_dependency(tmp.path(), "geo", Source::Path("../geo"), None, Abi::Source, None).unwrap();

    let text = read(tmp.path());
    assert!(text.contains("# my project"), "leading comment lost:\n{text}");
    assert!(text.contains("# inline note"), "inline comment lost:\n{text}");
    assert!(text.contains("[scripts]"), "unrelated section lost:\n{text}");
    assert!(text.contains("[lib.utils]"), "lib section lost:\n{text}");

    let m = parsed(tmp.path());
    assert!(m.scripts.is_some());
    assert!(m.lib.is_some());
    assert!(m.dependencies.is_some());
}

#[test]
fn add_replaces_an_existing_dependency_wholesale() {
    // Merging could leave an old `url` beside a new `path`, which validate()
    // then rejects for a reason the user never asked for.
    let tmp = project(
        "replace",
        "[project]\nname = \"app\"\n[dependencies.geo]\nversion = \"1.0.0\"\n\
         url = \"https://x/old.tar.gz\"\nabi = \"source\"\n",
    );

    add_dependency(tmp.path(), "geo", Source::Path("vendor/geo"), Some("2.0.0"), Abi::Source, None)
        .unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["geo"];
    assert_eq!(dep.path.as_deref(), Some("vendor/geo"));
    assert!(dep.url.is_none(), "the stale url must not survive");
    assert_eq!(dep.version.as_deref(), Some("2.0.0"));
    assert!(dep.validate("geo").is_ok());
}

#[test]
fn add_is_idempotent() {
    let tmp = project("addidem", "[project]\nname = \"app\"\n");
    let add = || {
        add_dependency(tmp.path(), "fast", Source::Path("rust/fast"), None, Abi::Rust, None)
            .unwrap()
    };

    add();
    let first = read(tmp.path());
    add();
    assert_eq!(read(tmp.path()), first, "re-adding the same dependency must not churn the file");
}

/// A source package's own dependency, Rust included, is written whole into the
/// project adding it.
#[test]
fn a_declared_rust_dependency_is_carried_over_with_its_package_name() {
    let tmp = project("addentry", "[project]\nname = \"app\"\n");
    let entry = crate::project::DependencyEntry {
        path: Some("../pkgs/fast".into()),
        package: Some("fast-math".into()),
        abi: Some(Abi::Rust),
        ..Default::default()
    };
    add_entry(tmp.path(), "fast", &entry).unwrap();
    let dep = &parsed(tmp.path()).dependencies.unwrap()["fast"];
    assert_eq!(dep.package.as_deref(), Some("fast-math"));
    assert_eq!(dep.abi, Some(Abi::Rust));
}

// ── remove_dependency ─────────────────────────────────────────────────────────

#[test]
fn remove_deletes_the_entry() {
    let tmp = project(
        "rm",
        "[project]\nname = \"app\"\n[dependencies.geo]\npath = \"geo\"\nabi = \"source\"\n\
         [dependencies.other]\npath = \"rust/other\"\nabi = \"rust\"\n",
    );

    assert!(remove_dependency(tmp.path(), "geo").unwrap());

    let deps = parsed(tmp.path()).dependencies.unwrap();
    assert!(!deps.contains_key("geo"));
    assert!(deps.contains_key("other"), "the other dependency must survive");
}

#[test]
fn remove_reports_an_absent_dependency() {
    let tmp = project("rmabsent", "[project]\nname = \"app\"\n");
    assert!(!remove_dependency(tmp.path(), "ghost").unwrap());
}

#[test]
fn remove_drops_an_emptied_dependencies_table() {
    let tmp = project(
        "rmempty",
        "[project]\nname = \"app\"\n[dependencies.only]\npath = \"a\"\nabi = \"source\"\n",
    );

    remove_dependency(tmp.path(), "only").unwrap();

    let text = read(tmp.path());
    assert!(!text.contains("[dependencies]"), "empty table left behind:\n{text}");
    assert!(parsed(tmp.path()).dependencies.is_none());
}

#[test]
fn remove_preserves_unrelated_content() {
    let tmp = project(
        "rmpreserve",
        "# keep me\n[project]\nname = \"app\"\n[dependencies.geo]\npath = \"a\"\n\
         abi = \"source\"\n[scripts]\nrun = \"jade run\"\n",
    );

    remove_dependency(tmp.path(), "geo").unwrap();

    let text = read(tmp.path());
    assert!(text.contains("# keep me"), "comment lost:\n{text}");
    assert!(text.contains("[scripts]"), "section lost:\n{text}");
}
