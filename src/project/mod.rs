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
    /// `[dependencies.<name>]` sections: external native packages, fetched into
    /// the project-local `libs/` directory and pinned by `jade.lock`. See
    /// [`DependencyEntry`].
    pub dependencies: Option<HashMap<String, DependencyEntry>>,
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
    let (lib_name, rest) = match import_path.split_once('/') {
        Some(pair) => pair,
        // A bare path — `use fastmath`. A dependency is a single artifact with
        // no second segment to name, so a lone name that matches a registered
        // library resolves to the module of the same name: `fastmath` →
        // `fastmath/fastmath`. When the name is *not* registered the `libs`
        // lookup below returns `Ok(None)` exactly as before, so ordinary
        // relative-file imports are unaffected.
        None => (import_path, import_path),
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

/// Resolve a bare/`::` import that is **not** a registered library to a file
/// relative to the importing file's `dir`. `import_path` is a module stem (no
/// extension), optionally with `/`-separated subdirectories (`sub/helper`).
///
/// This is the fallback that makes `use utils` load `./utils.jde` and
/// `use sub::helper` load `./sub/helper.jde` — the same probe order
/// `resolve_library_import` uses without an allowlist: a `.jde` source module
/// first, then a native library. A parent path (`..`) is not expressible in
/// `::` notation, so cross-directory imports go through `[lib]` instead.
///
/// Always returns a candidate; a missing file surfaces as a normal not-found
/// error at the read site (mirroring `resolve_library_import`).
pub fn resolve_relative_import(dir: &Path, import_path: &str) -> ResolvedLib {
    let jde = dir.join(format!("{import_path}.jde"));
    if jde.exists() {
        return ResolvedLib { path: jde, kind: ImportKind::Jade };
    }
    for suffix in ["dylib", "so", "dll"] {
        let cand = dir.join(format!("{import_path}.{suffix}"));
        if cand.exists() {
            return ResolvedLib { path: cand, kind: ImportKind::Native };
        }
    }
    ResolvedLib { path: jde, kind: ImportKind::Jade }
}

// ── Dependencies ──────────────────────────────────────────────────────────────

/// Which ABI a dependency's shared library speaks.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Abi {
    /// Exports `jade_pkg_init` directly — a Jade-authored package, or a Rust
    /// `cdylib` written against the Jade ABI.
    #[default]
    Jade,
    /// A plain C library with no knowledge of Jade. Requires a `[symbols]`
    /// table; a generated shim supplies `jade_pkg_init` at install time.
    C,
}

impl Abi {
    /// The spelling used in `jade.lock`. Kept explicit rather than derived from
    /// `Debug` so the lock format can't drift when this enum is refactored.
    pub fn as_str(self) -> &'static str {
        match self {
            Abi::Jade => "jade",
            Abi::C => "c",
        }
    }
}

/// One entry of a `[dependencies.<name>.symbols]` table: the C prototype of a
/// symbol to bind, in terms of the FFI's primitive type names.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CSymbol {
    pub args: Vec<String>,
    pub ret: String,
}

/// Entry in a `[dependencies.<name>]` section of `jade.toml`.
///
/// ```toml
/// [dependencies.fastmath]
/// version = "1.2.0"
/// url     = "https://example.com/fastmath-{platform}.so"
///
/// [dependencies.zlib]
/// version = "1.3.1"
/// path    = "vendor/libz.so"
/// abi     = "c"
/// [dependencies.zlib.symbols.crc32]
/// args = ["int", "str"]
/// ret  = "int"
/// ```
///
/// Exactly one of `path` or `url` names the source. A `url` may contain the
/// `{platform}` placeholder, expanded per target (`darwin-aarch64`,
/// `linux-x86_64`, …) when the lock is generated.
///
/// **Integrity lives in `jade.lock`, not here** — deliberately, following
/// Cargo. `jade add` fetches each platform's artifact once, hashes it, and
/// records the digests in the lock; every later install verifies against those.
/// A `{platform}` URL could not carry a single digest in the manifest anyway.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DependencyEntry {
    /// Exact version. Not a range — see [`DependencyEntry::validate`].
    pub version: Option<String>,
    /// Path to a local `.so`/`.dylib`, relative to the project root.
    pub path: Option<String>,
    /// Download URL, optionally containing `{platform}`.
    pub url: Option<String>,
    #[serde(default)]
    pub abi: Abi,
    /// Required for `abi = "c"`: the symbols to bind, and their prototypes.
    pub symbols: Option<HashMap<String, CSymbol>>,
}

/// Placeholder expanded to a platform tag when resolving a dependency `url`.
pub const PLATFORM_PLACEHOLDER: &str = "{platform}";

/// Characters that only appear in a *range* version requirement. Ranges need a
/// registry to resolve against, and Jade deliberately has none — a dependency
/// names one exact artifact. Rejecting these is friendlier than accepting a
/// range and silently treating it as a literal string that matches nothing.
const RANGE_CHARS: &[char] = &['^', '~', '*', '>', '<', '=', ',', '|'];

impl DependencyEntry {
    /// Whether this dependency's `url` is a per-platform template.
    pub fn is_platform_template(&self) -> bool {
        self.url.as_deref().is_some_and(|u| u.contains(PLATFORM_PLACEHOLDER))
    }

    /// Check the entry is well-formed, naming `name` in every error so the
    /// message is actionable without the user hunting for which table is wrong.
    pub fn validate(&self, name: &str) -> Result<(), String> {
        match (&self.path, &self.url) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "dependency '{name}' sets both 'path' and 'url' in jade.toml \
                     (a dependency has exactly one source)"
                ));
            }
            (None, None) => {
                return Err(format!(
                    "dependency '{name}' has no source in jade.toml \
                     (set either 'path' or 'url')"
                ));
            }
            _ => {}
        }

        // A url dependency is fetched into `libs/<name>-<version>/`, so the
        // version is what makes that directory unique — it can't be omitted.
        // A path dependency points at a file the user already controls.
        if self.url.is_some() && self.version.is_none() {
            return Err(format!(
                "dependency '{name}' has a 'url' but no 'version' in jade.toml \
                 (url dependencies are pinned to an exact version)"
            ));
        }

        if let Some(version) = &self.version {
            if version.trim().is_empty() {
                return Err(format!("dependency '{name}' has an empty 'version' in jade.toml"));
            }
            if let Some(bad) = version.chars().find(|c| RANGE_CHARS.contains(c)) {
                return Err(format!(
                    "dependency '{name}' has version '{version}' in jade.toml, but \
                     version ranges are not supported (found '{bad}') — Jade has no \
                     package registry to resolve a range against, so dependencies \
                     name one exact version, e.g. \"1.2.0\""
                ));
            }
        }

        match self.abi {
            Abi::C => {
                let empty = self.symbols.as_ref().is_none_or(|s| s.is_empty());
                if empty {
                    return Err(format!(
                        "dependency '{name}' sets abi = \"c\" but declares no \
                         [dependencies.{name}.symbols] in jade.toml — a plain C library \
                         exports no jade_pkg_init, so Jade needs the symbol prototypes \
                         to generate a binding shim"
                    ));
                }
            }
            Abi::Jade => {
                if self.symbols.is_some() {
                    return Err(format!(
                        "dependency '{name}' declares [dependencies.{name}.symbols] but \
                         does not set abi = \"c\" in jade.toml — a Jade-ABI package \
                         exports jade_pkg_init and describes its own bindings"
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Detect a bare-name import that could mean two different things.
///
/// `use fastmath` resolves against registered libraries (including
/// dependencies), but a sibling `fastmath.jde` next to the importing file would
/// have resolved too, before bare names named libraries. Picking one silently
/// is the kind of ambiguity that costs an afternoon, so the caller turns this
/// into a hard error naming both candidates.
///
/// Only bare names are ambiguous: a slashed path is unambiguously a library
/// reference, and a quoted string import is unambiguously a file.
///
/// Returns `Some(message)` when ambiguous, `None` otherwise.
pub fn ambiguous_bare_import(
    import_path: &str,
    libs: &HashMap<String, LibraryEntry>,
    source_dir: &Path,
) -> Option<String> {
    if import_path.contains('/') || !libs.contains_key(import_path) {
        return None;
    }

    let sibling = source_dir.join(format!("{import_path}.jde"));
    if !sibling.exists() {
        return None;
    }

    Some(format!(
        "import '{import_path}' is ambiguous: it names both a registered library \
         (or dependency) and the sibling file {}. Rename one of them.",
        sibling.display()
    ))
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
