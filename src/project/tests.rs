//! Tests for project manifest parsing, `[lib]` import resolution, root
//! discovery (via a unique temp dir), and test-file discovery.

use super::*;

// ── Unique temp-dir scaffolding ───────────────────────────────────────────────

/// Create a unique empty directory under the system temp dir. Cleaned up by
/// `TempDir::drop`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("jade_proj_test_{tag}_{pid}_{n}"));
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

// ── Manifest parsing ──────────────────────────────────────────────────────────

fn parse(src: &str) -> ProjectManifest {
    toml::from_str::<ProjectManifest>(src).expect("valid toml")
}

#[test]
fn parse_full_manifest() {
    let m = parse(
        r#"
        [project]
        name = "demo"
        version = "0.2.0"
        entry = "app.jde"

        [scripts]
        build = "jade build main.jde"
        test = "jade test"
        "#,
    );
    assert!(m.is_project());
    let p = m.project.as_ref().unwrap();
    assert_eq!(p.name, "demo");
    assert_eq!(p.version.as_deref(), Some("0.2.0"));
    assert_eq!(m.entry_file(), "app.jde");
    let scripts = m.scripts.as_ref().unwrap();
    assert_eq!(scripts.get("build").map(String::as_str), Some("jade build main.jde"));
    assert_eq!(scripts.get("test").map(String::as_str), Some("jade test"));
}

#[test]
fn parse_partial_manifest_defaults_entry() {
    let m = parse(
        r#"
        [project]
        name = "minimal"
        "#,
    );
    assert!(m.is_project());
    assert_eq!(m.project.as_ref().unwrap().name, "minimal");
    assert!(m.project.as_ref().unwrap().version.is_none());
    // No explicit entry → default.
    assert_eq!(m.entry_file(), "main.jde");
    assert!(m.scripts.is_none());
}

#[test]
fn empty_manifest_is_not_a_project() {
    let m = parse("");
    assert!(!m.is_project());
    // entry_file still returns the default even with no project section.
    assert_eq!(m.entry_file(), "main.jde");
}

#[test]
fn manifest_without_project_section_is_not_project() {
    let m = parse(
        r#"
        [scripts]
        run = "echo hi"
        "#,
    );
    assert!(!m.is_project());
    assert!(m.scripts.is_some());
}

#[test]
fn malformed_manifest_fails_to_parse() {
    // Missing required `name` in [project].
    let err = toml::from_str::<ProjectManifest>(
        r#"
        [project]
        version = "1.0"
        "#,
    );
    assert!(err.is_err(), "manifest without project.name must fail");

    // Syntactically broken toml.
    let broken = toml::from_str::<ProjectManifest>("[project\nname = ");
    assert!(broken.is_err());
}

#[test]
fn parse_lib_section() {
    let m = parse(
        r#"
        [project]
        name = "libdemo"

        [lib.utils]
        path = "src/utils"
        files = ["math.jde", "fast.dylib"]
        "#,
    );
    let libs = m.lib.as_ref().unwrap();
    let utils = libs.get("utils").unwrap();
    assert_eq!(utils.path, "src/utils");
    assert_eq!(
        utils.files.as_ref().unwrap(),
        &vec!["math.jde".to_string(), "fast.dylib".to_string()]
    );
}

// ── split_lib_ext / kind_for_ext (private helpers) ────────────────────────────

#[test]
fn split_lib_ext_recognizes_extensions() {
    assert_eq!(split_lib_ext("math.jde"), ("math", "jde"));
    assert_eq!(split_lib_ext("fast.dylib"), ("fast", "dylib"));
    assert_eq!(split_lib_ext("a.so"), ("a", "so"));
    assert_eq!(split_lib_ext("b.dll"), ("b", "dll"));
    // Subdir preserved in the stem.
    assert_eq!(split_lib_ext("nested/mod.jde"), ("nested/mod", "jde"));
    // Unknown extension → (name, "").
    assert_eq!(split_lib_ext("plain.txt"), ("plain.txt", ""));
    assert_eq!(split_lib_ext("noext"), ("noext", ""));
}

#[test]
fn kind_for_ext_maps_extensions() {
    assert_eq!(kind_for_ext("jde"), Some(ImportKind::Jade));
    assert_eq!(kind_for_ext("dylib"), Some(ImportKind::Native));
    assert_eq!(kind_for_ext("so"), Some(ImportKind::Native));
    assert_eq!(kind_for_ext("dll"), Some(ImportKind::Native));
    assert_eq!(kind_for_ext("txt"), None);
}

// ── resolve_library_import ────────────────────────────────────────────────────

fn libs_with(name: &str, entry: LibraryEntry) -> HashMap<String, LibraryEntry> {
    let mut m = HashMap::new();
    m.insert(name.to_string(), entry);
    m
}

#[test]
fn resolve_import_not_a_library_reference() {
    let libs = libs_with("utils", LibraryEntry { path: "src".into(), files: None });
    // Bare name that matches no registered library → not a lib reference, so a
    // plain relative import still falls through to file resolution.
    assert!(matches!(
        resolve_library_import(&libs, "plainimport", Path::new("/root")),
        Ok(None)
    ));
    // Unknown lib name → not a lib reference.
    assert!(matches!(
        resolve_library_import(&libs, "other/mod", Path::new("/root")),
        Ok(None)
    ));
}

#[test]
fn resolve_import_with_allowlist_hit() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["math.jde".into()]) },
    );
    let resolved = resolve_library_import(&libs, "utils/math", Path::new("/root"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, ImportKind::Jade);
    assert_eq!(resolved.path, PathBuf::from("/root/src/utils/math.jde"));
}

#[test]
fn resolve_import_with_allowlist_native() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["fast.dylib".into()]) },
    );
    let resolved = resolve_library_import(&libs, "utils/fast", Path::new("/root"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, ImportKind::Native);
    assert_eq!(resolved.path, PathBuf::from("/root/src/utils/fast.dylib"));
}

#[test]
fn resolve_import_allowlist_miss_is_err() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["math.jde".into()]) },
    );
    let err = resolve_library_import(&libs, "utils/missing", Path::new("/root"));
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("not registered"));
}

#[test]
fn resolve_import_absolute_lib_path() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "/abs/utils".into(), files: Some(vec!["m.jde".into()]) },
    );
    let resolved = resolve_library_import(&libs, "utils/m", Path::new("/root"))
        .unwrap()
        .unwrap();
    // Absolute lib path ignores `root`.
    assert_eq!(resolved.path, PathBuf::from("/abs/utils/m.jde"));
}

#[test]
fn resolve_import_no_allowlist_probes_disk() {
    let tmp = TempDir::new("libprobe");
    let libdir = tmp.path().join("mylib");
    std::fs::create_dir_all(&libdir).unwrap();
    std::fs::write(libdir.join("thing.jde"), "").unwrap();

    let libs = libs_with(
        "mylib",
        LibraryEntry { path: "mylib".into(), files: None },
    );
    let resolved = resolve_library_import(&libs, "mylib/thing", tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, ImportKind::Jade);
    assert_eq!(resolved.path, libdir.join("thing.jde"));
}

#[test]
fn resolve_import_no_allowlist_missing_returns_jde_candidate() {
    let tmp = TempDir::new("libmiss");
    let libs = libs_with(
        "mylib",
        LibraryEntry { path: "mylib".into(), files: None },
    );
    // Nothing on disk → returns the `.jde` candidate as Jade so caller emits a
    // normal not-found error.
    let resolved = resolve_library_import(&libs, "mylib/ghost", tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, ImportKind::Jade);
    assert!(resolved.path.ends_with("ghost.jde"));
}

// ── Root discovery ────────────────────────────────────────────────────────────

#[test]
fn find_project_root_from_walks_up() {
    let tmp = TempDir::new("rootwalk");
    let root = tmp.path();
    let nested = root.join("a").join("b").join("c");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        root.join("jade.toml"),
        "[project]\nname = \"rooted\"\n",
    )
    .unwrap();

    let found = find_project_root_from(&nested).expect("should find root by walking up");
    // Canonicalize both to compare (temp dirs may be symlinked on macOS).
    assert_eq!(
        std::fs::canonicalize(&found).unwrap(),
        std::fs::canonicalize(root).unwrap()
    );
}

#[test]
fn find_project_root_from_ignores_non_project_toml() {
    let tmp = TempDir::new("nonproject");
    let root = tmp.path();
    // A jade.toml with no [project] section should NOT count as a root.
    std::fs::write(root.join("jade.toml"), "[scripts]\nx = \"y\"\n").unwrap();
    let found = find_project_root_from(root);
    assert!(found.is_none(), "toml without [project] must not anchor a root");
}

#[test]
fn find_project_root_from_none_when_absent() {
    let tmp = TempDir::new("noroot");
    // No jade.toml anywhere in this isolated temp subtree — but the temp dir's
    // ancestors could theoretically contain one, so assert only that OUR file is
    // required: use a deep dir with no manifest and confirm discovery does not
    // find one inside our tree.
    let nested = tmp.path().join("x").join("y");
    std::fs::create_dir_all(&nested).unwrap();
    let found = find_project_root_from(&nested);
    // If discovery returns Some, it must be an ancestor OUTSIDE our temp dir
    // (never our own dir, which has no jade.toml).
    if let Some(f) = found {
        let canon = std::fs::canonicalize(&f).unwrap();
        let ours = std::fs::canonicalize(tmp.path()).unwrap();
        assert!(!canon.starts_with(&ours), "must not anchor inside our manifest-free tree");
    }
}

// ── load_project ──────────────────────────────────────────────────────────────

#[test]
fn load_project_reads_manifest() {
    let tmp = TempDir::new("load");
    std::fs::write(
        tmp.path().join("jade.toml"),
        "[project]\nname = \"loaded\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
    let m = load_project(tmp.path()).expect("load ok");
    assert_eq!(m.project.unwrap().name, "loaded");
}

#[test]
fn load_project_missing_file_errors() {
    let tmp = TempDir::new("loadmiss");
    let err = load_project(tmp.path());
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("cannot read"));
}

#[test]
fn load_project_invalid_toml_errors() {
    let tmp = TempDir::new("loadbad");
    std::fs::write(tmp.path().join("jade.toml"), "[project\nname =").unwrap();
    let err = load_project(tmp.path());
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("invalid jade.toml"));
}

// ── Test-file discovery ───────────────────────────────────────────────────────

#[test]
fn find_test_files_matches_conventions() {
    let tmp = TempDir::new("testfiles");
    let root = tmp.path();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    std::fs::write(root.join("test_alpha.jde"), "").unwrap();
    std::fs::write(sub.join("beta_test.jde"), "").unwrap();
    std::fs::write(root.join("regular.jde"), "").unwrap(); // not a test
    std::fs::write(root.join("notes.txt"), "").unwrap();   // wrong ext

    let found = find_test_files(root, None);
    let names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"test_alpha.jde".to_string()));
    assert!(names.contains(&"beta_test.jde".to_string()));
    assert!(!names.contains(&"regular.jde".to_string()));
    assert!(!names.contains(&"notes.txt".to_string()));
    assert_eq!(found.len(), 2);
    // Results are sorted.
    let mut sorted = found.clone();
    sorted.sort();
    assert_eq!(found, sorted);
}

#[test]
fn find_test_files_pattern_filters_by_stem() {
    let tmp = TempDir::new("testpattern");
    let root = tmp.path();
    std::fs::write(root.join("test_math.jde"), "").unwrap();
    std::fs::write(root.join("test_string.jde"), "").unwrap();

    let found = find_test_files(root, Some("math"));
    assert_eq!(found.len(), 1);
    assert!(found[0].file_name().unwrap().to_string_lossy().contains("math"));
}

#[test]
fn find_test_files_skips_target_and_hidden_dirs() {
    let tmp = TempDir::new("skipdirs");
    let root = tmp.path();
    for d in ["target", ".hidden", "docs"] {
        let dir = root.join(d);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test_skip.jde"), "").unwrap();
    }
    std::fs::write(root.join("test_keep.jde"), "").unwrap();

    let found = find_test_files(root, None);
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("test_keep.jde"));
}

// ── Bare-name library imports (`use fastmath`) ────────────────────────────────

#[test]
fn bare_import_resolves_registered_library() {
    // A dependency is a single artifact with no second path segment, so a bare
    // name that matches a registered library resolves to the module of the
    // same name.
    let libs = libs_with(
        "fastmath",
        LibraryEntry { path: "libs/fastmath-1.0.0".into(), files: Some(vec!["fastmath.so".into()]) },
    );
    let resolved = resolve_library_import(&libs, "fastmath", Path::new("/root"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.kind, ImportKind::Native);
    assert_eq!(resolved.path, PathBuf::from("/root/libs/fastmath-1.0.0/fastmath.so"));
}

#[test]
fn bare_import_still_honours_the_allowlist() {
    // Registered, but the bare name is not among its files → hard error, the
    // same as for a slashed import.
    let libs = libs_with(
        "fastmath",
        LibraryEntry { path: "libs".into(), files: Some(vec!["other.so".into()]) },
    );
    let err = resolve_library_import(&libs, "fastmath", Path::new("/root")).unwrap_err();
    assert!(err.contains("fastmath"), "error should name the module: {err}");
}

#[test]
fn slashed_imports_are_unaffected_by_bare_name_support() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["math.jde".into()]) },
    );
    let resolved = resolve_library_import(&libs, "utils/math", Path::new("/root"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.path, PathBuf::from("/root/src/utils/math.jde"));
}

// ── [dependencies] parsing ────────────────────────────────────────────────────

#[test]
fn parse_dependencies_section() {
    let m = parse(
        r#"
        [project]
        name = "app"

        [dependencies.fastmath]
        version = "1.2.0"
        url = "https://example.com/fastmath-{platform}.so"

        [dependencies.zlib]
        version = "1.3.1"
        path = "vendor/libz.so"
        abi = "c"

        [dependencies.zlib.symbols.crc32]
        args = ["int", "str"]
        ret = "int"
        "#,
    );
    let deps = m.dependencies.expect("dependencies parsed");
    assert_eq!(deps.len(), 2);

    let fastmath = &deps["fastmath"];
    assert_eq!(fastmath.abi, Abi::Jade, "abi defaults to jade");
    assert!(fastmath.is_platform_template());
    assert!(fastmath.validate("fastmath").is_ok());

    let zlib = &deps["zlib"];
    assert_eq!(zlib.abi, Abi::C);
    assert!(!zlib.is_platform_template());
    assert_eq!(zlib.symbols.as_ref().unwrap()["crc32"].ret, "int");
    assert!(zlib.validate("zlib").is_ok());
}

#[test]
fn manifest_without_dependencies_still_parses() {
    // Backward compatibility: every existing jade.toml predates this section.
    let m = parse("[project]\nname = \"app\"\n");
    assert!(m.dependencies.is_none());
}

// ── DependencyEntry::validate ─────────────────────────────────────────────────

fn dep(toml_src: &str) -> DependencyEntry {
    toml::from_str::<DependencyEntry>(toml_src).expect("valid dependency toml")
}

#[test]
fn validate_rejects_both_path_and_url() {
    let d = dep("version = \"1.0.0\"\npath = \"a.so\"\nurl = \"https://x/a.so\"\n");
    let err = d.validate("dup").unwrap_err();
    assert!(err.contains("dup"), "error should name the dependency: {err}");
    assert!(err.contains("exactly one source"), "unexpected message: {err}");
}

#[test]
fn validate_rejects_no_source() {
    let err = dep("version = \"1.0.0\"\n").validate("orphan").unwrap_err();
    assert!(err.contains("orphan"));
    assert!(err.contains("'path' or 'url'"), "unexpected message: {err}");
}

#[test]
fn validate_rejects_url_without_version() {
    let err = dep("url = \"https://x/a.so\"\n").validate("unpinned").unwrap_err();
    assert!(err.contains("unpinned"));
    assert!(err.contains("version"), "unexpected message: {err}");
}

#[test]
fn validate_allows_path_without_version() {
    // A local artifact is whatever the user points at; there is nothing to pin.
    assert!(dep("path = \"vendor/a.so\"\n").validate("local").is_ok());
}

#[test]
fn validate_rejects_version_ranges() {
    // Ranges need a registry to resolve against, and Jade has none.
    for range in ["^1.2", "~1.2.0", "1.*", ">=1.0", "1.0, <2.0"] {
        let d = dep(&format!("version = \"{range}\"\nurl = \"https://x/a.so\"\n"));
        let err = d
            .validate("ranged")
            .expect_err(&format!("range {range:?} should be rejected"));
        assert!(err.contains("ranges are not supported"), "unexpected message: {err}");
        assert!(err.contains("ranged"), "error should name the dependency: {err}");
    }
}

#[test]
fn validate_accepts_an_exact_version() {
    assert!(dep("version = \"1.2.0\"\nurl = \"https://x/a.so\"\n").validate("ok").is_ok());
}

#[test]
fn validate_rejects_empty_version() {
    let err = dep("version = \"\"\nurl = \"https://x/a.so\"\n").validate("blank").unwrap_err();
    assert!(err.contains("empty"), "unexpected message: {err}");
}

#[test]
fn validate_rejects_c_abi_without_symbols() {
    let d = dep("version = \"1.0.0\"\npath = \"libz.so\"\nabi = \"c\"\n");
    let err = d.validate("zlib").unwrap_err();
    assert!(err.contains("zlib"));
    assert!(err.contains("symbols"), "unexpected message: {err}");
}

#[test]
fn validate_rejects_symbols_without_c_abi() {
    // Declaring symbols but leaving abi = "jade" means the shim is never
    // generated and the symbols silently do nothing — catch it at parse time.
    let d = dep(
        "version = \"1.0.0\"\npath = \"a.so\"\n[symbols.foo]\nargs = []\nret = \"int\"\n",
    );
    let err = d.validate("mismatch").unwrap_err();
    assert!(err.contains("abi = \"c\""), "unexpected message: {err}");
}
