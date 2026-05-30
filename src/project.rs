use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

// ── Manifest types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectManifest {
    pub project: Option<ProjectSection>,
    pub scripts: Option<HashMap<String, String>>,
    /// `[native]` section: maps package name → entry with `path` and required `alias`.
    /// Used via `use "native/<name>"` in Jade source; the package binds as `alias`.
    pub native: Option<HashMap<String, NativePackageEntry>>,
    /// `[lib.<name>]` sections: register a directory and its `.jde` modules as a
    /// named library so they can be imported cross-directory via
    /// `use "<name>/<module>"`, anchored at the project root. See
    /// [`resolve_library_import`].
    pub lib: Option<HashMap<String, LibraryEntry>>,
}

/// Entry in the `[native]` section of `jade.toml`.
/// Both fields are required — the `alias` is the name the package is bound to in Jade.
#[derive(Debug, Clone, Deserialize)]
pub struct NativePackageEntry {
    pub path: String,
    pub alias: String,
}

/// Entry in a `[lib.<name>]` section of `jade.toml`.
///
/// ```toml
/// [lib.utils]
/// path  = "src/utils"         # directory, relative to the project root
/// files = ["math", "strings"] # importable module stems (no .jde extension)
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryEntry {
    pub path: String,
    pub files: Vec<String>,
}

/// Resolve a `use` path against registered `[lib]` libraries, anchored at `root`.
///
/// A path is a *library reference* when it has the form `<lib>/<module>` and
/// `<lib>` names a registered library. The module must appear in the library's
/// `files` allowlist, and resolution is anchored at the project `root` (not the
/// importing file) — this is what enables cross-directory imports.
///
/// Returns:
///   * `Ok(Some(path))` — a registered library file (a trailing `.jde` in the
///     import is optional and is appended here),
///   * `Ok(None)` — not a library reference; the caller falls back to normal
///     relative-path resolution (hybrid mode),
///   * `Err(msg)` — the library exists but the module is not in its `files` list.
pub fn resolve_library_import(
    libs: &HashMap<String, LibraryEntry>,
    import_path: &str,
    root: &Path,
) -> Result<Option<PathBuf>, String> {
    let Some((lib_name, rest)) = import_path.split_once('/') else {
        return Ok(None);
    };
    let Some(entry) = libs.get(lib_name) else {
        return Ok(None);
    };
    let module = rest.strip_suffix(".jde").unwrap_or(rest);
    if !entry.files.iter().any(|f| f == module) {
        return Err(format!(
            "module '{module}' is not registered in [lib.{lib_name}] of jade.toml \
             (registered files: {:?})",
            entry.files
        ));
    }
    let base = if Path::new(&entry.path).is_absolute() {
        PathBuf::from(&entry.path)
    } else {
        root.join(&entry.path)
    };
    Ok(Some(base.join(format!("{module}.jde"))))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectSection {
    pub name: String,
    pub version: Option<String>,
    pub entry: Option<String>,
}

impl ProjectManifest {
    /// Returns true if this toml file actually declares a Jade project.
    pub fn is_project(&self) -> bool {
        self.project.is_some()
    }

    /// The entry-point file for this project (default: `main.jde`).
    pub fn entry_file(&self) -> &str {
        self.project
            .as_ref()
            .and_then(|p| p.entry.as_deref())
            .unwrap_or("main.jde")
    }
}

// ── Root discovery ────────────────────────────────────────────────────────────

/// Walk up from the current working directory searching for a `jade.toml` that
/// contains a `[project]` section.  Returns the directory containing that file.
pub fn find_project_root() -> Option<PathBuf> {
    find_project_root_from(&std::env::current_dir().ok()?)
}

/// Like [`find_project_root`] but starts from `start` instead of the current
/// working directory. Used by the AOT build daemon, which resolves a project
/// relative to the source file it was handed rather than its own CWD.
pub fn find_project_root_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("jade.toml");
        if candidate.exists() {
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                if let Ok(manifest) = toml::from_str::<ProjectManifest>(&content) {
                    if manifest.is_project() {
                        return Some(dir);
                    }
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load the project manifest from the given root directory.
pub fn load_project(root: &Path) -> Result<ProjectManifest, String> {
    let path = root.join("jade.toml");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    toml::from_str::<ProjectManifest>(&content)
        .map_err(|e| format!("invalid jade.toml: {}", e))
}

// ── Test file discovery ───────────────────────────────────────────────────────

/// Recursively discover test files under `root`.
///
/// Convention: files named `test_*.jde` or `*_test.jde`.
/// An optional `pattern` string further filters by stem substring.
pub fn find_test_files(root: &Path, pattern: Option<&str>) -> Vec<PathBuf> {
    let mut results = Vec::new();
    collect_test_files(root, pattern, &mut results);
    results.sort();
    results
}

fn collect_test_files(dir: &Path, pattern: Option<&str>, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip hidden dirs, build output, and common non-source dirs.
            if name.starts_with('.') || name == "target" || name == "docs" {
                continue;
            }
            collect_test_files(&path, pattern, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jde") {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let is_test = stem.starts_with("test_") || stem.ends_with("_test");
            if is_test {
                if let Some(pat) = pattern {
                    if !stem.contains(pat) {
                        continue;
                    }
                }
                out.push(path);
            }
        }
    }
}
