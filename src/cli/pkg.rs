//! `jade add` / `remove` / `install` / `update` / `list`.
//!
//! The manifest is the source of truth and `jade.lock` is derived from it.
//! There is no registry to query, so "update" means *reconcile the lock with
//! the manifest*, not *discover a newer version* — bumping a version is an edit
//! to `jade.toml` (by hand, or via `jade add <name>` again).

use std::path::PathBuf;

use crate::pkg::{self, fetch::HttpFetcher, lock, manifest};
use crate::project::{self, Abi, ProjectManifest};

/// Resolve the project root, or exit — every command here is project-scoped.
fn root_or_exit() -> PathBuf {
    match project::find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: not inside a Jade project (no jade.toml with a [project] section)");
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

/// Re-resolve every dependency and write `jade.lock`, then install.
fn relock_and_install(root: &std::path::Path, manifest: &ProjectManifest) {
    let fetcher = HttpFetcher::new();
    let resolved = pkg::resolve(root, manifest, &fetcher).unwrap_or_else(|e| fail(e));
    lock::write(root, &resolved).unwrap_or_else(|e| fail(e));
    pkg::materialize(root, &resolved, &fetcher).unwrap_or_else(|e| fail(e));
    pkg::build_c_shims(root, &resolved, manifest).unwrap_or_else(|e| fail(e));
}

// ── add ───────────────────────────────────────────────────────────────────────

/// `jade add <name> --path <p> | --url <u> [--version <v>] [--abi c]`
pub fn run_add(name: &str, path: Option<&str>, url: Option<&str>, version: Option<&str>, c_abi: bool) {
    let root = root_or_exit();

    let source = match (path, url) {
        (Some(p), None) => manifest::Source::Path(p),
        (None, Some(u)) => manifest::Source::Url(u),
        (Some(_), Some(_)) => {
            eprintln!("error: --path and --url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("error: `jade add {name}` needs a source: --path <file> or --url <url>");
            eprintln!("       there is no package registry, so a dependency names where it lives");
            std::process::exit(1);
        }
    };

    let abi = if c_abi { Abi::C } else { Abi::Jade };
    if c_abi {
        // The symbol table cannot be supplied on the command line — it is a
        // per-symbol prototype list. Write the entry, then point at the file.
        eprintln!(
            "note: add a [dependencies.{name}.symbols] table to jade.toml describing the C \
             symbols to bind, then run `jade install`"
        );
    }

    manifest::add_dependency(&root, name, source, version, abi, None).unwrap_or_else(|e| fail(e));

    // A C dependency is not installable until its symbols are declared, so stop
    // after the manifest edit rather than failing validation the user can't fix
    // yet.
    if c_abi {
        println!("added {name} to jade.toml");
        return;
    }

    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);
    println!("added {name}");
}

// ── remove ────────────────────────────────────────────────────────────────────

/// `jade remove <name>` — drop it from the manifest, the lock, and `libs/`.
pub fn run_remove(name: &str) {
    let root = root_or_exit();

    // Capture the install directory before the lock loses the entry.
    let install_dir = lock::read(&root)
        .ok()
        .flatten()
        .and_then(|l| l.get(name).map(|p| p.install_dir()));

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

/// `jade install [--locked]`
///
/// Without `--locked`, resolves anything the lock is missing and writes it.
/// With `--locked`, refuses to change the lock — the CI mode, where a manifest
/// edit that was never locked should fail rather than silently resolve.
pub fn run_install(locked: bool) {
    let root = root_or_exit();
    let manifest = load_or_exit(&root);

    let has_deps = manifest.dependencies.as_ref().is_some_and(|d| !d.is_empty());
    if !has_deps {
        println!("no dependencies");
        return;
    }

    let existing = lock::read(&root).unwrap_or_else(|e| fail(e));

    if locked {
        let lock = existing.unwrap_or_else(|| {
            fail("--locked was given but there is no jade.lock")
        });
        pkg::verify_in_sync(&manifest, &lock).unwrap_or_else(|e| fail(e));
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        pkg::build_c_shims(&root, &lock, &manifest).unwrap_or_else(|e| fail(e));
        println!("installed {} dependencies from jade.lock", lock.packages.len());
        return;
    }

    // An in-sync lock is authoritative: reuse it so `jade install` does not
    // re-fetch every artifact just to recompute digests it already has.
    if let Some(lock) = existing.filter(|l| pkg::verify_in_sync(&manifest, l).is_ok()) {
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        pkg::build_c_shims(&root, &lock, &manifest).unwrap_or_else(|e| fail(e));
        println!("installed {} dependencies", lock.packages.len());
        return;
    }

    relock_and_install(&root, &manifest);
    let n = manifest.dependencies.as_ref().map_or(0, |d| d.len());
    println!("installed {n} dependencies");
}

// ── update ────────────────────────────────────────────────────────────────────

/// `jade update [name]` — re-resolve against the manifest and rewrite the lock.
///
/// This re-fetches to pick up an artifact republished at the same URL. It
/// cannot find a *newer version*: with no registry there is nothing to ask, so
/// moving to 2.0.0 means editing `jade.toml` (or `jade add <name> --version`).
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

/// `jade list` — what is locked, and whether it is installed here.
pub fn run_list() {
    let root = root_or_exit();

    let Some(lock) = lock::read(&root).unwrap_or_else(|e| fail(e)) else {
        println!("no dependencies (no jade.lock)");
        return;
    };
    if lock.packages.is_empty() {
        println!("no dependencies");
        return;
    }

    let platform = pkg::fetch::platform_tag().unwrap_or("unsupported");
    for p in &lock.packages {
        let dir = root.join(pkg::LIBS_DIR).join(p.install_dir());
        let here = p
            .artifacts
            .get(platform)
            .or_else(|| p.artifacts.get(pkg::ANY_PLATFORM));

        let status = match here {
            Some(a) if dir.join(&a.file).exists() => "installed",
            Some(_) => "not installed",
            None => "unavailable on this platform",
        };
        let platforms: Vec<&str> = p.artifacts.keys().map(String::as_str).collect();
        println!(
            "{name} {version}  [{abi}]  {status}\n  platforms: {plats}",
            name = p.name,
            version = p.version,
            abi = p.abi,
            status = status,
            plats = platforms.join(", "),
        );
    }
}
