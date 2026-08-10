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
    assert!(matches!(resolve_library_import(&libs, "plainimport", Path::new("/root")), Ok(None)));
    // Unknown lib name → not a lib reference.
    assert!(matches!(resolve_library_import(&libs, "other/mod", Path::new("/root")), Ok(None)));
}

#[test]
fn resolve_import_with_allowlist_hit() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["math.jde".into()]) },
    );
    let resolved =
        resolve_library_import(&libs, "utils/math", Path::new("/root")).unwrap().unwrap();
    assert_eq!(resolved.kind, ImportKind::Jade);
    assert_eq!(resolved.path, PathBuf::from("/root/src/utils/math.jde"));
}

#[test]
fn resolve_import_with_allowlist_native() {
    let libs = libs_with(
        "utils",
        LibraryEntry { path: "src/utils".into(), files: Some(vec!["fast.dylib".into()]) },
    );
    let resolved =
        resolve_library_import(&libs, "utils/fast", Path::new("/root")).unwrap().unwrap();
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
    let resolved = resolve_library_import(&libs, "utils/m", Path::new("/root")).unwrap().unwrap();
    // Absolute lib path ignores `root`.
    assert_eq!(resolved.path, PathBuf::from("/abs/utils/m.jde"));
}

#[test]
fn resolve_import_no_allowlist_probes_disk() {
    let tmp = TempDir::new("libprobe");
    let libdir = tmp.path().join("mylib");
    std::fs::create_dir_all(&libdir).unwrap();
    std::fs::write(libdir.join("thing.jde"), "").unwrap();

    let libs = libs_with("mylib", LibraryEntry { path: "mylib".into(), files: None });
    let resolved = resolve_library_import(&libs, "mylib/thing", tmp.path()).unwrap().unwrap();
    assert_eq!(resolved.kind, ImportKind::Jade);
    assert_eq!(resolved.path, libdir.join("thing.jde"));
}

#[test]
fn resolve_import_no_allowlist_missing_returns_jde_candidate() {
    let tmp = TempDir::new("libmiss");
    let libs = libs_with("mylib", LibraryEntry { path: "mylib".into(), files: None });
    // Nothing on disk → returns the `.jde` candidate as Jade so caller emits a
    // normal not-found error.
    let resolved = resolve_library_import(&libs, "mylib/ghost", tmp.path()).unwrap().unwrap();
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
    std::fs::write(root.join("jade.toml"), "[project]\nname = \"rooted\"\n").unwrap();

    let found = find_project_root_from(&nested).expect("should find root by walking up");
    // Canonicalize both to compare (temp dirs may be symlinked on macOS).
    assert_eq!(std::fs::canonicalize(&found).unwrap(), std::fs::canonicalize(root).unwrap());
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
    let err = load_project(tmp.path()).expect_err("no manifest to load");
    // Absent, and it says so as a variant rather than as prose: a caller that
    // falls back to a default manifest is entitled to do that here, and only
    // here.
    assert!(matches!(err, ManifestError::Missing(_)), "should be Missing: {err}");
    assert!(!err.is_present());
}

#[test]
fn load_project_invalid_toml_errors() {
    let tmp = TempDir::new("loadbad");
    std::fs::write(tmp.path().join("jade.toml"), "[project\nname =").unwrap();
    let err = load_project(tmp.path()).expect_err("the manifest is not valid TOML");
    // Present and broken, which is the distinction the type exists for. The
    // message has to carry the parse error, because the line it names is the
    // only part that says what to fix.
    assert!(matches!(err, ManifestError::Malformed(..)), "should be Malformed: {err}");
    assert!(err.is_present());
    let msg = err.to_string();
    assert!(msg.contains("is not valid TOML"), "should name the fault: {msg}");
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
    std::fs::write(root.join("notes.txt"), "").unwrap(); // wrong ext

    let found = find_test_files(root, None);
    let names: Vec<String> =
        found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
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
        LibraryEntry {
            path: "libs/fastmath-1.0.0".into(),
            files: Some(vec!["fastmath.so".into()]),
        },
    );
    let resolved = resolve_library_import(&libs, "fastmath", Path::new("/root")).unwrap().unwrap();
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
    let resolved =
        resolve_library_import(&libs, "utils/math", Path::new("/root")).unwrap().unwrap();
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
        let err = d.validate("ranged").expect_err(&format!("range {range:?} should be rejected"));
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
    let d = dep("version = \"1.0.0\"\npath = \"a.so\"\n[symbols.foo]\nargs = []\nret = \"int\"\n");
    let err = d.validate("mismatch").unwrap_err();
    assert!(err.contains("abi = \"c\""), "unexpected message: {err}");
}

// ── Import resolution and the check-time graph walk ───────────────────────────
//
// These cover the gap v1.1.33 closed: `jade check` used to accept a `use` naming
// a module that does not exist, because import resolution happened when the VM
// executed the Import opcode rather than at compile time.

/// Parse a source string and hand back its import paths, the way
/// `walk_imports`'s caller does.
fn paths_of(source: &str) -> Vec<(String, crate::frontend::error::Span)> {
    let tokens = crate::frontend::lexer::tokenize(source).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    program_import_paths(&program)
}

/// An import context anchored at `dir` with no project and no registered libs —
/// the plain "file sitting next to another file" case.
fn bare_ctx(dir: &Path) -> (HashMap<String, LibraryEntry>, PathBuf) {
    (HashMap::new(), dir.to_path_buf())
}

#[test]
fn a_use_naming_nothing_is_rejected() {
    let tmp = TempDir::new("missing_import");
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    let err = walk_imports(&paths_of("use totally_made_up_module\n"), &ctx).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cannot find import"), "unexpected message: {msg}");
    assert!(msg.contains("totally_made_up_module"), "message should name it: {msg}");
}

#[test]
fn a_use_naming_a_sibling_file_resolves() {
    let tmp = TempDir::new("sibling_import");
    std::fs::write(tmp.path().join("helper.jde"), "fn hi() { return 1 }\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(walk_imports(&paths_of("use helper\n"), &ctx).is_ok());
}

#[test]
fn a_builtin_package_resolves_without_touching_disk() {
    // `std::math` is compiled in; an empty directory must not change the answer.
    let tmp = TempDir::new("builtin_import");
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert_eq!(resolve_import(&ctx, "std/math").unwrap(), ImportTarget::Builtin);
    assert!(walk_imports(&paths_of("use std::math\n"), &ctx).is_ok());
}

#[test]
fn an_invented_std_package_is_rejected() {
    // `std::` is not a blanket escape hatch — a package that is not compiled in
    // falls through to file resolution and must fail like any other name.
    let tmp = TempDir::new("fake_std");
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(walk_imports(&paths_of("use std::totally_fake\n"), &ctx).is_err());
}

#[test]
fn from_use_is_walked_too() {
    let tmp = TempDir::new("from_use");
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(walk_imports(&paths_of("from nowhere use thing\n"), &ctx).is_err());
}

#[test]
fn a_broken_import_one_level_down_is_found() {
    // The walk is transitive: a module that itself imports nothing real breaks
    // the program that imports it, so check must say so.
    let tmp = TempDir::new("transitive");
    std::fs::write(tmp.path().join("mid.jde"), "use also_missing\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    let err = walk_imports(&paths_of("use mid\n"), &ctx).unwrap_err();
    assert!(err.to_string().contains("also_missing"), "unexpected: {err}");
}

#[test]
fn an_imported_module_resolves_against_its_own_directory() {
    // `app.jde` imports `sub/mid.jde`, which imports `leaf`. `leaf.jde` sits
    // beside *mid*, not beside app — so a walk that kept using the importer's
    // directory would wrongly call this missing.
    let tmp = TempDir::new("own_dir");
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/mid.jde"), "use leaf\n").unwrap();
    std::fs::write(tmp.path().join("sub/leaf.jde"), "fn hi() { return 1 }\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(walk_imports(&paths_of("use sub::mid\n"), &ctx).is_ok());
}

#[test]
fn a_circular_import_terminates() {
    // Two modules importing each other must not recurse forever. Whether a cycle
    // should be a check-time error is a separate question; this pins that the
    // walk stops.
    let tmp = TempDir::new("cycle");
    std::fs::write(tmp.path().join("a.jde"), "use b\n").unwrap();
    std::fs::write(tmp.path().join("b.jde"), "use a\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(walk_imports(&paths_of("use a\n"), &ctx).is_ok());
}

#[test]
fn a_native_module_is_checked_for_existence_but_not_loaded() {
    // An empty file with a library extension is not a loadable package. The walk
    // must still accept it: opening it would run its initializer, and `check`
    // does not execute the program it is checking.
    let tmp = TempDir::new("native_import");
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    std::fs::write(tmp.path().join(format!("fastmath.{ext}")), b"").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    assert!(matches!(resolve_import(&ctx, "fastmath").unwrap(), ImportTarget::Native(_)));
    assert!(walk_imports(&paths_of("use fastmath\n"), &ctx).is_ok());
}

// ── [package] ─────────────────────────────────────────────────────────────────
//
// A package's shape is declared in jade.toml so it is a property of the project
// rather than of the command line somebody remembered to type. These pin the
// two things that makes worth having: sensible defaults, and errors that name
// the value to fix.

fn package(src: &str) -> crate::project::PackageSection {
    let m: ProjectManifest = toml::from_str(src).expect("valid manifest toml");
    m.package.expect("a [package] section")
}

#[test]
fn a_package_entry_defaults_to_its_own_name() {
    let p = package("[package]\nname = \"mathlib\"\n");
    assert_eq!(p.entry_file(), "mathlib.jde");
    assert!(p.validate().is_ok());
}

#[test]
fn a_package_entry_can_be_named_explicitly() {
    let p = package("[package]\nname = \"mathlib\"\nentry = \"src/api.jde\"\n");
    assert_eq!(p.entry_file(), "src/api.jde");
    assert!(p.validate().is_ok());
}

#[test]
fn a_package_artifact_takes_the_platform_extension() {
    // `use <name>` resolves a package by stem, so the artifact has to be a real
    // shared library named after the package.
    let p = package("[package]\nname = \"mathlib\"\n");
    let expected = if cfg!(target_os = "macos") { "mathlib.dylib" } else { "mathlib.so" };
    assert_eq!(p.artifact_file(), expected);
}

#[test]
fn a_package_name_that_is_not_an_identifier_is_rejected() {
    // The name becomes a filename *and* the name `use` binds, so a hyphen would
    // produce a package that cannot be imported under the name it was built as.
    let err = package("[package]\nname = \"my-lib\"\n").validate().unwrap_err();
    assert!(err.contains("my-lib"), "error should name the value: {err}");
    assert!(err.contains("use my-lib"), "error should say why it matters: {err}");

    assert!(package("[package]\nname = \"my_lib2\"\n").validate().is_ok());
}

#[test]
fn a_package_entry_must_be_a_jade_file() {
    let err = package("[package]\nname = \"m\"\nentry = \"m.dylib\"\n").validate().unwrap_err();
    assert!(err.contains("m.dylib"), "error should name the file: {err}");
    assert!(err.contains(".jde"), "error should say what is expected: {err}");
}

#[test]
fn package_sources_must_list_the_entry() {
    // sources reads as the package's complete inventory, so the entry belongs in
    // it — otherwise the list means "the other files", which nothing says.
    let err = package("[package]\nname = \"mathlib\"\nsources = [\"helper.jde\"]\n")
        .validate()
        .unwrap_err();
    assert!(err.contains("mathlib.jde"), "error should name the entry: {err}");

    assert!(
        package("[package]\nname = \"mathlib\"\nsources = [\"helper.jde\", \"mathlib.jde\"]\n")
            .validate()
            .is_ok()
    );
}

#[test]
fn package_sources_reject_a_non_jade_file() {
    let err = package("[package]\nname = \"m\"\nsources = [\"m.jde\", \"libz.so\"]\n")
        .validate()
        .unwrap_err();
    assert!(err.contains("libz.so"), "error should name the file: {err}");
    assert!(err.contains("[dependencies]"), "error should point at the right home: {err}");
}

#[test]
fn package_sources_reject_a_duplicate() {
    let err = package("[package]\nname = \"m\"\nsources = [\"m.jde\", \"a.jde\", \"a.jde\"]\n")
        .validate()
        .unwrap_err();
    assert!(err.contains("a.jde"), "error should name the duplicate: {err}");
}

#[test]
fn empty_package_lists_are_rejected_rather_than_silently_meaning_nothing() {
    // `sources = []` and `exports = []` each read as "I meant something" while
    // doing the opposite of the sensible default. Omitting them is the way to
    // ask for the default, so an empty list is a mistake worth reporting.
    let err = package("[package]\nname = \"m\"\nsources = []\n").validate().unwrap_err();
    assert!(err.contains("omit it"), "error should say what to do instead: {err}");

    let err = package("[package]\nname = \"m\"\nexports = []\n").validate().unwrap_err();
    assert!(err.contains("binding nothing"), "error should say what it would build: {err}");
}

#[test]
fn a_manifest_without_a_package_section_has_none() {
    let m: ProjectManifest =
        toml::from_str("[project]\nname = \"app\"\n").expect("valid manifest toml");
    assert!(m.package.is_none(), "[package] is opt-in");
}

// ── reachable_jade_modules ────────────────────────────────────────────────────

#[test]
fn reachable_modules_follows_imports_transitively() {
    // What `jade build --lib` checks a package's declared sources against.
    let tmp = TempDir::new("reachable");
    std::fs::write(tmp.path().join("entry.jde"), "use mid\n").unwrap();
    std::fs::write(tmp.path().join("mid.jde"), "use leaf\n").unwrap();
    std::fs::write(tmp.path().join("leaf.jde"), "fn f() { return 1 }\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    let reached = crate::project::reachable_jade_modules(&paths_of("use mid\n"), &ctx).unwrap();

    let names: std::collections::HashSet<String> = reached
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert!(names.contains("mid.jde"), "a direct import: {names:?}");
    assert!(names.contains("leaf.jde"), "a transitive import: {names:?}");
    assert!(!names.contains("entry.jde"), "the root of the walk is not something it reached");
}

#[test]
fn reachable_modules_excludes_a_file_nothing_imports() {
    // The case `sources` exists to catch: a file sitting in the directory that
    // no `use` reaches would not be in the artifact.
    let tmp = TempDir::new("reachorphan");
    std::fs::write(tmp.path().join("used.jde"), "fn f() { return 1 }\n").unwrap();
    std::fs::write(tmp.path().join("orphan.jde"), "fn g() { return 2 }\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    let reached = crate::project::reachable_jade_modules(&paths_of("use used\n"), &ctx).unwrap();
    assert_eq!(reached.len(), 1);
    assert!(reached.iter().next().unwrap().ends_with("used.jde"));
}

#[test]
fn reachable_modules_terminates_on_a_cycle() {
    let tmp = TempDir::new("reachcycle");
    std::fs::write(tmp.path().join("a.jde"), "use b\n").unwrap();
    std::fs::write(tmp.path().join("b.jde"), "use a\n").unwrap();
    let (libs, dir) = bare_ctx(tmp.path());
    let ctx = ImportContext { libraries: &libs, project_root: None, source_dir: dir };

    let reached = crate::project::reachable_jade_modules(&paths_of("use a\n"), &ctx).unwrap();
    assert_eq!(reached.len(), 2, "both files, visited once each");
}
