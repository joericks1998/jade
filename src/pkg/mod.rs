//! The package manager: `[dependencies]` → `jade.lock` → `libs/`.
//!
//! Dependencies are prebuilt native shared libraries, sourced from a local path
//! or a URL. There is deliberately **no package registry** — like Go, a
//! dependency names where it lives rather than an entry in a central index.
//!
//! That choice has a consequence worth stating plainly: a `.so` carries no
//! manifest of its own, so **there is no transitive resolution and no version
//! solving**. Each dependency contributes exactly one artifact, `jade.lock` is a
//! flat list, and "resolution" means picking the right platform build. A
//! package that needs another package must say so in its documentation; Jade
//! cannot discover it.
//!
//! The integration surface with the rest of the compiler is one function,
//! [`dependency_libraries`]: resolved dependencies are handed back as synthetic
//! [`crate::project::LibraryEntry`] values and unioned into the manifest's
//! `[lib]` map. Neither the VM ([`crate::vm`]) nor the AOT import
//! resolver ([`crate::aot::imports`]) learns what a dependency is — they
//! keep resolving `[lib]` entries exactly as before, so the two backends cannot
//! drift.

pub mod bindgen;
pub mod cshim;
pub mod fetch;
pub mod lock;
pub mod manifest;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::project::{DependencyEntry, LibraryEntry, ProjectManifest};
use fetch::Fetcher;
use lock::{LockedArtifact, LockedPackage, Lockfile};

/// Project-local directory holding materialized dependency artifacts. Committed
/// to `.gitignore`, not to the repository — `jade.lock` is what travels.
pub const LIBS_DIR: &str = "libs";

/// Artifact key for a dependency whose URL carries no `{platform}` placeholder:
/// one build serves every platform. Kept out of the platform namespace so it
/// can never collide with a real tag.
pub const ANY_PLATFORM: &str = "any";

/// Version recorded for a local `path` dependency that declares none. The
/// version is only ever a directory component and a label here — there is no
/// registry to resolve it against.
pub const LOCAL_VERSION: &str = "local";

// ── Resolution ────────────────────────────────────────────────────────────────

/// Resolve `[dependencies]` into a [`Lockfile`], fetching each artifact once to
/// record its digest.
///
/// A `{platform}` URL is expanded across [`fetch::SUPPORTED_PLATFORMS`] and
/// every variant that exists is recorded, so a lock generated on one machine
/// still describes the others. A platform whose artifact is missing is skipped
/// rather than fatal — plenty of packages ship for a subset — but a dependency
/// that resolves to *nothing* is an error, since it could never be installed.
pub fn resolve(
    root: &Path,
    manifest: &ProjectManifest,
    fetcher: &dyn Fetcher,
) -> Result<Lockfile, String> {
    let mut out = Lockfile::new();
    let Some(deps) = &manifest.dependencies else {
        return Ok(out);
    };

    // Sorted so resolution order — and therefore any error the user sees — does
    // not depend on HashMap iteration order.
    let mut names: Vec<&String> = deps.keys().collect();
    names.sort();

    for name in names {
        let entry = &deps[name];
        entry.validate(name)?;
        out.packages.push(resolve_one(root, name, entry, fetcher)?);
    }

    Ok(out)
}

fn resolve_one(
    root: &Path,
    name: &str,
    entry: &DependencyEntry,
    fetcher: &dyn Fetcher,
) -> Result<LockedPackage, String> {
    let version = entry.version.clone().unwrap_or_else(|| LOCAL_VERSION.to_string());
    let abi = entry.abi.as_str().to_string();

    let (source, artifacts) = if let Some(path) = &entry.path {
        (lock::source_path(path), resolve_local(root, name, path, &abi)?)
    } else {
        let url = entry.url.as_deref().expect("validate() guarantees a source");
        (lock::source_url(url), resolve_remote(name, url, entry, &abi, fetcher)?)
    };

    Ok(LockedPackage { name: name.to_string(), version, source, abi, artifacts })
}

/// Hash a local artifact in place. It is copied into `libs/` by
/// [`materialize`], not here — resolution never writes.
fn resolve_local(
    root: &Path,
    name: &str,
    rel: &str,
    abi: &str,
) -> Result<BTreeMap<String, LockedArtifact>, String> {
    let src = root.join(rel);
    let bytes = std::fs::read(&src)
        .map_err(|e| format!("dependency '{name}': cannot read {} ({e})", src.display()))?;

    let file = artifact_filename(name, rel, abi);

    // A local artifact is one build of one thing; nothing about it is
    // platform-general, but neither is it tied to a tag we could verify. Record
    // it as `any` and let the user own the portability question.
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ANY_PLATFORM.to_string(),
        LockedArtifact { url: None, file, sha256: fetch::sha256_hex(&bytes) },
    );
    Ok(artifacts)
}

fn resolve_remote(
    name: &str,
    url: &str,
    entry: &DependencyEntry,
    abi: &str,
    fetcher: &dyn Fetcher,
) -> Result<BTreeMap<String, LockedArtifact>, String> {
    let mut artifacts = BTreeMap::new();

    if !entry.is_platform_template() {
        let bytes = fetcher.fetch(url).map_err(|e| format!("dependency '{name}': {e}"))?;
        artifacts.insert(
            ANY_PLATFORM.to_string(),
            LockedArtifact {
                url: Some(url.to_string()),
                file: artifact_filename(name, &filename_from_url(name, url)?, abi),
                sha256: fetch::sha256_hex(&bytes),
            },
        );
        return Ok(artifacts);
    }

    let mut failures = Vec::new();
    for platform in fetch::SUPPORTED_PLATFORMS {
        let expanded = fetch::expand_platform(url, platform);
        match fetcher.fetch(&expanded) {
            Ok(bytes) => {
                artifacts.insert(
                    (*platform).to_string(),
                    LockedArtifact {
                        url: Some(expanded.clone()),
                        file: artifact_filename(name, &filename_from_url(name, &expanded)?, abi),
                        sha256: fetch::sha256_hex(&bytes),
                    },
                );
            }
            // Not every package ships for every platform. Record what exists.
            Err(e) => failures.push(format!("  {platform}: {e}")),
        }
    }

    if artifacts.is_empty() {
        return Err(format!(
            "dependency '{name}': no artifact could be fetched for any supported platform\n{}",
            failures.join("\n")
        ));
    }

    Ok(artifacts)
}

/// The filename an artifact is installed as: the dependency's own name, keeping
/// the source's extension.
///
/// Imports resolve by *stem* — `use fastmath` looks for `fastmath.<ext>` — so an
/// artifact left under its upstream name (`libfastmath.dylib`, or a
/// platform-tagged `tok-linux-x86_64.so`) would be unreachable under the name it
/// was added as. Renaming on install makes the import predictable and keeps
/// `libs/` readable.
fn artifact_filename(name: &str, source_file: &str, abi: &str) -> String {
    // A C dependency's importable module is the generated shim, which takes the
    // plain `<name>.<ext>` slot. The raw library steps aside so the two can live
    // in one directory without colliding.
    let stem = if abi == crate::project::Abi::C.as_str() {
        format!("{name}_native")
    } else {
        name.to_string()
    };
    match Path::new(source_file).extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem,
    }
}

/// Last path segment of a URL, used to recover the artifact's extension.
fn filename_from_url(name: &str, url: &str) -> Result<String, String> {
    let trimmed = url.split(['?', '#']).next().unwrap_or(url);
    trimmed
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("dependency '{name}': cannot derive a filename from url '{url}'"))
}

// ── Materialization ───────────────────────────────────────────────────────────

/// Ensure `libs/` matches `lock` for the current platform.
///
/// Verification runs on **every** call, not only after a download: a `.so` in
/// `libs/` is `dlopen`ed, which is arbitrary code execution before any Jade code
/// runs, so an artifact that is merely *present* is not thereby trustworthy.
pub fn materialize(root: &Path, lock: &Lockfile, fetcher: &dyn Fetcher) -> Result<(), String> {
    let platform = fetch::platform_tag();

    for pkg in &lock.packages {
        let (_, artifact) = select_artifact(pkg, platform)?;
        let dir = root.join(LIBS_DIR).join(pkg.install_dir());
        let dest = dir.join(&artifact.file);

        // Present and matching → nothing to do. Present but wrong — a corrupted
        // download, a partial write, a swapped file — falls through to a
        // re-fetch, which is the only safe response.
        if std::fs::read(&dest).is_ok_and(|b| fetch::sha256_hex(&b) == artifact.sha256) {
            continue;
        }

        let bytes = match &artifact.url {
            Some(url) => {
                fetcher.fetch(url).map_err(|e| format!("dependency '{}': {e}", pkg.name))?
            }
            None => {
                let src = local_source_path(root, pkg)?;
                std::fs::read(&src).map_err(|e| {
                    format!("dependency '{}': cannot read {} ({e})", pkg.name, src.display())
                })?
            }
        };

        // A dependency is something the dynamic loader can open, and this is the
        // one point every source passes through with the bytes in hand. Checking
        // here rather than only in `jade pkg add` covers a hand-written manifest,
        // a URL serving the wrong file, and an `install` on a fresh clone —
        // none of which go through `add` at all. Without it the first complaint
        // comes from `dlopen`, in a finished program, having built cleanly.
        if !bindgen::bytes_are_loadable_object(&bytes) {
            return Err(format!(
                "dependency '{}': {} is not a shared library\n  \
                 It does not start with a Mach-O or ELF header, so nothing could load it. \
                 A dependency is a prebuilt .dylib or .so, not source and not a header.",
                pkg.name, artifact.file
            ));
        }

        let actual = fetch::sha256_hex(&bytes);
        if actual != artifact.sha256 {
            return Err(format!(
                "dependency '{}': checksum mismatch for {}\n  expected {}\n  actual   {}\n\
                 The artifact does not match jade.lock — it may have been replaced upstream. \
                 Re-run `jade pkg update {}` if the change is expected.",
                pkg.name, artifact.file, artifact.sha256, actual, pkg.name
            ));
        }

        write_artifact(&dir, &dest, &bytes)
            .map_err(|e| format!("dependency '{}': {e}", pkg.name))?;
    }

    Ok(())
}

/// The artifact for `platform`, falling back to [`ANY_PLATFORM`].
///
/// Errors name the platforms that *are* available, since "this package has no
/// build for your machine" is otherwise a dead end for the user.
fn select_artifact<'a>(
    pkg: &'a LockedPackage,
    platform: Option<&str>,
) -> Result<(String, &'a LockedArtifact), String> {
    if let Some((tag, a)) = platform.and_then(|t| pkg.artifacts.get(t).map(|a| (t, a))) {
        return Ok((tag.to_string(), a));
    }
    if let Some(a) = pkg.artifacts.get(ANY_PLATFORM) {
        return Ok((ANY_PLATFORM.to_string(), a));
    }

    let available: Vec<&str> = pkg.artifacts.keys().map(String::as_str).collect();
    Err(match platform {
        Some(tag) => format!(
            "dependency '{}' has no artifact for this platform ({tag}); \
             jade.lock lists: {}",
            pkg.name,
            available.join(", ")
        ),
        None => format!(
            "dependency '{}' cannot be installed: this platform ({}/{}) is not supported by jade",
            pkg.name,
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    })
}

/// Recover the original local path from a `path+…` lock source.
fn local_source_path(root: &Path, pkg: &LockedPackage) -> Result<PathBuf, String> {
    pkg.source.strip_prefix(lock::PATH_SOURCE).map(|rel| root.join(rel)).ok_or_else(|| {
        format!(
            "dependency '{}': lock entry has no url and source '{}' is not a local path",
            pkg.name, pkg.source
        )
    })
}

// ── Local sources ─────────────────────────────────────────────────────────────
//
// A `path` dependency points at a file the user builds, and rebuilds. Every
// other kind of dependency is immutable at its source — a URL either serves the
// bytes the lock pins or it does not — so for those the pin written at `jade pkg
// add` stays true forever. A local path is the one source that legitimately
// changes under a lock that is otherwise still correct, and nothing below
// `resolve` ever re-reads it. That is what let a rebuilt library keep running as
// the copy it was on the day it was added.

/// Whether a locked package came from a `path` dependency.
fn is_local(pkg: &LockedPackage) -> bool {
    pkg.source.starts_with(lock::PATH_SOURCE)
}

/// Current SHA-256 of a local dependency's source file.
///
/// `None` when the file cannot be read. That is not treated as an error
/// anywhere: a source that has moved away or been deleted leaves the existing
/// pin standing, which keeps a project whose `libs/` is already populated
/// working exactly as it did before.
fn local_source_digest(root: &Path, pkg: &LockedPackage) -> Option<String> {
    let src = local_source_path(root, pkg).ok()?;
    std::fs::read(&src).ok().map(|b| fetch::sha256_hex(&b))
}

/// Re-pin every local `path` dependency against its source on disk, returning
/// the names whose digest moved.
///
/// The caller writes the lock back. Splitting it that way keeps the decision
/// about *whether the lock may change* with the command: `jade pkg install`
/// rewrites it, `--locked` refuses to (see [`verify_local_unchanged`]).
pub fn refresh_local(root: &Path, lock: &mut Lockfile) -> Vec<String> {
    let mut changed = Vec::new();

    for pkg in &mut lock.packages {
        if !is_local(pkg) {
            continue;
        }
        let Some(digest) = local_source_digest(root, pkg) else {
            continue;
        };

        // Only the artifacts actually copied from the local file. A path
        // dependency carries no other kind, but keying off `url` rather than
        // assuming a single entry keeps this honest if that ever changes.
        let mut moved = false;
        for artifact in pkg.artifacts.values_mut() {
            if artifact.url.is_none() && artifact.sha256 != digest {
                artifact.sha256 = digest.clone();
                moved = true;
            }
        }

        if moved {
            changed.push(pkg.name.clone());
        }
    }

    changed
}

/// Whether a local dependency's source has changed since it was pinned.
///
/// The read-only question behind [`refresh_local`], for reporting. Anything not
/// local, and any source that cannot be read, answers `false` — there is no
/// drift to report without a file to compare against.
pub fn local_drift(root: &Path, pkg: &LockedPackage) -> bool {
    if !is_local(pkg) {
        return false;
    }
    let Some(digest) = local_source_digest(root, pkg) else {
        return false;
    };
    pkg.artifacts.values().any(|a| a.url.is_none() && a.sha256 != digest)
}

/// Fail if a local `path` dependency no longer matches its pin.
///
/// The `--locked` half of [`refresh_local`]. In CI a moved source means the
/// committed lock is stale, and installing the old digest anyway is precisely
/// the silent-wrong-binary outcome that mode exists to prevent.
pub fn verify_local_unchanged(root: &Path, lock: &Lockfile) -> Result<(), String> {
    for pkg in &lock.packages {
        if !is_local(pkg) {
            continue;
        }
        let Some(digest) = local_source_digest(root, pkg) else {
            continue;
        };
        let Some(artifact) = pkg.artifacts.values().find(|a| a.url.is_none()) else {
            continue;
        };
        if artifact.sha256 != digest {
            return Err(format!(
                "dependency '{}': the local source has changed since jade.lock was written\n  \
                 locked {}\n  on disk {}\n\
                 --locked forbids rewriting the lock. Run `jade pkg install` and commit \
                 jade.lock, or rebuild the source to match.",
                pkg.name, artifact.sha256, digest
            ));
        }
    }
    Ok(())
}

/// Write via a temp file and rename, so an interrupted install never leaves a
/// half-written `.so` that would pass an existence check and fail a `dlopen`.
fn write_artifact(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {} ({e})", dir.display()))?;

    let tmp = dir.join(format!(".jade-install-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("cannot write {} ({e})", tmp.display()))?;

    // Shared libraries are loaded, not executed directly, but some loaders and
    // plenty of tooling still expect the execute bit.
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot set permissions on {} ({e})", tmp.display()));
    }

    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot install {} ({e})", dest.display())
    })
}

/// Copy this project's installed dependencies into a `libs/` beside an artifact.
///
/// Returns the install directories written, for the message the CLI prints.
///
/// **Everything installed, not only what the artifact names.** A package can
/// load a dependency of its own, and the artifact that uses that package never
/// mentions it — the host cannot see through a package to what it will reach
/// for. Copying only the direct ones produces a bundle that works until the
/// first nested load, which is the failure this exists to prevent. The cost is
/// carrying a dependency the program may not use; the alternative is shipping
/// one that breaks.
///
/// **Whole directories, never single files.** A C dependency's install dir holds
/// two artifacts — the generated shim and the library it wraps — and the shim
/// finds the second through `@loader_path` on macOS and `$ORIGIN` on Linux (see
/// [`compile_shim`]). Copying the shim alone leaves that reference pointing at
/// nothing, and the failure lands as a dyld error on someone else's machine with
/// no mention of bundling.
///
/// **One shared `libs/`, not one per artifact.** That is a requirement rather
/// than a layout preference. `dlopen` keys a loaded image by the path it was
/// asked for, so a per-artifact bundle would give two packages that share a
/// dependency two copies of it — two sets of globals, two initializers. For a
/// library that owns a device that is two devices.
pub fn bundle_beside_artifact(artifact: &Path, libs_root: &Path) -> Result<Vec<String>, String> {
    let dest_root = artifact.parent().unwrap_or_else(|| Path::new(".")).join(LIBS_DIR);
    let mut written: Vec<String> = Vec::new();

    let entries = std::fs::read_dir(libs_root)
        .map_err(|e| format!("cannot read {} ({e})", libs_root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {} ({e})", libs_root.display()))?;
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let src_dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let dest_dir = dest_root.join(&name);

        // Building in place — `jade build --lib` at the project root — already
        // has the dependency exactly where it belongs. Copying a directory onto
        // itself would truncate every file in it.
        if same_dir(&src_dir, &dest_dir) {
            continue;
        }

        for entry in std::fs::read_dir(&src_dir)
            .map_err(|e| format!("cannot read {} ({e})", src_dir.display()))?
        {
            let entry = entry.map_err(|e| format!("cannot read {} ({e})", src_dir.display()))?;
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let from = entry.path();
            let to = dest_dir.join(entry.file_name());
            let bytes = std::fs::read(&from)
                .map_err(|e| format!("cannot read {} ({e})", from.display()))?;

            // Two artifacts built into one directory share the bundle, which is
            // the point. Two *different* builds of one dependency landing there
            // is not — the second would silently replace the first for both.
            if let Ok(existing) = std::fs::read(&to)
                && crate::pkg::fetch::sha256_hex(&existing) != crate::pkg::fetch::sha256_hex(&bytes)
            {
                return Err(format!(
                    "cannot bundle '{name}': {} already holds a different build of it.\n  \
                     Two artifacts in one directory share one libs/, so they must agree on \
                     every dependency. Build them into separate directories, or install one \
                     version of it.",
                    dest_dir.display()
                ));
            }
            write_artifact(&dest_dir, &to, &bytes)?;
        }
        written.push(name);
    }

    written.sort();
    written.dedup();
    Ok(written)
}

/// Whether two directories are the same place, following symlinks.
///
/// A plain path comparison would miss a symlinked `libs/`, and copying a
/// directory onto itself truncates every file in it.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ── C binding shims ───────────────────────────────────────────────────────────

/// Filename of the generated shim for a C dependency, using the host's native
/// shared-library extension — it is compiled here, not downloaded.
pub fn shim_filename(name: &str) -> String {
    // Named after the dependency, because this is the file `use <name>` must
    // resolve to — imports match by stem.
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    format!("{name}.{ext}")
}

/// The file that actually gets `dlopen`ed for a package.
///
/// For a Jade-ABI package that is the artifact itself; for a C library it is the
/// generated shim, since the raw library exports no `jade_pkg_init`.
fn module_file(pkg: &LockedPackage, artifact: &LockedArtifact) -> String {
    if pkg.abi == crate::project::Abi::C.as_str() {
        shim_filename(&pkg.name)
    } else {
        artifact.file.clone()
    }
}

/// The error for a dependency still carrying `"?"` prototypes, or `None`.
///
/// One message, used from both the places that can reach a placeholder, so the
/// answer reads the same whether it came from `jade run` or `jade check`.
///
/// It names the symbols because the fix is per-symbol, and it shows the shape
/// to write because "fill in the prototype" is not an instruction anyone can
/// act on without an example. Long tables are cut off — a two hundred symbol
/// library would otherwise bury the two lines that say what to do.
pub fn unresolved_report(name: &str, entry: &crate::project::DependencyEntry) -> Option<String> {
    const SHOWN: usize = 8;

    let missing = entry.unresolved_symbols();
    if missing.is_empty() {
        return None;
    }

    let mut listed = missing.iter().take(SHOWN).copied().collect::<Vec<_>>().join(", ");
    if missing.len() > SHOWN {
        listed.push_str(&format!(", and {} more", missing.len() - SHOWN));
    }
    let example = missing[0];

    Some(format!(
        "dependency '{name}' has {n} symbol{s} with no signature yet: {listed}\n  \
         A shared library says what it exports and nothing more — C keeps no argument or return \
         types in\n  a compiled artifact — so these went into jade.toml as \"?\" for you to fill \
         in, e.g.\n    \
         [dependencies.{name}.symbols.{example}]\n    \
         args = [\"int\", \"int\"]\n    \
         ret  = \"int\"\n  \
         Or point at the library's header and let them be generated:\n    \
         jade pkg bind {name} --header <its header.h>",
        n = missing.len(),
        s = if missing.len() == 1 { "" } else { "s" },
    ))
}

/// The same check across the whole manifest, for callers with no lock in hand.
///
/// `jade check` claims to be an honest predictor of whether `jade run` will
/// accept a file, and a placeholder is something `jade run` refuses. Checking
/// costs a manifest read that has already happened, so the claim holds without
/// `check` doing any of the installing it deliberately avoids.
pub fn check_symbols_resolved(manifest: &ProjectManifest) -> Result<(), String> {
    let Some(deps) = &manifest.dependencies else { return Ok(()) };
    for (name, entry) in deps {
        if let Some(e) = unresolved_report(name, entry) {
            return Err(e);
        }
    }
    Ok(())
}

/// Generate and compile a binding shim for every `abi = "c"` dependency.
///
/// Runs after [`materialize`], against the manifest rather than the lock: the
/// symbol table is user-declared configuration, not a resolution result.
pub fn build_c_shims(
    root: &Path,
    lock: &Lockfile,
    manifest: &ProjectManifest,
) -> Result<(), String> {
    let Some(deps) = &manifest.dependencies else {
        return Ok(());
    };

    for pkg in &lock.packages {
        if pkg.abi != crate::project::Abi::C.as_str() {
            continue;
        }
        let Some(entry) = deps.get(&pkg.name) else { continue };
        // A C library with no symbol table has no binding, and a plain C library
        // is exactly what the loader cannot take. Skipping used to install it
        // raw, report success, and leave the program to fail at run time with
        // "missing jade_pkg_init" — which names a symbol rather than the fact
        // that the dependency was never bound.
        let Some(symbols) = &entry.symbols else {
            return Err(format!(
                "dependency '{0}' is a C library with no symbols, so no binding was generated.\n  \
                 Jade cannot load a plain C library directly — it needs a table of the functions \
                 to bind, which is read from the library's header:\n    \
                 jade pkg add {0} --path <the .dylib> --header <its header.h>\n  \
                 Or write a [dependencies.{0}.symbols] table by hand.",
                pkg.name
            ));
        };

        // A `"?"` is a prototype nobody has written yet, and it cannot be
        // guessed on the way past — see `project::UNRESOLVED`. Refusing here
        // means the answer arrives at `jade run` naming the manifest, rather
        // than as `cc` failing on `?` as a type name.
        if let Some(e) = unresolved_report(&pkg.name, entry) {
            return Err(e);
        }

        let (_, artifact) = select_artifact(pkg, fetch::platform_tag())?;
        let dir = root.join(LIBS_DIR).join(pkg.install_dir());
        let shim_c = dir.join(format!("{}_shim.c", pkg.name));
        let shim_out = dir.join(shim_filename(&pkg.name));

        let empty_structs = std::collections::HashMap::new();
        let structs = entry.structs.as_ref().unwrap_or(&empty_structs);
        let headers = entry.headers.as_deref().unwrap_or(&[]);
        let source = cshim::generate(&pkg.name, symbols, structs, headers)?;

        // Skip the compile when nothing changed — reinstalls are common and cc
        // is not cheap. "Nothing changed" means both the declared symbols and
        // the library being bound: a rebuilt artifact may no longer export what
        // the existing shim was linked against, so an out-of-date shim has to be
        // relinked even though its source is identical.
        let unchanged = std::fs::read_to_string(&shim_c).is_ok_and(|s| s == source);
        if unchanged && is_newer(&shim_out, &dir.join(&artifact.file)) {
            continue;
        }

        std::fs::write(&shim_c, &source).map_err(|e| {
            format!("dependency '{}': cannot write {} ({e})", pkg.name, shim_c.display())
        })?;

        compile_shim(
            &pkg.name,
            &shim_c,
            &shim_out,
            &dir,
            &artifact.file,
            entry.include_dirs.as_deref().unwrap_or(&[]),
        )?;
    }

    Ok(())
}

/// Whether `out` is at least as new as `input`.
///
/// Answers `false` when either timestamp is unreadable — including when `out`
/// does not exist at all — so every uncertain case falls through to a rebuild
/// rather than to a stale artifact.
fn is_newer(out: &Path, input: &Path) -> bool {
    let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (stamp(out), stamp(input)) {
        (Some(o), Some(i)) => o >= i,
        _ => false,
    }
}

/// Link the shim against the target library.
fn compile_shim(
    name: &str,
    shim_c: &Path,
    out: &Path,
    dir: &Path,
    target_file: &str,
    include_dirs: &[String],
) -> Result<(), String> {
    let mut cc = std::process::Command::new("cc");
    if cfg!(target_os = "macos") {
        cc.arg("-dynamiclib");
    } else {
        cc.arg("-shared");
    }
    // A header that is not on the default search path — Homebrew's, most often.
    for inc in include_dirs {
        cc.arg(format!("-I{inc}"));
    }
    // C lets a function be called with no declaration in scope, assuming it
    // returns `int` and taking the arguments at face value. For this shim that
    // is never right and never survivable: a call that really returns a pointer
    // comes back truncated to 32 bits, and the crash lands several calls later
    // with nothing pointing at the cause. It means the manifest names a symbol
    // whose header is missing from `headers`, so the error says that.
    cc.arg("-Werror=implicit-function-declaration")
        .arg("-fPIC")
        .arg(shim_c)
        .arg("-o")
        .arg(out)
        // Link the target directly by path: it has no `lib<name>` naming
        // convention to rely on after being renamed on install.
        .arg(dir.join(target_file));

    // Both loaders must find the target next to the shim at runtime, since
    // `libs/` is not on any search path. On Linux $ORIGIN in an rpath does
    // that; macOS needs a post-link fixup (below) because the target's baked-in
    // install name is what gets recorded.
    #[cfg(target_os = "linux")]
    cc.arg("-Wl,-rpath,$ORIGIN");

    let result = cc
        .output()
        .map_err(|e| format!("dependency '{name}': cannot run cc ({e}) — a C compiler is required to bind a C library"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        // An undefined symbol here means the manifest declares something the
        // target library does not export. Say so, rather than surfacing raw
        // linker output the user has to decode.
        let hint = if stderr.contains("implicit-function-declaration") {
            format!(
                "\n  A symbol in the manifest is declared by no header the shim includes, so C \
                 would have\n  guessed its prototype. Add the header that declares it:\n    \
                 jade pkg bind {name} --header <that header.h>"
            )
        } else if stderr.contains("undefined") {
            format!(
                "\n  A declared symbol is missing from the library. Check the \
                 [dependencies.{name}.symbols] names against `nm -gU` on the artifact."
            )
        } else {
            String::new()
        };
        return Err(format!(
            "dependency '{name}': could not build the C binding shim{hint}\n{}",
            stderr.trim()
        ));
    }

    #[cfg(target_os = "macos")]
    retarget_macos_load_path(name, out, dir, target_file)?;

    Ok(())
}

/// Point the shim's dependency at the target sitting beside it.
///
/// macOS records a dylib's own **install name** (`LC_ID_DYLIB`) in whatever
/// links against it — here that is whatever the package author built with, e.g.
/// `libplainc.dylib`. `libs/` is on no search path, so dyld fails at load with
/// "Library not loaded". Rewriting the shim's reference to `@loader_path/...`
/// makes it resolve relative to the shim itself.
///
/// The fixup is applied to the **shim**, never the artifact: the artifact's
/// SHA-256 is pinned in `jade.lock`, and editing it in place would make the
/// next install see a checksum mismatch and re-download.
#[cfg(target_os = "macos")]
fn retarget_macos_load_path(
    name: &str,
    shim: &Path,
    dir: &Path,
    target_file: &str,
) -> Result<(), String> {
    // `otool -D` prints the target's install name — exactly the string the
    // linker recorded, so the -change below matches precisely.
    let out = std::process::Command::new("otool")
        .arg("-D")
        .arg(dir.join(target_file))
        .output()
        .map_err(|e| format!("dependency '{name}': cannot run otool ({e})"))?;

    let text = String::from_utf8_lossy(&out.stdout);
    let Some(install_name) = text.lines().nth(1).map(str::trim).filter(|s| !s.is_empty()) else {
        // No install name recorded (a bundle, or an object built without one) —
        // nothing was written into the shim to rewrite.
        return Ok(());
    };

    let status = std::process::Command::new("install_name_tool")
        .arg("-change")
        .arg(install_name)
        .arg(format!("@loader_path/{target_file}"))
        .arg(shim)
        .output()
        .map_err(|e| format!("dependency '{name}': cannot run install_name_tool ({e})"))?;

    if !status.status.success() {
        return Err(format!(
            "dependency '{name}': could not point the binding shim at its library\n{}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }

    Ok(())
}

// ── Manifest / lock agreement ─────────────────────────────────────────────────

/// Check `jade.lock` still describes `[dependencies]`.
///
/// Reports every discrepancy at once rather than the first, so editing a
/// manifest by hand does not turn into a sequence of one-error runs.
pub fn verify_in_sync(manifest: &ProjectManifest, lock: &Lockfile) -> Result<(), String> {
    let empty = Default::default();
    let deps = manifest.dependencies.as_ref().unwrap_or(&empty);

    let mut missing: Vec<&str> =
        deps.keys().filter(|n| lock.get(n).is_none()).map(String::as_str).collect();
    let mut stale: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| !deps.contains_key(&p.name))
        .map(|p| p.name.as_str())
        .collect();
    // The two can also name the same dependency and disagree about what it is.
    // Comparing only names let a lock saying `abi = "jade"` outlive a manifest
    // corrected to `abi = "c"`: the build read the lock, skipped the shim, and
    // loaded a plain C library as though it were a Jade package. Which of the
    // two is right is not for this function to decide — they simply must agree.
    let mut disagreed: Vec<String> = deps
        .iter()
        .filter_map(|(name, entry)| {
            let locked = lock.get(name)?;
            (locked.abi != entry.abi.as_str()).then(|| {
                format!(
                    "{name} (jade.toml says {}, jade.lock says {})",
                    entry.abi.as_str(),
                    locked.abi
                )
            })
        })
        .collect();
    missing.sort();
    stale.sort();
    disagreed.sort();

    if missing.is_empty() && stale.is_empty() && disagreed.is_empty() {
        return Ok(());
    }

    let mut msg = String::from("jade.lock is out of sync with jade.toml");
    if !missing.is_empty() {
        msg.push_str(&format!("\n  in jade.toml but not locked: {}", missing.join(", ")));
    }
    if !stale.is_empty() {
        msg.push_str(&format!("\n  locked but not in jade.toml: {}", stale.join(", ")));
    }
    if !disagreed.is_empty() {
        msg.push_str(&format!("\n  locked with a different ABI: {}", disagreed.join(", ")));
    }
    msg.push_str("\nRun `jade pkg install` to update the lock.");
    Err(msg)
}

// ── Integration with import resolution ────────────────────────────────────────

/// Present locked dependencies as `[lib]` entries.
///
/// This is the whole integration surface. Each dependency becomes a library
/// whose directory is its `libs/` install dir and whose single importable file
/// is this platform's artifact — so `use fastmath` resolves through exactly the
/// same code path as a hand-written `[lib.fastmath]`, in both backends.
///
/// A dependency with no artifact for this platform is skipped rather than
/// failing here: this runs on the import path, where the useful error is
/// "unknown module", and the actionable one comes from [`materialize`].
pub fn dependency_libraries(lock: &Lockfile) -> std::collections::HashMap<String, LibraryEntry> {
    let platform = fetch::platform_tag();
    let mut out = std::collections::HashMap::new();

    for pkg in &lock.packages {
        if let Ok((_, artifact)) = select_artifact(pkg, platform) {
            out.insert(
                pkg.name.clone(),
                LibraryEntry {
                    path: format!("{LIBS_DIR}/{}", pkg.install_dir()),
                    files: Some(vec![module_file(pkg, artifact)]),
                },
            );
        }
    }

    out
}

/// Make a project's dependencies usable: check the lock agrees with the
/// manifest, then materialize this platform's artifacts.
///
/// Called before the VM starts, so `jade run` in a fresh clone fetches what
/// `jade.lock` pins rather than failing with an unhelpful import error — the
/// same implicit-fetch behavior as `cargo run`. A project with no dependencies
/// does no work and touches no network.
pub fn ensure_ready(root: &Path, manifest: &ProjectManifest) -> Result<(), String> {
    let has_deps = manifest.dependencies.as_ref().is_some_and(|d| !d.is_empty());
    if !has_deps {
        return Ok(());
    }

    // Before the lock, because an unfilled prototype is not something
    // installing can fix. Sending the user to `jade pkg install` first would
    // cost them a step to arrive at this same message.
    check_symbols_resolved(manifest)?;

    let mut lock = lock::read(root)?.ok_or_else(|| {
        "jade.toml declares [dependencies] but there is no jade.lock — \
         run `jade pkg install` to resolve and pin them"
            .to_string()
    })?;

    verify_in_sync(manifest, &lock)?;

    // Pick up a rebuilt local dependency before installing, so `jade run` uses
    // the library as it is now rather than as it was when it was added.
    let changed = refresh_local(root, &mut lock);
    if !changed.is_empty() {
        eprintln!("note: re-pinned {} (local source changed)", changed.join(", "));
        // A read-only checkout is not a reason to refuse to run: the refreshed
        // digest lives in memory and the correct bytes still get installed. The
        // lock is just left for the next writable run to update.
        if let Err(e) = lock::write(root, &lock) {
            eprintln!("warning: could not update jade.lock ({e})");
        }
    }

    materialize(root, &lock, &fetch::HttpFetcher::new())?;
    build_c_shims(root, &lock, manifest)
}

/// Every library visible to imports: the manifest's `[lib]` entries plus the
/// locked dependencies.
///
/// This is what the CLI and the AOT resolver hand to import resolution in place
/// of a bare `manifest.lib`. A missing or unreadable `jade.lock` degrades to
/// just the manifest's entries rather than failing — this runs on the import
/// path, and the actionable diagnostics belong to `jade pkg install`.
///
/// **A manifest `[lib]` wins over a dependency of the same name**, so a project
/// can always shadow something it depends on. The shadow is reported, because
/// silently ignoring a declared dependency is the kind of thing that costs an
/// afternoon.
pub fn resolved_libraries(
    root: &Path,
    manifest: &ProjectManifest,
) -> std::collections::HashMap<String, LibraryEntry> {
    let mut out = match lock::read(root) {
        Ok(Some(l)) => dependency_libraries(&l),
        Ok(None) => std::collections::HashMap::new(),
        Err(e) => {
            eprintln!("warning: {e}");
            std::collections::HashMap::new()
        }
    };

    if let Some(libs) = &manifest.lib {
        for (name, entry) in libs {
            if out.insert(name.clone(), entry.clone()).is_some() {
                eprintln!(
                    "warning: [lib.{name}] in jade.toml shadows the dependency of the same \
                     name; the local library is being used"
                );
            }
        }
    }

    out
}

#[cfg(test)]
mod tests;
