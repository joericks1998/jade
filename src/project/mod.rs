use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[cfg(test)]
mod tests;

// ── Manifest types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectManifest {
    pub project: Option<ProjectSection>,
    pub scripts: Option<HashMap<String, String>>,
    /// `[lib.<name>]` sections: register a directory and its modules as a named
    /// library so they can be imported cross-directory via `use <name>.<module>`,
    /// anchored at the project root. A module is a Jade source file (`.jde`) or a
    /// native C-ABI shared library (`.dylib` / `.so` / `.dll`) — the file
    /// extension decides. See [`resolve_library_import`].
    pub lib: Option<HashMap<String, LibraryEntry>>,
}

/// Entry in a `[lib.<name>]` section of `jade.toml`.
///
/// ```toml
/// [lib.utils]
/// path  = "src/utils"                  # directory, relative to the project root
/// files = ["math.jde", "fast.dylib"]   # optional allowlist of importable filenames
/// ```
///
/// `files` is an allowlist of importable module **filenames, with extension**.
/// The extension both disambiguates modules from other files in the directory
/// and selects how each is loaded: `.jde` is a Jade source module, while
/// `.dylib` / `.so` / `.dll` is a native shared library (loaded over the
/// `jade_pkg_init` C ABI). Omit `files` to make every recognized file in `path`
/// importable.
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryEntry {
    pub path: String,
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

/// How a resolved `[lib]` module is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// A Jade source module (`.jde`).
    Jade,
    /// A native C-ABI shared library (`.dylib` / `.so` / `.dll`).
    Native,
}

/// A resolved `[lib]` import: the on-disk file plus how to load it.
#[derive(Debug, Clone)]
pub struct ResolvedLib {
    pub path: PathBuf,
    pub kind: ImportKind,
}

/// Map a recognized library file extension to its import kind.
fn kind_for_ext(ext: &str) -> Option<ImportKind> {
    match ext {
        "jde" => Some(ImportKind::Jade),
        "dylib" | "so" | "dll" => Some(ImportKind::Native),
        _ => None,
    }
}

/// Split a recognized library extension off a filename, preserving any
/// subdirectory in the stem. Returns `(stem, ext)`, or `(name, "")` if none match.
fn split_lib_ext(name: &str) -> (&str, &str) {
    for (suffix, ext) in [(".jde", "jde"), (".dylib", "dylib"), (".so", "so"), (".dll", "dll")] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return (stem, ext);
        }
    }
    (name, "")
}

/// Resolve a `use` path against registered `[lib]` libraries, anchored at `root`.
///
/// A path is a *library reference* when it has the form `<lib>/<module>` and
/// `<lib>` names a registered library. Resolution is anchored at the project
/// `root` (not the importing file) — this is what enables cross-directory
/// imports. The resolved file's extension determines its [`ImportKind`]. With a
/// `files` allowlist the module must match one of its entries (by stem);
/// otherwise the directory is probed for `<module>.jde` then a native library.
///
/// Returns:
///   * `Ok(Some(resolved))` — a registered library module + how to load it,
///   * `Ok(None)` — not a library reference; the caller falls back to normal
///     relative-path resolution (hybrid mode),
///   * `Err(msg)` — the library exists but the module is not registered / has an
///     unsupported extension.
pub fn resolve_library_import(
    libs: &HashMap<String, LibraryEntry>,
    import_path: &str,
    root: &Path,
) -> Result<Option<ResolvedLib>, String> {
    let Some((lib_name, rest)) = import_path.split_once('/') else {
        return Ok(None);
    };
    let Some(entry) = libs.get(lib_name) else {
        return Ok(None);
    };
    // The import may carry a recognized extension (string form); normalize to a
    // bare module stem, preserving any subpath.
    let (module, _) = split_lib_ext(rest);

    let base = if Path::new(&entry.path).is_absolute() {
        PathBuf::from(&entry.path)
    } else {
        root.join(&entry.path)
    };

    if let Some(files) = &entry.files {
        for f in files {
            let (stem, ext) = split_lib_ext(f);
            if stem == module {
                let kind = kind_for_ext(ext).ok_or_else(|| {
                    format!(
                        "module '{f}' in [lib.{lib_name}] of jade.toml has an unsupported \
                         extension (expected .jde, .dylib, .so, or .dll)"
                    )
                })?;
                return Ok(Some(ResolvedLib { path: base.join(f), kind }));
            }
        }
        return Err(format!(
            "module '{module}' is not registered in [lib.{lib_name}] of jade.toml \
             (registered files: {:?})",
            files
        ));
    }

    // No allowlist: prefer a Jade source module, then probe for a native library.
    let jde = base.join(format!("{module}.jde"));
    if jde.exists() {
        return Ok(Some(ResolvedLib { path: jde, kind: ImportKind::Jade }));
    }
    for suffix in [".dylib", ".so", ".dll"] {
        let cand = base.join(format!("{module}{suffix}"));
        if cand.exists() {
            return Ok(Some(ResolvedLib { path: cand, kind: ImportKind::Native }));
        }
    }
    // Nothing on disk — return the `.jde` candidate so the caller surfaces a
    // normal not-found error.
    Ok(Some(ResolvedLib { path: jde, kind: ImportKind::Jade }))
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
