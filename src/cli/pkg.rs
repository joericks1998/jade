//! `jade pkg add` / `remove` / `install` / `update` / `list`.
//!
//! The manifest is the source of truth and `jade.lock` is derived from it.
//! There is no registry to query, so "update" means *reconcile the lock with
//! the manifest*, not *discover a newer version* — bumping a version is an edit
//! to `jade.toml` (by hand, or via `jade pkg add <name>` again).

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

/// `jade pkg add <name> --path <p> | --url <u> [--version <v>] [--header <h.h>]`
///
/// A `--header` says the artifact is a plain C library *and* how to bind it, so
/// adding one is a single step: the symbol table is generated and the shim is
/// built before the command returns. Requiring a separate `jade pkg bind` was
/// asking the user to know about a stage that has no decision in it.
pub fn run_add(
    name: &str,
    path: Option<&str>,
    url: Option<&str>,
    version: Option<&str>,
    c_abi: bool,
    header: Option<&str>,
    include: &[String],
) {
    let root = root_or_exit();

    let source = match (path, url) {
        (Some(p), None) => manifest::Source::Path(p),
        (None, Some(u)) => manifest::Source::Url(u),
        (Some(_), Some(_)) => {
            eprintln!("error: --path and --url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("error: `jade pkg add {name}` needs a source: --path <file> or --url <url>");
            eprintln!("       there is no package registry, so a dependency names where it lives");
            std::process::exit(1);
        }
    };

    // A header is only meaningful for a plain C library, so it implies --c-abi
    // rather than having to be paired with it.
    let abi = if c_abi || header.is_some() { Abi::C } else { Abi::Jade };

    manifest::add_dependency(&root, name, source, version, abi, None).unwrap_or_else(|e| fail(e));

    if let Some(h) = header {
        bind_header(&root, name, h, include, None, false).unwrap_or_else(|e| fail(e));
    } else if abi == Abi::C {
        // Nothing to bind from, and a C dependency is not installable until its
        // symbols exist. Stop after the manifest edit rather than failing
        // validation the user cannot yet satisfy — and say what would fix it.
        println!("added {name} to jade.toml");
        eprintln!(
            "note: {name} is a C library with no symbols yet. Either re-run with\n  \
             jade pkg add {name} --path <the .so> --header <its header.h>\n\
             to generate them, or write a [dependencies.{name}.symbols] table by hand."
        );
        return;
    }

    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);
    println!("added {name}");
}

// ── binding a C library ───────────────────────────────────────────────────────

/// The header path and include directories to record for a dependency.
///
/// Absolute, because the shim is compiled inside `libs/<dep>/` rather than
/// wherever the command was run — a relative `-I` resolves against the wrong
/// directory, and the failure is a "file not found" from cc at install time,
/// well away from the cause.
fn header_locations(header: &std::path::Path, include: &[String]) -> (Vec<String>, Vec<String>) {
    let abs = |p: &std::path::Path| -> String {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
    };
    let headers = vec![header
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| header.to_string_lossy().into_owned())];

    let mut dirs: Vec<String> = include.iter().map(|d| abs(std::path::Path::new(d))).collect();
    let parent = header.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = abs(parent.unwrap_or_else(|| std::path::Path::new(".")));
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
    (headers, dirs)
}

/// Read a header and write the tables into `jade.toml`. Shared by `add`,
/// `install` and `bind`, so all three produce the same manifest.
fn bind_header(
    root: &std::path::Path,
    name: &str,
    header: &str,
    include: &[String],
    only: Option<&str>,
    quiet: bool,
) -> Result<(), String> {
    let header_path = std::path::Path::new(header);
    if !header_path.exists() {
        return Err(format!("no such header: {header}"));
    }

    let binding = pkg::bindgen::from_header(header_path, include, only)?;
    if binding.symbols.is_empty() {
        return Err(format!(
            "{}\nnothing in {header} could be bound. The reasons above say why; a symbol table \
             written by hand can still cover what this could not.",
            binding.report()
        ));
    }

    let (headers, dirs) = header_locations(header_path, include);
    manifest::set_bindings(root, name, &binding.symbols, &binding.structs, &headers, &dirs)?;

    if !quiet {
        println!("{}", binding.report());
    }
    Ok(())
}

// ── bind ──────────────────────────────────────────────────────────────────────

/// `jade pkg bind <name> --header <h.h> [-I dir] [--only text] [--dry-run]`
///
/// Reads the header with clang and writes the symbol table into `jade.toml`, so
/// a library with two hundred entry points does not have to be transcribed by
/// hand. What it *could not* bind is printed, with reasons: a generator that
/// silently covers two thirds of an API is how the missing third is found at
/// run time.
pub fn run_bind(name: &str, header: &str, include: &[String], only: Option<&str>, dry_run: bool) {
    let root = root_or_exit();

    if dry_run {
        // Report only. Useful for looking at a large header before committing
        // its table to the manifest.
        let header_path = std::path::Path::new(header);
        if !header_path.exists() {
            fail(format!("no such header: {header}"));
        }
        let binding =
            pkg::bindgen::from_header(header_path, include, only).unwrap_or_else(|e| fail(e));
        println!("{}", binding.report());
        println!("\n(dry run — jade.toml unchanged)");
        return;
    }

    bind_header(&root, name, header, include, only, false).unwrap_or_else(|e| fail(e));
    println!("\nwrote [dependencies.{name}.symbols] to jade.toml");

    // Build it too. Re-binding and then leaving the shim stale is never what
    // anyone wanted, and it is the step that would otherwise be forgotten.
    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);
    println!("installed {name}");
}

/// Bind every `abi = "c"` dependency that names a header but has no symbols.
/// Returns whether the manifest was changed.
fn bind_missing_symbols(root: &std::path::Path, manifest: &ProjectManifest) -> bool {
    let Some(deps) = &manifest.dependencies else { return false };
    let mut changed = false;

    for (name, entry) in deps {
        if entry.abi != Abi::C || entry.symbols.is_some() {
            continue;
        }
        let Some(headers) = entry.headers.as_ref().filter(|h| !h.is_empty()) else { continue };

        // The manifest records a bare filename plus the directories to find it
        // in, so the lookup is the same one the shim compile will do.
        let dirs = entry.include_dirs.clone().unwrap_or_default();
        let found = dirs
            .iter()
            .map(|d| std::path::Path::new(d).join(&headers[0]))
            .find(|p| p.exists())
            .or_else(|| Some(root.join(&headers[0])).filter(|p| p.exists()));

        let Some(path) = found else {
            eprintln!(
                "note: dependency '{name}' names header '{}' but it was not found, so no symbols \
                 were generated. Point at it with\n  jade pkg bind {name} --header <path>",
                headers[0]
            );
            continue;
        };

        println!("binding {name} from {}", headers[0]);
        match bind_header(root, name, &path.to_string_lossy(), &dirs, None, false) {
            Ok(()) => changed = true,
            Err(e) => eprintln!("note: could not bind '{name}': {e}"),
        }
    }
    changed
}

// ── remove ────────────────────────────────────────────────────────────────────

/// `jade pkg remove <name>` — drop it from the manifest, the lock, and `libs/`.
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

/// `jade pkg install [--locked]`
///
/// Without `--locked`, resolves anything the lock is missing and writes it.
/// With `--locked`, refuses to change the lock — the CI mode, where a manifest
/// edit that was never locked should fail rather than silently resolve.
pub fn run_install(locked: bool) {
    let root = root_or_exit();
    let mut manifest = load_or_exit(&root);

    let has_deps = manifest.dependencies.as_ref().is_some_and(|d| !d.is_empty());
    if !has_deps {
        println!("no dependencies");
        return;
    }

    // Bind anything that says which header to read but has no symbols yet.
    // Without this a hand-written entry naming a header would install a shim
    // with nothing in it, and the user would have to know that a separate
    // command exists to fill it.
    //
    // Only when `symbols` is *absent*: a committed manifest already carries
    // them, so a fresh clone installs without needing clang at all. Re-running
    // after a header changes is `jade pkg bind`, which is an explicit act.
    if !locked && bind_missing_symbols(&root, &manifest) {
        manifest = load_or_exit(&root);
    }

    let existing = lock::read(&root).unwrap_or_else(|e| fail(e));

    if locked {
        let lock = existing.unwrap_or_else(|| {
            fail("--locked was given but there is no jade.lock")
        });
        pkg::verify_in_sync(&manifest, &lock).unwrap_or_else(|e| fail(e));
        // A rebuilt local dependency is a stale lock, and this mode is where a
        // stale lock has to be an error rather than a fixup.
        pkg::verify_local_unchanged(&root, &lock).unwrap_or_else(|e| fail(e));
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        pkg::build_c_shims(&root, &lock, &manifest).unwrap_or_else(|e| fail(e));
        println!("installed {} dependencies from jade.lock", lock.packages.len());
        return;
    }

    // An in-sync lock is authoritative for anything with a URL: reuse it so
    // `jade pkg install` does not re-fetch every artifact just to recompute
    // digests it already has. A local `path` dependency is the exception — its
    // source is a file the user rebuilds, so it gets re-hashed every time.
    if let Some(mut lock) = existing.filter(|l| pkg::verify_in_sync(&manifest, l).is_ok()) {
        let changed = pkg::refresh_local(&root, &mut lock);
        if !changed.is_empty() {
            lock::write(&root, &lock).unwrap_or_else(|e| fail(e));
            println!("re-pinned {} (local source changed)", changed.join(", "));
        }
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

/// `jade pkg update [name]` — re-resolve against the manifest and rewrite the lock.
///
/// This re-fetches to pick up an artifact republished at the same URL. It
/// cannot find a *newer version*: with no registry there is nothing to ask, so
/// moving to 2.0.0 means editing `jade.toml` (or `jade pkg add <name> --version`).
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

/// `jade pkg list` — what is locked, and whether it is installed here.
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
            // A local dependency whose source has been rebuilt is installed but
            // out of date, and saying only "installed" is how that goes
            // unnoticed. The next `jade pkg install` or `jade run` re-pins it.
            Some(a) if dir.join(&a.file).exists() => match pkg::local_drift(&root, p) {
                true => "installed (local source changed — run `jade pkg install`)",
                false => "installed",
            },
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
