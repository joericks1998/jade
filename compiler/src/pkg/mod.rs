//! The package manager: `[dependencies]` → `jade.lock` → `libs/`.
//!
//! A dependency is one of two kinds. A *source package* is Jade source, from a
//! local path, a git repository or a `.tar.gz` archive, compiled into each program
//! that uses it (`source.rs`). A *Rust package* is a crate that marks what Jade may
//! call with `#[jade::export]`; cargo fetches and builds it with the program
//! (`crate::rustpkg`), so this module only hands its name to import resolution.
//! There is deliberately **no package registry** of Jade's own — like Go, a source
//! dependency names where it lives rather than an entry in a central index.
//!
//! **There is no version solving.** Solving searches a space of candidate versions
//! to satisfy a set of ranges, which needs ranges and a registry to enumerate; Jade
//! has neither, and [`crate::project`] rejects a range outright. What there is
//! instead is a choice between the two versions already named: the higher one wins,
//! which is Go's rule and the only one available when there is no third version to
//! go and fetch. See [`compare_versions`].
//!
//! `jade.lock` stays a flat list of one source package per name, and once written
//! it is authoritative — [`ensure_ready`] reads it rather than resolving again, so
//! the choice above happens when a dependency is *added* and never at build or run
//! time. A Rust package is not in it: cargo pins those, in the generated crate's
//! `Cargo.lock`.
//!
//! The integration surface with the rest of the compiler is one function,
//! [`resolved_libraries`]: dependencies are handed back as synthetic
//! [`crate::project::LibraryEntry`] values and unioned into the manifest's `[lib]`
//! map, so the import resolver ([`crate::aot::imports`]) never learns what a
//! dependency is.
//!
//! Until 2.0.4 a dependency could also be a prebuilt shared library: a Jade-ABI
//! package, or a C library behind a shim generated from its header. Both are gone;
//! `design/v2-rust-packages.md` says why.

pub mod fetch;
pub mod lock;
pub mod manifest;
pub mod source;

use std::path::{Path, PathBuf};

use crate::project::{Abi, LibraryEntry, ProjectManifest};
use fetch::Fetcher;
use lock::{LockedArtifact, LockedPackage, Lockfile};

/// Project-local directory holding installed source packages. Committed to
/// `.gitignore`, not to the repository — `jade.lock` is what travels.
pub const LIBS_DIR: &str = "libs";

/// Artifact key for a source package: source is the same on every platform.
/// Kept out of the platform namespace so it can never collide with a real tag.
pub const ANY_PLATFORM: &str = "any";

/// Version recorded for a local `path` dependency that declares none. The
/// version is only ever a directory component and a label here — there is no
/// registry to resolve it against.
pub const LOCAL_VERSION: &str = "local";

/// Order two versions, or `None` when they cannot be ordered.
///
/// Dotted numbers only: `1.2.0` against `1.10.0` compares component-wise, so
/// `10` is greater than `2` rather than lexically smaller. A component that is
/// not a plain number makes the pair incomparable — `2.0-beta` against `2.0`,
/// or the `local` a path dependency carries.
///
/// Refusing to order what it cannot is the point. This is what decides which of
/// two versions a program will actually build, and inventing an order for a
/// spelling nobody defined would pick a winner on a coin toss. An incomparable
/// pair falls back to the conflict error, which names both and asks.
pub fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parse =
        |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse::<u64>().ok()).collect() };
    let (x, y) = (parse(a)?, parse(b)?);
    // `1.2` and `1.2.0` are the same version written two ways, so compare over
    // the longer of the two with the shorter zero-extended.
    let n = x.len().max(y.len());
    for i in 0..n {
        let (l, r) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if l != r {
            return Some(l.cmp(&r));
        }
    }
    Some(std::cmp::Ordering::Equal)
}

/// Purpose: This function says whether a dependency belongs in `jade.lock`.
///
/// Inputs:
/// - `entry`: its manifest entry.
///
/// Outputs: true for a source package; a Rust package is cargo's to pin.
fn is_locked(entry: &crate::project::DependencyEntry) -> bool {
    entry.abi != Some(Abi::Rust)
}

// ── Resolution ────────────────────────────────────────────────────────────────

/// Resolve `[dependencies]` into a [`Lockfile`], reading each source package once
/// to record the hash of its source.
///
/// Every entry is validated, a Rust package included, so a malformed one is
/// reported by `jade pkg install` rather than at the next build; only source
/// packages are locked.
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
        if is_locked(entry) {
            out.packages.push(source::resolve(root, name, entry, fetcher)?);
        }
    }

    Ok(out)
}

// ── Materialization ───────────────────────────────────────────────────────────

/// Ensure `libs/` matches `lock`.
///
/// Verification runs on **every** call, not only after a download: a package in
/// `libs/` is compiled into the program, so a tree that is merely *present* is
/// not thereby the one the lock pins.
pub fn materialize(root: &Path, lock: &Lockfile, fetcher: &dyn Fetcher) -> Result<(), String> {
    let platform = fetch::platform_tag();
    for pkg in &lock.packages {
        let (_, artifact) = select_artifact(pkg, platform)?;
        if !is_source(pkg) {
            return Err(format!(
                "jade.lock records '{}' as abi = \"{}\", a prebuilt library, which jade 2.0.4 \
                 removed; run `jade pkg install` to lock the manifest again",
                pkg.name, pkg.abi
            ));
        }
        source::materialize(root, pkg, artifact, fetcher)?;
    }
    Ok(())
}

/// The artifact for `platform`, falling back to [`ANY_PLATFORM`].
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
    Err(format!(
        "dependency '{}' has nothing in jade.lock to install here; jade.lock lists: {}",
        pkg.name,
        available.join(", ")
    ))
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
// A `path` dependency points at a directory the user edits. Every other kind of
// source package is immutable at its source — a URL either serves the bytes the
// lock pins or it does not, and a git commit is a commit — so for those the pin
// written at `jade pkg add` stays true forever. A local path is the one source
// that legitimately changes under a lock that is otherwise still correct.

/// Whether a locked package came from a `path` dependency.
fn is_local(pkg: &LockedPackage) -> bool {
    pkg.source.starts_with(lock::PATH_SOURCE)
}

/// Whether a locked package is Jade source.
fn is_source(pkg: &LockedPackage) -> bool {
    pkg.abi == Abi::Source.as_str()
}

/// The current hash of a local dependency's source tree, or `None` when it cannot
/// be read. That is not treated as an error anywhere: a source that has moved away
/// leaves the existing pin standing.
fn local_source_digest(root: &Path, pkg: &LockedPackage) -> Option<String> {
    let src = local_source_path(root, pkg).ok()?;
    source::tree_hash(&src).ok()
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
/// the silent-wrong-build outcome that mode exists to prevent.
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
                 jade.lock, or put the source back to match.",
                pkg.name, artifact.sha256, digest
            ));
        }
    }
    Ok(())
}

// ── Manifest / lock agreement ─────────────────────────────────────────────────

/// Check `jade.lock` still describes the source packages in `[dependencies]`.
///
/// Reports every discrepancy at once rather than the first, so editing a
/// manifest by hand does not turn into a sequence of one-error runs. A Rust
/// package is not locked, so it is in neither list.
pub fn verify_in_sync(manifest: &ProjectManifest, lock: &Lockfile) -> Result<(), String> {
    let empty = Default::default();
    let deps = manifest.dependencies.as_ref().unwrap_or(&empty);

    let mut missing: Vec<&str> = deps
        .iter()
        .filter(|(n, e)| is_locked(e) && lock.get(n).is_none())
        .map(|(n, _)| n.as_str())
        .collect();
    let mut stale: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| !deps.get(&p.name).is_some_and(is_locked))
        .map(|p| p.name.as_str())
        .collect();
    // The two can also name the same dependency and disagree about what it is.
    // Which of the two is right is not for this function to decide — they simply
    // must agree.
    let mut disagreed: Vec<String> = deps
        .iter()
        .filter_map(|(name, entry)| {
            let locked = lock.get(name)?;
            let kind = entry.kind().as_str();
            (locked.abi != kind)
                .then(|| format!("{name} (jade.toml says {kind}, jade.lock says {})", locked.abi))
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
        msg.push_str(&format!("\n  locked as a different kind: {}", disagreed.join(", ")));
    }
    msg.push_str("\nRun `jade pkg install` to update the lock.");
    Err(msg)
}

// ── Integration with import resolution ────────────────────────────────────────

/// Present locked source packages as `[lib]` entries, each pointing at its
/// directory and entry module, so `use geometry` resolves through exactly the
/// same code path as a hand-written `[lib.geometry]`.
pub fn dependency_libraries(
    root: &Path,
    lock: &Lockfile,
) -> std::collections::HashMap<String, LibraryEntry> {
    let platform = fetch::platform_tag();
    let mut out = std::collections::HashMap::new();
    for pkg in lock.packages.iter().filter(|p| is_source(p)) {
        if let Ok((_, artifact)) = select_artifact(pkg, platform) {
            out.insert(pkg.name.clone(), source::library(root, pkg, artifact));
        }
    }
    out
}

/// Make a project's source packages usable: check the lock agrees with the
/// manifest, then install what it pins.
///
/// Called before a build, so `jade run` in a fresh clone fetches what `jade.lock`
/// pins rather than failing with an unhelpful import error — the same implicit
/// fetch as `cargo run`. A project with no source packages does no work and
/// touches no network; its Rust packages are fetched by cargo when the program is
/// built.
pub fn ensure_ready(root: &Path, manifest: &ProjectManifest) -> Result<(), String> {
    let deps = manifest.dependencies.as_ref();
    for (name, entry) in deps.into_iter().flatten() {
        entry.validate(name)?;
    }
    let has_locked = deps.is_some_and(|d| d.values().any(is_locked));
    if !has_locked {
        return Ok(());
    }

    let mut lock = lock::read(root)?.ok_or_else(|| {
        "jade.toml declares source packages under [dependencies] but there is no jade.lock — \
         run `jade pkg install` to resolve and pin them"
            .to_string()
    })?;

    verify_in_sync(manifest, &lock)?;

    // Pick up an edited local dependency before installing, so `jade run` uses
    // the package as it is now rather than as it was when it was added.
    let changed = refresh_local(root, &mut lock);
    if !changed.is_empty() {
        eprintln!("note: re-pinned {} (local source changed)", changed.join(", "));
        // A read-only checkout is not a reason to refuse to run: the refreshed
        // digest lives in memory and the correct tree is still used. The lock is
        // just left for the next writable run to update.
        if let Err(e) = lock::write(root, &lock) {
            eprintln!("warning: could not update jade.lock ({e})");
        }
    }

    materialize(root, &lock, &fetch::HttpFetcher::new())
}

/// Every library visible to imports: the manifest's `[lib]` entries, the locked
/// source packages, and the Rust packages.
///
/// This is what the CLI and the AOT resolver hand to import resolution in place
/// of a bare `manifest.lib`. A missing or unreadable `jade.lock` degrades to the
/// manifest's own entries rather than failing — this runs on the import path, and
/// the actionable diagnostics belong to `jade pkg install`.
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
        Ok(Some(l)) => dependency_libraries(root, &l),
        Ok(None) => std::collections::HashMap::new(),
        Err(e) => {
            eprintln!("warning: {e}");
            std::collections::HashMap::new()
        }
    };

    // A Rust package is a library whose functions the build reads out of the
    // compiled crate, so its entry names the dependency rather than a directory.
    for name in crate::rustpkg::dependencies(manifest).into_keys() {
        out.insert(
            name.clone(),
            LibraryEntry { path: String::new(), rust: Some(name), ..Default::default() },
        );
    }

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
