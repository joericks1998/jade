//! `jade pkg add` / `remove` / `install` / `update` / `list`.
//!
//! The manifest is the source of truth and `jade.lock` is derived from it.
//! There is no registry to query, so "update" means *reconcile the lock with
//! the manifest*, not *discover a newer version* — bumping a version is an edit
//! to `jade.toml` (by hand, or via `jade pkg add <name>` again).
//!
//! A dependency is a Jade source package or a Rust package. The lock and `libs/`
//! hold source packages alone; a Rust package is written to the manifest and
//! built by cargo with the program (see `crate::rustpkg`).

use std::path::PathBuf;

use crate::pkg::{self, fetch::HttpFetcher, lock, manifest};
use crate::project::{self, Abi, ProjectManifest};

/// Resolve the project root, or exit — every command here is project-scoped.
fn root_or_exit() -> PathBuf {
    match project::find_project_root() {
        Some(r) => r,
        None => {
            eprintln!(
                "error: not inside a Jade project (no jade.toml with a [project] or [package] section)"
            );
            eprintln!("       run `jade init` to create one");
            std::process::exit(1);
        }
    }
}

fn load_or_exit(root: &std::path::Path) -> ProjectManifest {
    match project::load_project(root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    std::process::exit(1);
}

/// Fail out of `jade pkg add`, taking the manifest edit with it.
///
/// `add` has to write the entry before it can validate it — resolving reads the
/// dependency back out of `jade.toml`. So a failure lands *after* the write.
/// Leaving the entry behind was worse than it sounds: every other `pkg` command
/// re-validates the whole manifest, so one `add` that failed on a missing
/// directory made `install`, `list` and even a later successful `add` fail on an
/// orphan the user never managed to add, with nothing naming it as the cause.
///
/// Only a newly-created entry is removed. `add` replaces an existing dependency
/// outright, and rolling that back would delete a working entry to clean up
/// after a failed attempt to change it — so an existing one is left as it is,
/// and the message says the file was touched.
fn fail_new_dependency(
    root: &std::path::Path,
    name: &str,
    existed: bool,
    e: impl std::fmt::Display,
) -> ! {
    eprintln!("error: {e}");
    if existed {
        eprintln!(
            "note: [dependencies.{name}] in jade.toml was already replaced, and is left as it is"
        );
    } else if matches!(manifest::remove_dependency(root, name), Ok(true)) {
        eprintln!("note: {name} was not added — jade.toml is unchanged");
    }
    std::process::exit(1);
}

/// Whether `[dependencies.<name>]` is already in the manifest.
///
/// Read before `add` writes, so a rollback can tell "undo what I just created"
/// from "leave what was already there".
fn dependency_exists(root: &std::path::Path, name: &str) -> bool {
    project::load_project(root)
        .ok()
        .and_then(|m| m.dependencies)
        .is_some_and(|d| d.contains_key(name))
}

/// Re-resolve every dependency and write `jade.lock`, then install.
fn relock_and_install(root: &std::path::Path, manifest: &ProjectManifest) {
    try_relock_and_install(root, manifest).unwrap_or_else(|e| fail(e));
}

/// The same work, handing the error back instead of exiting.
///
/// `jade pkg add` needs this so it can undo its manifest edit before it dies;
/// every other caller has nothing to undo and uses the exiting form above.
fn try_relock_and_install(
    root: &std::path::Path,
    manifest: &ProjectManifest,
) -> Result<(), String> {
    let fetcher = HttpFetcher::new();
    let resolved = pkg::resolve(root, manifest, &fetcher)?;
    lock::write(root, &resolved)?;
    pkg::materialize(root, &resolved, &fetcher)
}

// ── add ───────────────────────────────────────────────────────────────────────

/// Purpose: This struct holds a `--git` source and the commit it names.
#[derive(Clone, Copy)]
pub struct GitSource<'a> {
    /// The repository.
    pub url: &'a str,
    /// `("tag", "v1.0")`, `("rev", …)` or `("branch", …)`.
    pub reference: (&'a str, &'a str),
}

/// `jade pkg add <name> --path <dir> | --git <url> | --url <archive> [--rust]`
///
/// What kind of dependency it is comes from what is there: a directory holding a
/// `Cargo.toml` is a Rust package, and one holding a `jade.toml` is a Jade source
/// package. `--rust` says so for a git repository or a crates.io version, which
/// cannot be looked inside before cargo fetches it.
pub fn run_add(
    name: &str,
    path: Option<&str>,
    url: Option<&str>,
    git: Option<GitSource<'_>>,
    version: Option<&str>,
    rust: bool,
    package: Option<&str>,
) {
    let root = root_or_exit();
    let crate_dir = path.map(|p| root.join(p)).filter(|d| d.join("Cargo.toml").is_file());
    let rust = rust
        || (crate_dir.is_some() && !path.is_some_and(|p| root.join(p).join("jade.toml").is_file()));

    if rust {
        let source = match (path, url, &git) {
            (Some(p), None, None) => manifest::Source::Path(p),
            (None, None, Some(g)) => manifest::Source::Git(g.url, g.reference),
            (None, None, None) if version.is_some() => manifest::Source::Registry,
            (None, Some(_), None) => fail(
                "a Rust package comes from --path, --git or a crates.io --version, and cargo \
                 fetches it; --url names a Jade source archive",
            ),
            _ => fail(format!(
                "`jade pkg add {name} --rust` needs a source: --path <the crate's directory>, \
                 --git <url> with --tag, --rev or --branch, or --version for crates.io"
            )),
        };
        if path.is_some() && crate_dir.is_none() {
            fail(format!(
                "{} holds no Cargo.toml; a Rust package's --path names the directory of its crate",
                path.unwrap_or_default()
            ));
        }
        add_rust(&root, name, source, version, package);
        return;
    }

    let source = match (path, url, &git) {
        (None, None, Some(g)) => manifest::Source::Git(g.url, g.reference),
        (Some(p), None, None) if root.join(p).is_dir() => manifest::Source::Path(p),
        (None, Some(u), None) if pkg::source::is_archive_url(u) => manifest::Source::Url(u),
        (Some(p), None, None) => fail(format!(
            "{p} is not a directory. A dependency is a Jade source package, a directory with a \
             jade.toml, or a Rust package, a directory with a Cargo.toml. A prebuilt .dylib or \
             .so is no longer one: reach a C library through a Rust crate that wraps it (see \
             design/v2-rust-packages.md)"
        )),
        (None, Some(u), None) => fail(format!(
            "{u} is not a .tar.gz archive of a Jade source package; a prebuilt library is no \
             longer a dependency (see design/v2-rust-packages.md)"
        )),
        _ => fail(format!(
            "`jade pkg add {name}` needs a source: --path <dir>, --git <url> or --url \
             <archive.tar.gz>; there is no package registry, so a dependency names where it lives"
        )),
    };
    add_source(&root, name, source, version);
}

/// Purpose: This function adds a Rust package, then builds it to report what it
/// exports.
///
/// Inputs:
/// - `root`: the project root.
/// - `name`: the name the program imports it by.
/// - `source`: where cargo finds it.
/// - `version`: the version to record, if given.
/// - `package`: the Rust package's own name, if it differs.
///
/// Outputs: none. It exits with a message on failure, leaving `jade.toml` as it
/// was.
fn add_rust(
    root: &std::path::Path,
    name: &str,
    source: manifest::Source<'_>,
    version: Option<&str>,
    package: Option<&str>,
) {
    let existed = dependency_exists(root, name);
    manifest::add_dependency(root, name, source, version, Abi::Rust, package)
        .unwrap_or_else(|e| fail(e));
    let manifest = load_or_exit(root);
    // Building is how anything about a Rust package is known: the records in its
    // library are its signatures. Doing it now means a package that does not
    // compile is refused here rather than at the next build.
    let built = crate::rustpkg::build(root, &manifest)
        .unwrap_or_else(|e| fail_new_dependency(root, name, existed, e));
    let pkg = built.as_ref().and_then(|b| b.packages.get(name));
    let (fns, structs) = pkg.map_or((0, 0), |p| (p.functions.len(), p.structs.len()));
    println!(
        "added {name}, a Rust package exporting {fns} function{} and {structs} struct{}",
        if fns == 1 { "" } else { "s" },
        if structs == 1 { "" } else { "s" }
    );
    if fns == 0 && structs == 0 {
        eprintln!(
            "note: {name} exports nothing a Jade program can call; mark functions and structs \
             with #[jade::export] in the crate"
        );
    }
}

/// Purpose: This function adds a Jade source package, then the dependencies it
/// declares.
///
/// Inputs:
/// - `root`: the project root.
/// - `name`: the name the program imports it by.
/// - `source`: where it is.
/// - `version`: the version to record, if given.
///
/// Outputs: none. It exits with a message on failure, leaving `jade.toml` as it
/// was.
fn add_source(
    root: &std::path::Path,
    name: &str,
    source: manifest::Source<'_>,
    version: Option<&str>,
) {
    if let manifest::Source::Path(p) = &source
        && let Err(e) = pkg::source::package_section(&root.join(p))
    {
        fail(e);
    }
    let existed = dependency_exists(root, name);
    manifest::add_dependency(root, name, source, version, Abi::Source, None)
        .unwrap_or_else(|e| fail(e));
    let manifest = load_or_exit(root);
    try_relock_and_install(root, &manifest)
        .unwrap_or_else(|e| fail_new_dependency(root, name, existed, e));
    println!("added {name}, compiled from source into each program that uses it");

    let Some(locked) = lock::read(root).ok().flatten().and_then(|l| l.get(name).cloned()) else {
        return;
    };
    let dir = pkg::source::package_dir(root, &locked);
    let local = locked.source.starts_with(lock::PATH_SOURCE);
    let (declared, unfollowed) = pkg::source::declared_dependencies(&dir, root, local);

    let have = load_or_exit(root).dependencies.unwrap_or_default();
    let mut added = Vec::new();
    for (dep, entry) in declared {
        match have.get(&dep) {
            // Two packages needing the same dependency is the ordinary case.
            Some(mine) if mine.version == entry.version => {}
            Some(mine) => fail(format!(
                "'{name}' needs {dep} {}, and this project already has {dep} {}.\n  \
                 One copy of a package is compiled into a program, so the two have to agree. \
                 Align them in jade.toml.",
                entry.version.as_deref().unwrap_or(pkg::LOCAL_VERSION),
                mine.version.as_deref().unwrap_or(pkg::LOCAL_VERSION)
            )),
            None => {
                manifest::add_entry(root, &dep, &entry).unwrap_or_else(|e| fail(e));
                added.push(dep);
            }
        }
    }
    if !unfollowed.is_empty() {
        eprintln!(
            "note: '{name}' also needs {}, which it names by a path on the machine it came \
             from.\n  Add each one yourself:\n    jade pkg add {} --path <where it is here>",
            unfollowed.join(", "),
            unfollowed[0]
        );
    }
    if !added.is_empty() {
        println!("{name} also needs {}", added.join(", "));
        let manifest = load_or_exit(root);
        try_relock_and_install(root, &manifest).unwrap_or_else(|e| fail(e));
    }
}

// ── remove ────────────────────────────────────────────────────────────────────

/// `jade pkg remove <name>` — drop it from the manifest, the lock, and `libs/`.
pub fn run_remove(name: &str) {
    let root = root_or_exit();

    // Capture the install directory before the lock loses the entry.
    let install_dir =
        lock::read(&root).ok().flatten().and_then(|l| l.get(name).map(|p| p.install_dir()));

    let removed = manifest::remove_dependency(&root, name).unwrap_or_else(|e| fail(e));
    if !removed {
        eprintln!("error: no dependency named '{name}' in jade.toml");
        std::process::exit(1);
    }

    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);

    if let Some(dir) = install_dir {
        let path = root.join(pkg::LIBS_DIR).join(dir);
        // Already gone is fine — `libs/` is disposable and may never have been
        // populated on this machine.
        match std::fs::remove_dir_all(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("warning: could not remove {}: {e}", path.display());
            }
            _ => {}
        }
    }

    println!("removed {name}");
}

// ── install ───────────────────────────────────────────────────────────────────

/// `jade pkg install [--locked]`
///
/// Without `--locked`, resolves anything the lock is missing and writes it.
/// With `--locked`, refuses to change the lock — the CI mode, where a manifest
/// edit that was never locked should fail rather than silently resolve. Rust
/// packages are validated and left for cargo to fetch at build time.
pub fn run_install(locked: bool) {
    let root = root_or_exit();
    let manifest = load_or_exit(&root);

    let has_deps = manifest.dependencies.as_ref().is_some_and(|d| !d.is_empty());
    if !has_deps {
        println!("no dependencies");
        return;
    }
    let rust = crate::rustpkg::dependencies(&manifest).len();
    let rust_note = |n: usize| {
        let plural =
            |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        match (n, rust) {
            (_, 0) => println!("installed {}", plural(n, "dependency", "dependencies")),
            (0, _) => println!(
                "nothing to install: {} with the program, by cargo",
                plural(rust, "Rust package builds", "Rust packages build")
            ),
            _ => println!(
                "installed {}; {} with the program, by cargo",
                plural(n, "source package", "source packages"),
                plural(rust, "Rust package builds", "Rust packages build")
            ),
        }
    };

    let existing = lock::read(&root).unwrap_or_else(|e| fail(e));

    if locked {
        let lock = existing.unwrap_or_else(|| fail("--locked was given but there is no jade.lock"));
        pkg::verify_in_sync(&manifest, &lock).unwrap_or_else(|e| fail(e));
        // An edited local dependency is a stale lock, and this mode is where a
        // stale lock has to be an error rather than a fixup.
        pkg::verify_local_unchanged(&root, &lock).unwrap_or_else(|e| fail(e));
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        rust_note(lock.packages.len());
        return;
    }

    // An in-sync lock is authoritative for anything with a URL: reuse it so
    // `jade pkg install` does not re-fetch every package just to recompute
    // digests it already has. A local `path` dependency is the exception — its
    // source is a directory the user edits, so it gets re-hashed every time.
    if let Some(mut lock) = existing.filter(|l| pkg::verify_in_sync(&manifest, l).is_ok()) {
        for (n, e) in manifest.dependencies.iter().flatten() {
            e.validate(n).unwrap_or_else(|e| fail(e));
        }
        let changed = pkg::refresh_local(&root, &mut lock);
        if !changed.is_empty() {
            lock::write(&root, &lock).unwrap_or_else(|e| fail(e));
            println!("re-pinned {} (local source changed)", changed.join(", "));
        }
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        rust_note(lock.packages.len());
        return;
    }

    relock_and_install(&root, &manifest);
    let n = lock::read(&root).ok().flatten().map_or(0, |l| l.packages.len());
    rust_note(n);
}

// ── update ────────────────────────────────────────────────────────────────────

/// `jade pkg update [name]` — re-resolve against the manifest and rewrite the lock.
///
/// This re-reads each source package to pick up a change published at the same
/// place. It cannot find a *newer version*: with no registry there is nothing to
/// ask, so moving to 2.0.0 means editing `jade.toml`.
pub fn run_update(name: Option<&str>) {
    let root = root_or_exit();
    let manifest = load_or_exit(&root);

    if let Some(name) = name {
        let known = manifest.dependencies.as_ref().is_some_and(|d| d.contains_key(name));
        if !known {
            eprintln!("error: no dependency named '{name}' in jade.toml");
            std::process::exit(1);
        }
    }

    relock_and_install(&root, &manifest);
    match name {
        Some(n) => println!("updated {n}"),
        None => println!("updated all dependencies"),
    }
}

// ── list ──────────────────────────────────────────────────────────────────────

/// `jade pkg list` — what is locked and whether it is installed here, and the
/// Rust packages cargo builds.
pub fn run_list() {
    let root = root_or_exit();
    let manifest = load_or_exit(&root);
    let rust = crate::rustpkg::dependencies(&manifest);

    let lock = lock::read(&root).unwrap_or_else(|e| fail(e));
    let packages = lock.as_ref().map(|l| l.packages.as_slice()).unwrap_or_default();
    if packages.is_empty() && rust.is_empty() {
        println!("no dependencies");
        return;
    }

    for p in packages {
        let installed = pkg::source::package_dir(&root, p).join("jade.toml").is_file();
        let status = match (installed, pkg::local_drift(&root, p)) {
            // A local dependency whose source has been edited is installed but
            // out of date, and saying only "installed" is how that goes
            // unnoticed. The next `jade pkg install` or `jade run` re-pins it.
            (true, true) => "installed (local source changed — run `jade pkg install`)",
            (true, false) => "installed",
            (false, _) => "not installed",
        };
        println!("{} {}  [{}]  {status}", p.name, p.version, p.abi);
    }
    for (name, d) in &rust {
        let from = d
            .path
            .as_deref()
            .map(|p| format!("path {p}"))
            .or_else(|| d.git.as_deref().map(|g| format!("git {g}")))
            .or_else(|| d.version.as_deref().map(|v| format!("crates.io {v}")))
            .unwrap_or_default();
        println!("{name}  [rust]  built by cargo with the program, from {from}");
    }
}
