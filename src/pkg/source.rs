//! Source packages: Jade dependencies compiled into each program that uses them.
//!
//! Design decision 11 in `design/v2-static-values.md` makes a Jade package a
//! source dependency. A prebuilt package carried its own runtime and could not
//! hold a generic function, since the checker copies a function per set of
//! argument types and a library cannot know a caller's types. Source has
//! neither problem: the package's files join the program's import graph and are
//! checked and compiled with it.
//!
//! A source package is a directory with a `jade.toml` holding a `[package]`
//! section. It comes from one of three places:
//!
//! ```toml
//! [dependencies]
//! geometry = { path = "../geometry", abi = "source" }
//! shapes   = { git = "https://github.com/me/shapes", tag = "v1.2.0", abi = "source" }
//! vectors  = { url = "https://example.com/vectors-1.0.tar.gz", version = "1.0", abi = "source" }
//! ```
//!
//! A path is used where it is, the way a path dependency always was. A git
//! repository or an archive is unpacked into `libs/<name>-<version>/`.
//!
//! *What the lock pins* is a hash of the package's source: its `jade.toml` and
//! every `.jde` file, by path and content. The same hash covers all three
//! sources, so an install checks the unpacked tree rather than the bytes it came
//! in, and a git dependency's lock also records the commit it was. Files other
//! than source do not change what a program compiles to, so they are left out,
//! and a README edit does not move a pin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::fetch::Fetcher;
use super::lock::{self, LockedArtifact, LockedPackage};
use super::{ANY_PLATFORM, LIBS_DIR, LOCAL_VERSION};
use crate::project::{Abi, DependencyEntry, LibraryEntry, PackageSection};

/// The lock `source` prefix for a git repository: `git+<url>#<commit>`.
pub const GIT_SOURCE: &str = "git+";

/// Purpose: This function makes a git dependency's lock `source`.
///
/// Inputs:
/// - `url`: the repository.
/// - `commit`: the full commit id it resolved to.
///
/// Outputs: `git+<url>#<commit>`.
pub fn source_git(url: &str, commit: &str) -> String {
    format!("{GIT_SOURCE}{url}#{commit}")
}

/// Purpose: This function splits a git lock `source` into its repository and
/// commit.
///
/// Inputs:
/// - `source`: the lock's `source` string.
///
/// Outputs: the repository and commit, or `None` for another kind of source.
pub fn split_git(source: &str) -> Option<(&str, &str)> {
    source.strip_prefix(GIT_SOURCE)?.rsplit_once('#')
}

/// Purpose: This function says whether a `url` names an archive of source.
///
/// Inputs:
/// - `url`: the dependency's url.
///
/// Outputs: whether it ends in `.tar.gz` or `.tgz`.
pub fn is_archive_url(url: &str) -> bool {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    bare.ends_with(".tar.gz") || bare.ends_with(".tgz")
}

/// Purpose: This function says whether a directory holds a source package.
///
/// Inputs:
/// - `dir`: the directory.
///
/// Outputs: whether its `jade.toml` has a `[package]` section.
pub fn is_package_dir(dir: &Path) -> bool {
    dir.is_dir() && package_section(dir).is_ok()
}

/// Purpose: This function reads a source package's `[package]` section.
///
/// Inputs:
/// - `dir`: the package's directory.
///
/// Outputs: the checked section, or a message naming what is missing.
pub fn package_section(dir: &Path) -> Result<PackageSection, String> {
    let manifest = crate::project::load_project(dir)
        .map_err(|e| format!("{} is not a Jade source package: {e}", dir.display()))?;
    let section = manifest.package.ok_or_else(|| {
        format!(
            "{} has a jade.toml with no [package] section, so it is a project rather than a \
             package; add [package] with a name to share it",
            dir.display()
        )
    })?;
    section.validate()?;
    let entry = dir.join(section.entry_file());
    if !entry.is_file() {
        return Err(format!(
            "package '{}' names its entry module {}, which does not exist",
            section.name,
            entry.display()
        ));
    }
    Ok(section)
}

/// Purpose: This function hashes a source package's files.
///
/// Inputs:
/// - `dir`: the package's directory.
///
/// Outputs: the lowercase hex SHA-256 over `jade.toml` and every `.jde` file,
/// each as its path relative to `dir` and its bytes, in path order; or a message
/// when the tree cannot be read.
pub fn tree_hash(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_source(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for rel in &files {
        let bytes = std::fs::read(dir.join(rel))
            .map_err(|e| format!("cannot read {} ({e})", dir.join(rel).display()))?;
        h.update(rel.as_bytes());
        h.update([0]);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Purpose: This function lists a package's source files below one directory.
///
/// Inputs:
/// - `root`: the package's directory, which paths are written relative to.
/// - `dir`: the directory being listed.
/// - `out`: where the relative paths go, with `/` between parts.
///
/// Outputs: `Ok(())`, or a message when a directory cannot be read.
fn collect_source(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {} ({e})", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {} ({e})", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // `.git` and other hidden directories, and a package's own installed
        // dependencies, are not its source.
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(|e| format!("{} ({e})", path.display()))?;
        if kind.is_dir() {
            if dir == root && name == LIBS_DIR {
                continue;
            }
            collect_source(root, &path, out)?;
        } else if kind.is_file() && (name.ends_with(".jde") || (dir == root && name == "jade.toml"))
        {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Purpose: This function resolves a source dependency into its lock entry.
///
/// Inputs:
/// - `root`: the project root.
/// - `name`: the dependency's name.
/// - `entry`: its `jade.toml` entry.
/// - `fetcher`: how an archive is downloaded.
///
/// Outputs: the locked package, or a message naming the dependency.
pub fn resolve(
    root: &Path,
    name: &str,
    entry: &DependencyEntry,
    fetcher: &dyn Fetcher,
) -> Result<LockedPackage, String> {
    let fail = |e: String| format!("dependency '{name}': {e}");
    let (source, dir, version, _scratch) = if let Some(rel) = &entry.path {
        let dir = root.join(rel);
        let version = entry.version.clone().unwrap_or_else(|| LOCAL_VERSION.to_string());
        (lock::source_path(rel), dir, version, None)
    } else if let Some(url) = &entry.git {
        let reference =
            entry.rev.as_deref().or(entry.tag.as_deref()).or(entry.branch.as_deref()).unwrap_or("");
        let scratch = Scratch::new(root, name).map_err(fail)?;
        let commit = git_checkout(url, reference, scratch.path()).map_err(fail)?;
        let version = entry
            .version
            .clone()
            .or_else(|| entry.tag.clone())
            .unwrap_or_else(|| commit.chars().take(12).collect());
        let dir = scratch.path().to_path_buf();
        (source_git(url, &commit), dir, version, Some(scratch))
    } else {
        let url = entry.url.as_deref().expect("validate() guarantees a source");
        let scratch = Scratch::new(root, name).map_err(fail)?;
        let bytes = fetcher.fetch(url).map_err(fail)?;
        let dir = unpack(&bytes, scratch.path()).map_err(fail)?;
        let version = entry.version.clone().unwrap_or_else(|| LOCAL_VERSION.to_string());
        (lock::source_url(url), dir, version, Some(scratch))
    };

    let section = package_section(&dir).map_err(fail)?;
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ANY_PLATFORM.to_string(),
        LockedArtifact {
            url: entry.url.clone(),
            file: section.entry_file(),
            sha256: tree_hash(&dir).map_err(fail)?,
        },
    );
    Ok(LockedPackage {
        name: name.to_string(),
        version,
        source,
        abi: Abi::Source.as_str().to_string(),
        artifacts,
    })
}

/// Purpose: This function makes a locked source package present in `libs/`,
/// checked against its hash.
///
/// Inputs:
/// - `root`: the project root.
/// - `pkg`: the locked package.
/// - `artifact`: its one artifact, which carries the hash.
/// - `fetcher`: how an archive is downloaded.
///
/// Outputs: `Ok(())`, or a message naming the dependency.
pub fn materialize(
    root: &Path,
    pkg: &LockedPackage,
    artifact: &LockedArtifact,
    fetcher: &dyn Fetcher,
) -> Result<(), String> {
    let fail = |e: String| format!("dependency '{}': {e}", pkg.name);
    // A path package is used where it is; there is nothing to install.
    if let Some(rel) = pkg.source.strip_prefix(lock::PATH_SOURCE) {
        let dir = root.join(rel);
        package_section(&dir).map_err(fail)?;
        return Ok(());
    }

    let dest = root.join(LIBS_DIR).join(pkg.install_dir());
    if dest.is_dir() && tree_hash(&dest).is_ok_and(|h| h == artifact.sha256) {
        return Ok(());
    }
    if dest.exists() {
        eprintln!(
            "note: libs/{} did not match jade.lock and is being reinstalled (dependency '{}')",
            pkg.install_dir(),
            pkg.name
        );
    }

    let scratch = Scratch::new(root, &pkg.name).map_err(fail)?;
    let dir = if let Some((url, commit)) = split_git(&pkg.source) {
        git_checkout(url, commit, scratch.path()).map_err(fail)?;
        let _ = std::fs::remove_dir_all(scratch.path().join(".git"));
        scratch.path().to_path_buf()
    } else {
        let url = artifact.url.as_deref().ok_or_else(|| {
            fail(format!("jade.lock gives source '{}' and no url to fetch", pkg.source))
        })?;
        let bytes = fetcher.fetch(url).map_err(fail)?;
        unpack(&bytes, scratch.path()).map_err(fail)?
    };

    let actual = tree_hash(&dir).map_err(fail)?;
    if actual != artifact.sha256 {
        return Err(fail(format!(
            "the source does not match jade.lock\n  expected {}\n  actual   {}\n\
             It may have changed upstream. Re-run `jade pkg update {}` if the change is expected.",
            artifact.sha256, actual, pkg.name
        )));
    }

    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .map_err(|e| fail(format!("cannot replace {} ({e})", dest.display())))?;
    }
    std::fs::rename(&dir, &dest)
        .map_err(|e| fail(format!("cannot install into {} ({e})", dest.display())))?;
    Ok(())
}

/// Purpose: This function gives a locked source package's `[lib]` entry, the
/// form import resolution reads.
///
/// Inputs:
/// - `root`: the project root.
/// - `pkg`: the locked package.
/// - `artifact`: its artifact, which names the entry module.
///
/// Outputs: the entry: the package's directory, its entry module and its
/// exports.
pub fn library(root: &Path, pkg: &LockedPackage, artifact: &LockedArtifact) -> LibraryEntry {
    let path = match pkg.source.strip_prefix(lock::PATH_SOURCE) {
        Some(rel) => rel.to_string(),
        None => format!("{LIBS_DIR}/{}", pkg.install_dir()),
    };
    let exports = package_section(&root.join(&path)).ok().and_then(|s| s.exports);
    LibraryEntry { path, entry: Some(artifact.file.clone()), exports, ..Default::default() }
}

/// Purpose: This function gives the directory a locked source package's files
/// are in.
///
/// Inputs:
/// - `root`: the project root.
/// - `pkg`: the locked package.
///
/// Outputs: the directory: the path itself for a path package, else its
/// install directory under `libs/`.
pub fn package_dir(root: &Path, pkg: &LockedPackage) -> PathBuf {
    match pkg.source.strip_prefix(lock::PATH_SOURCE) {
        Some(rel) => root.join(rel),
        None => root.join(LIBS_DIR).join(pkg.install_dir()),
    }
}

/// Purpose: This function lists the dependencies a source package declares,
/// rewritten to make sense from the project adding it.
///
/// Inputs:
/// - `pkg_dir`: the package's directory.
/// - `root`: the project adding it.
/// - `local`: whether the package itself is a path dependency, so a path it
///   names can be followed from here.
///
/// Outputs: each dependency's name and entry, and the names of path
/// dependencies that cannot be followed.
pub fn declared_dependencies(
    pkg_dir: &Path,
    root: &Path,
    local: bool,
) -> (Vec<(String, DependencyEntry)>, Vec<String>) {
    let Ok(manifest) = crate::project::load_project(pkg_dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut out = Vec::new();
    let mut unfollowed = Vec::new();
    let mut names: Vec<_> = manifest.dependencies.unwrap_or_default().into_iter().collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, mut entry) in names {
        if let Some(rel) = entry.path.clone() {
            // A path is relative to the package. It means something here only
            // when the package is on this machine too.
            match local.then(|| relative_to(&pkg_dir.join(&rel), root)).flatten() {
                Some(rebased) => entry.path = Some(rebased),
                None => {
                    unfollowed.push(name);
                    continue;
                }
            }
        }
        out.push((name, entry));
    }
    (out, unfollowed)
}

/// Purpose: This function writes a path relative to a directory, climbing with
/// `..` where it has to.
///
/// Inputs:
/// - `path`: the path.
/// - `base`: the directory it should be relative to.
///
/// Outputs: the relative path with `/` between parts, or `None` when either
/// cannot be made absolute.
fn relative_to(path: &Path, base: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let base = base.canonicalize().ok()?;
    let p: Vec<_> = path.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".to_string(); b.len() - common];
    parts.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    Some(if parts.is_empty() { ".".to_string() } else { parts.join("/") })
}

/// Purpose: This function checks out one commit of a git repository.
///
/// Inputs:
/// - `url`: the repository.
/// - `reference`: a commit id, tag or branch.
/// - `into`: an empty directory for the checkout.
///
/// Outputs: the full id of the commit checked out, or git's message.
fn git_checkout(url: &str, reference: &str, into: &Path) -> Result<String, String> {
    let run = |args: &[&str], dir: Option<&Path>| -> Result<String, String> {
        let mut cmd = Command::new("git");
        if let Some(d) = dir {
            cmd.arg("-C").arg(d);
        }
        let out = cmd
            .args(args)
            .output()
            .map_err(|e| format!("cannot run git ({e}); a git dependency needs git installed"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let target = into.to_string_lossy().into_owned();
    run(&["clone", "--quiet", "--no-checkout", url, &target], None)?;
    run(&["checkout", "--quiet", "--detach", reference], Some(into))
        .map_err(|e| format!("{e}\n  '{reference}' names no commit, tag or branch of {url}"))?;
    run(&["rev-parse", "HEAD"], Some(into))
}

/// Purpose: This function unpacks a `.tar.gz` archive of a source package.
///
/// Inputs:
/// - `bytes`: the archive.
/// - `into`: an empty directory to unpack into.
///
/// Outputs: the package's directory: `into` itself, or the one directory the
/// archive holds when it wraps everything in one, as GitHub's archives do.
fn unpack(bytes: &[u8], into: &Path) -> Result<PathBuf, String> {
    let archive = into.with_extension("tar.gz");
    std::fs::write(&archive, bytes)
        .map_err(|e| format!("cannot write {} ({e})", archive.display()))?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|e| format!("cannot run tar ({e})"));
    let _ = std::fs::remove_file(&archive);
    let out = out?;
    if !out.status.success() {
        return Err(format!(
            "the archive could not be unpacked: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if into.join("jade.toml").is_file() {
        return Ok(into.to_path_buf());
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(into)
        .map_err(|e| format!("cannot read {} ({e})", into.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    match entries.as_slice() {
        [only] if only.is_dir() => Ok(only.clone()),
        _ => Err("the archive holds no jade.toml at its top".to_string()),
    }
}

/// Purpose: This struct holds a scratch directory under `libs/`, removed when
/// it is dropped.
///
/// It sits under `libs/` so that installing is a rename within one file system.
struct Scratch(PathBuf);

impl Scratch {
    /// Purpose: This function makes an empty scratch directory.
    ///
    /// Inputs:
    /// - `root`: the project root.
    /// - `name`: the dependency, for the directory's name.
    ///
    /// Outputs: the scratch directory, or a message.
    fn new(root: &Path, name: &str) -> Result<Self, String> {
        let dir = root.join(LIBS_DIR).join(format!(".partial-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {} ({e})", dir.display()))?;
        Ok(Scratch(dir))
    }

    /// Purpose: This function gives the scratch directory's path.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the path.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("tar.gz"));
    }
}
