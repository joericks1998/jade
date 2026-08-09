//! `jade.lock` — the pinned, reproducible resolution of `[dependencies]`.
//!
//! The manifest says *what* a project depends on; this file says *exactly* what
//! gets installed, down to a digest per platform. `jade.lock` is meant to be
//! committed; the `libs/` directory it describes is not.
//!
//! ## Why every platform is recorded
//!
//! Dependencies are prebuilt shared libraries, so — unlike Cargo, which locks
//! portable source — a lock naming one artifact would only be valid on the
//! machine that generated it. A macOS developer would commit a lock that Linux
//! CI could not install, and worse, could not *verify*: with no package
//! registry there is nothing to ask for the Linux artifact's digest at install
//! time. So the lock records a [`LockedArtifact`] for every platform the
//! dependency publishes, and installation picks the one line matching the host
//! (or, later, the build target).
//!
//! Only the matching artifact is ever downloaded. The others cost a few hundred
//! bytes of text each and are what make a committed lock portable across a team.
//! This mirrors a Homebrew formula's `bottle` block, which lists a `sha256` per
//! platform and installs exactly one.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Format version of `jade.lock`. Bump when the shape below changes
/// incompatibly; [`read`] refuses anything it does not recognise rather than
/// silently misreading a future lock.
pub const LOCK_VERSION: u32 = 1;

/// Filename, relative to the project root.
pub const LOCK_FILE: &str = "jade.lock";

/// A parsed `jade.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    /// Serialized as `[[package]]` tables. Kept sorted by name on write so the
    /// file is byte-stable across runs and produces clean diffs.
    #[serde(default, rename = "package")]
    pub packages: Vec<LockedPackage>,
}

/// One resolved dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    /// Exact version; also the `libs/<name>-<version>/` directory component.
    pub version: String,
    /// Where it came from: `path+<relative path>` or `url+<url>`, the latter
    /// still holding `{platform}` if the dependency is a template. Tagged
    /// rather than bare so a future source kind is additive.
    pub source: String,
    /// `"jade"` or `"c"` — see [`crate::project::Abi`].
    pub abi: String,
    /// Platform tag (`darwin-aarch64`, `linux-x86_64`, …) → artifact.
    ///
    /// A `BTreeMap` so iteration order is deterministic without an explicit
    /// sort, which is what keeps [`write`] byte-stable. A local `path`
    /// dependency has exactly one entry, under the platform it was added on.
    #[serde(default)]
    pub artifacts: BTreeMap<String, LockedArtifact>,
}

/// A single platform's build of a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedArtifact {
    /// Absent for local `path` dependencies, which are copied rather than
    /// downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Filename inside `libs/<name>-<version>/`.
    pub file: String,
    /// Lowercase hex SHA-256 of the artifact. Verified on every install, not
    /// only on first download — a `.so` is `dlopen`ed, which is arbitrary code
    /// execution before any Jade code runs, so a corrupted or swapped file in
    /// `libs/` must be caught rather than trusted because it is already present.
    pub sha256: String,
}

impl Lockfile {
    /// An empty lock at the current format version.
    pub fn new() -> Self {
        Lockfile { version: LOCK_VERSION, packages: Vec::new() }
    }

    /// The locked package named `name`, if present.
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile::new()
    }
}

impl LockedPackage {
    /// The artifact for `platform`, if this package publishes one.
    pub fn artifact(&self, platform: &str) -> Option<&LockedArtifact> {
        self.artifacts.get(platform)
    }

    /// Directory holding this package's artifact, relative to the project root.
    pub fn install_dir(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// Prefix marking a lock `source` as a local file rather than a URL.
pub const PATH_SOURCE: &str = "path+";

/// Tag a local path as a lock `source`.
pub fn source_path(path: &str) -> String {
    format!("{PATH_SOURCE}{path}")
}

/// Tag a URL as a lock `source`.
pub fn source_url(url: &str) -> String {
    format!("url+{url}")
}

/// Absolute path to a project's lockfile.
pub fn path(root: &Path) -> PathBuf {
    root.join(LOCK_FILE)
}

/// Read `<root>/jade.lock`.
///
/// Returns `Ok(None)` when the file does not exist — a project with no
/// dependencies never needs one, and `jade pkg install` writes it on first use. A
/// malformed or future-versioned lock is an error rather than a silent reset,
/// since discarding a lock would defeat the point of having one.
pub fn read(root: &Path) -> Result<Option<Lockfile>, String> {
    let file = path(root);
    let content = match std::fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };

    let lock: Lockfile =
        toml::from_str(&content).map_err(|e| format!("invalid {}: {e}", file.display()))?;

    if lock.version != LOCK_VERSION {
        return Err(format!(
            "{} has lock format version {} but this jade understands version {} \
             — delete it and re-run `jade pkg install` to regenerate",
            file.display(),
            lock.version,
            LOCK_VERSION
        ));
    }

    Ok(Some(lock))
}

/// Write `<root>/jade.lock`.
///
/// Packages are sorted by name first, so two runs over the same resolution
/// produce byte-identical output and the file diffs cleanly.
pub fn write(root: &Path, lock: &Lockfile) -> Result<(), String> {
    let mut sorted = lock.clone();
    sorted.packages.sort_by(|a, b| a.name.cmp(&b.name));

    let content =
        toml::to_string_pretty(&sorted).map_err(|e| format!("cannot serialize lockfile: {e}"))?;

    let file = path(root);
    std::fs::write(&file, content).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

#[cfg(test)]
mod tests;
