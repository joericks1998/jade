use std::collections::HashMap;
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
fn add_writes_a_url_dependency() {
    let tmp = project("addurl", "[project]\nname = \"app\"\n");

    add_dependency(
        tmp.path(),
        "tok",
        Source::Url("https://x/tok-{platform}.so"),
        Some("1.2.0"),
        Abi::Jade,
        None,
    )
    .unwrap();

    let m = parsed(tmp.path());
    let dep = &m.dependencies.unwrap()["tok"];
    assert_eq!(dep.version.as_deref(), Some("1.2.0"));
    assert_eq!(dep.url.as_deref(), Some("https://x/tok-{platform}.so"));
    assert!(dep.path.is_none());
    assert_eq!(dep.abi, Abi::Jade);
    assert!(dep.validate("tok").is_ok());
}

#[test]
fn add_writes_a_path_dependency() {
    let tmp = project("addpath", "[project]\nname = \"app\"\n");

    add_dependency(tmp.path(), "zlib", Source::Path("vendor/libz.so"), None, Abi::Jade, None)
        .unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["zlib"];
    assert_eq!(dep.path.as_deref(), Some("vendor/libz.so"));
    assert!(dep.version.is_none());
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

    add_dependency(tmp.path(), "tok", Source::Url("https://x/t.so"), Some("1.0.0"), Abi::Jade, None)
        .unwrap();

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
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\n\
         url = \"https://x/old.so\"\n",
    );

    add_dependency(tmp.path(), "tok", Source::Path("vendor/tok.so"), Some("2.0.0"), Abi::Jade, None)
        .unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["tok"];
    assert_eq!(dep.path.as_deref(), Some("vendor/tok.so"));
    assert!(dep.url.is_none(), "the stale url must not survive");
    assert_eq!(dep.version.as_deref(), Some("2.0.0"));
    assert!(dep.validate("tok").is_ok());
}

#[test]
fn add_writes_c_abi_with_symbols() {
    let tmp = project("addc", "[project]\nname = \"app\"\n");
    let mut symbols = HashMap::new();
    symbols.insert(
        "crc32".to_string(),
        CSymbol { args: vec!["int".into(), "str".into()], ret: "int".into(), fails_when: None },
    );

    add_dependency(tmp.path(), "zlib", Source::Path("libz.so"), Some("1.3.1"), Abi::C, Some(&symbols))
        .unwrap();

    let dep = &parsed(tmp.path()).dependencies.unwrap()["zlib"];
    assert_eq!(dep.abi, Abi::C);
    let syms = dep.symbols.as_ref().unwrap();
    assert_eq!(syms["crc32"].args, vec!["int".to_string(), "str".to_string()]);
    assert_eq!(syms["crc32"].ret, "int");
    assert!(dep.validate("zlib").is_ok());
}

#[test]
fn add_is_idempotent() {
    let tmp = project("addidem", "[project]\nname = \"app\"\n");
    let add = || {
        add_dependency(
            tmp.path(),
            "tok",
            Source::Url("https://x/t.so"),
            Some("1.0.0"),
            Abi::Jade,
            None,
        )
        .unwrap()
    };

    add();
    let first = read(tmp.path());
    add();
    assert_eq!(read(tmp.path()), first, "re-adding the same dependency must not churn the file");
}

// ── remove_dependency ─────────────────────────────────────────────────────────

#[test]
fn remove_deletes_the_entry() {
    let tmp = project(
        "rm",
        "[project]\nname = \"app\"\n[dependencies.tok]\nversion = \"1.0.0\"\n\
         url = \"https://x/t.so\"\n[dependencies.other]\npath = \"a.so\"\n",
    );

    assert!(remove_dependency(tmp.path(), "tok").unwrap());

    let deps = parsed(tmp.path()).dependencies.unwrap();
    assert!(!deps.contains_key("tok"));
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
        "[project]\nname = \"app\"\n[dependencies.only]\npath = \"a.so\"\n",
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
        "# keep me\n[project]\nname = \"app\"\n[dependencies.tok]\npath = \"a.so\"\n\
         [scripts]\nrun = \"jade run\"\n",
    );

    remove_dependency(tmp.path(), "tok").unwrap();

    let text = read(tmp.path());
    assert!(text.contains("# keep me"), "comment lost:\n{text}");
    assert!(text.contains("[scripts]"), "section lost:\n{text}");
}
