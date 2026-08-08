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

/// Fail out of `jade pkg add`, taking the manifest edit with it.
///
/// `add` has to write the entry before it can validate it — binding a C library
/// reads the dependency back out of `jade.toml`, and resolving needs it there to
/// resolve. So a failure lands *after* the write. Leaving the entry behind was
/// worse than it sounds: every other `pkg` command re-validates the whole
/// manifest, so one `add` that failed on a missing file made `install`, `list`
/// and even a later successful `add` fail on an orphan the user never managed to
/// add, with nothing naming it as the cause.
///
/// Only a newly-created entry is removed. `add` replaces an existing dependency
/// outright, and rolling that back would delete a working entry to clean up
/// after a failed attempt to change it — so an existing one is left as it is,
/// and the message says the file was touched.
fn fail_new_dependency(root: &std::path::Path, name: &str, existed: bool, e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    if existed {
        eprintln!("note: [dependencies.{name}] in jade.toml was already replaced, and is left as it is");
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
fn try_relock_and_install(root: &std::path::Path, manifest: &ProjectManifest) -> Result<(), String> {
    let resolved = relock_and_fetch(root, manifest)?;
    pkg::build_c_shims(root, &resolved, manifest)
}

/// Everything up to the binding shims: resolve, write `jade.lock`, copy the
/// artifacts into `libs/`.
///
/// Split out for the one case that legitimately cannot build a shim yet — a C
/// dependency added with placeholder prototypes. Stopping short of the lock
/// entirely would be worse: `jade run` refuses to resolve on its own, so the
/// user would fill in the blanks, run their program, and be sent to
/// `jade pkg install` for a step that had nothing to do with what they just
/// fixed. Locking now leaves the shim as the only thing missing, which is
/// exactly what filling in the blanks completes.
fn relock_and_fetch(
    root: &std::path::Path,
    manifest: &ProjectManifest,
) -> Result<lock::Lockfile, String> {
    let fetcher = HttpFetcher::new();
    let resolved = pkg::resolve(root, manifest, &fetcher)?;
    lock::write(root, &resolved)?;
    pkg::materialize(root, &resolved, &fetcher)?;
    Ok(resolved)
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

    // Catch a missing file before touching jade.toml. Resolution would catch it
    // anyway, but only after the entry was written, and "no such file" is a much
    // better answer here than the same fact reported as a resolution failure.
    if let Some(p) = path {
        let full = root.join(p);
        if !full.exists() {
            eprintln!("error: {p} does not exist");
            eprintln!("       a --path dependency names a file to copy, relative to the project root");
            std::process::exit(1);
        }
        // A dependency is a *loadable* shared library, and nothing downstream
        // checks that. Without this, a file that merely has the right name is
        // copied into libs/, resolved, linked and built, and first refused by
        // the dynamic loader when the finished program runs.
        if !pkg::bindgen::is_loadable_object(&full) {
            eprintln!("error: {p} is not a shared library");
            eprintln!(
                "       A dependency is a prebuilt .dylib or .so. This file does not start with\n       \
                 a Mach-O or ELF header, so nothing could load it.\n\n       \
                 The usual cause is compiling the header instead of the source:\n         \
                 clang -o lib{0}.dylib {0}.h      # makes a precompiled header, not a library\n         \
                 clang -dynamiclib -o lib{0}.dylib {0}.c",
                name
            );
            std::process::exit(1);
        }
    }

    let existed = dependency_exists(&root, name);

    // A local artifact can be read for its export table — which says both what
    // kind of library it is and, later, whether a candidate header describes it.
    let lib_path = path.map(|p| root.join(p)).filter(|p| p.exists());

    // What the user said, then what the artifact says, then the default. A
    // header is only meaningful for a plain C library, so passing one is itself
    // a statement of the ABI.
    let abi = if c_abi || header.is_some() {
        Abi::C
    } else {
        match lib_path.as_deref().and_then(detect_abi) {
            Some(Abi::C) => {
                println!("{name} exports no jade_pkg_init, so it is a plain C library");
                Abi::C
            }
            Some(Abi::Jade) => Abi::Jade,
            // A URL dependency has nothing to read yet, and an unreadable table
            // proves nothing. Assume a Jade package, which is what `--c-abi` is
            // there to correct.
            None => Abi::Jade,
        }
    };

    manifest::add_dependency(&root, name, source, version, abi, None).unwrap_or_else(|e| fail(e));

    // Given, or found. A .so has no headers in it — names only, and C does not
    // mangle them — so one has to come from the filesystem; but the user should
    // not have to know where. `libsqlite3.dylib` implies `sqlite3.h`, and the
    // export table says whether the one we found is really this library's.
    let found = header.map(std::path::PathBuf::from).or_else(|| {
        if abi != Abi::C {
            return None;
        }
        let lib = lib_path.as_deref()?;
        let h = pkg::bindgen::discover_header(lib, &root, name)?;
        println!("found header {}", h.display());
        Some(h)
    });

    if let Some(h) = found {
        bind_header(&root, name, &h.to_string_lossy(), include, None, false, lib_path.as_deref())
            .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));
    } else if abi == Abi::C {
        // No header, but the library still says what it exports. Write those
        // names with `"?"` for the prototype rather than nothing at all: the
        // user fills in blanks in a file that already lists every function,
        // instead of going to look for a header that may not exist on this
        // machine. Guessing the types would be worse than leaving them blank —
        // see `project::UNRESOLVED`.
        let found = lib_path.as_deref().map(pkg::bindgen::placeholder_symbols).unwrap_or_default();
        if !found.is_empty() {
            let names: Vec<&str> = found.keys().map(String::as_str).collect();
            let empty = std::collections::BTreeMap::new();
            manifest::set_bindings(&root, name, &found, &empty, &[], &[])
                .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));

            // Lock and copy the artifact, but do not try to bind it — filling
            // in the prototypes is then the only thing left between here and a
            // working dependency.
            let m = load_or_exit(&root);
            relock_and_fetch(&root, &m)
                .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));

            println!("added {name} to jade.toml");
            println!(
                "{} of its symbols are listed there with no signature: {}",
                names.len(),
                summarize(&names)
            );
            eprintln!(
                "note: replace each \"?\" under [dependencies.{name}.symbols] with the \
                 prototype, e.g.\n    \
                 [dependencies.{name}.symbols.{}]\n    \
                 args = [\"int\", \"int\"]\n    \
                 ret  = \"int\"\n  \
                 A shared library says what it exports and nothing more — C keeps no argument \
                 or return\n  types in a compiled artifact — so no header was found to read \
                 them from. If you have\n  one, this generates the whole table:\n    \
                 jade pkg bind {name} --header <its header.h>",
                names[0]
            );
            return;
        }

        // Nothing readable at all: a URL dependency has no artifact yet, and a
        // library that exports nothing bindable has no names to offer either.
        println!("added {name} to jade.toml");
        eprintln!(
            "note: {name} is a C library and no header for it was found, so it has no symbols \
             yet.\n  A shared library carries no headers — only symbol names, and C does not \
             mangle them — so\n  one has to be pointed at:\n    \
             jade pkg add {name} --path <the .so> --header <its header.h>\n  \
             Or write a [dependencies.{name}.symbols] table by hand, which can cover what the\n  \
             generator will not guess at."
        );
        return;
    }

    let manifest = load_or_exit(&root);
    try_relock_and_install(&root, &manifest)
        .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));
    println!("added {name}");
}

/// A readable list of names, cut off before it fills the terminal.
fn summarize(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    let mut s = names.iter().take(SHOWN).copied().collect::<Vec<_>>().join(", ");
    if names.len() > SHOWN {
        s.push_str(&format!(", and {} more", names.len() - SHOWN));
    }
    s
}

/// Which ABI an artifact speaks, read from the artifact itself.
///
/// A Jade package exports `jade_pkg_init`; a plain C library does not. That is
/// not a heuristic — it is the same symbol the loader requires at run time, so
/// anything answering "Jade" here is exactly what `use` will later accept.
/// Both kinds are a `.dylib`, so the file extension says nothing and only the
/// symbol table can tell them apart.
///
/// `None` when the table cannot be read, which is a reason to fall back on what
/// the user said rather than to guess.
fn detect_abi(lib: &std::path::Path) -> Option<Abi> {
    let syms = pkg::bindgen::exported_symbols(lib)?;
    Some(if syms.contains("jade_pkg_init") { Abi::Jade } else { Abi::C })
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
    lib: Option<&std::path::Path>,
) -> Result<(), String> {
    let header_path = std::path::Path::new(header);
    if !header_path.exists() {
        return Err(format!("no such header: {header}"));
    }

    let binding = pkg::bindgen::from_header(header_path, include, only)?;

    // Check the header against the library it is supposed to describe. A header
    // declaring symbols the library does not export is the wrong header, and
    // the shim would fail to link with an undefined-symbol error naming none of
    // this.
    if let Some(exported) = lib.and_then(pkg::bindgen::exported_symbols) {
        let (covered, total) = pkg::bindgen::coverage(&binding, &exported);
        if covered == 0 && !binding.symbols.is_empty() {
            return Err(format!(
                "{header} declares none of the {total} symbols {} exports — it looks like the \
                 wrong header for this library.",
                lib.map(|l| l.display().to_string()).unwrap_or_default()
            ));
        }
        if !quiet {
            println!("covers {covered} of the {total} symbols the library exports");
        }
    }

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

    // The dependency's own artifact, so the header can be checked against the
    // library it claims to describe before anything is written.
    let lib = load_or_exit(&root)
        .dependencies
        .as_ref()
        .and_then(|d| d.get(name))
        .and_then(|e| e.path.clone())
        .map(|p| root.join(p))
        .filter(|p| p.exists());

    bind_header(&root, name, header, include, only, false, lib.as_deref())
        .unwrap_or_else(|e| fail(e));
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

        let lib = entry.path.as_ref().map(|p| root.join(p)).filter(|p| p.exists());
        let Some(path) = found else {
            eprintln!(
                "note: dependency '{name}' names header '{}' but it was not found, so no symbols \
                 were generated. Point at it with\n  jade pkg bind {name} --header <path>",
                headers[0]
            );
            continue;
        };

        println!("binding {name} from {}", headers[0]);
        match bind_header(root, name, &path.to_string_lossy(), &dirs, None, false, lib.as_deref()) {
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
