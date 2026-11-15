use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

mod imports;
#[cfg(test)]
mod tests;

pub use imports::{
    ImportContext, ImportTarget, program_import_paths, reachable_jade_modules, resolve_import,
    walk_imports,
};

// ── Manifest types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectManifest {
    pub project: Option<ProjectSection>,
    pub scripts: Option<HashMap<String, String>>,
    /// `[dependencies.<name>]` sections: Jade source packages, fetched into the
    /// project-local `libs/` directory and pinned by `jade.lock`, and Rust
    /// packages, which cargo builds. See [`DependencyEntry`].
    pub dependencies: Option<HashMap<String, DependencyEntry>>,
    /// `[lib.<name>]` sections: register a directory and its Jade modules as a
    /// named library so they can be imported cross-directory via
    /// `use <name>.<module>`, anchored at the project root. See
    /// [`resolve_library_import`].
    pub lib: Option<HashMap<String, LibraryEntry>>,
    /// `[package]` section: this project *is* a Jade package, and
    /// `jade build --lib` should read what it is made of from here rather than
    /// from command-line flags. See [`PackageSection`].
    pub package: Option<PackageSection>,
    /// `[model]`: which model a program that dereferences a prompt runs, by
    /// its name in the dovata process. See [`ModelSection`].
    pub model: Option<ModelSection>,
}

/// Purpose: This struct holds the `[model]` section of `jade.toml`.
///
/// ```toml
/// [model]
/// name = "qwen3-0.6b"
/// ```
///
/// Without it, a program runs the one model the dovata process holds.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelSection {
    /// The model's name, as `dovata status` lists it.
    pub name: Option<String>,
}

/// The `[package]` section of `jade.toml` — a project that builds itself into a
/// shared library other projects can depend on.
///
/// ```toml
/// [package]
/// name    = "mathlib"
/// version = "1.2.0"
/// entry   = "mathlib.jde"                              # optional
/// sources = ["geometry.jde", "text.jde", "mathlib.jde"]  # optional
/// exports = ["area", "shout", "version"]               # optional
/// ```
///
/// ## Why `sources` when the imports already say
///
/// The backend finds a package's files by following `use` from `entry`, so the
/// build works without this list. What the list buys is the two errors the
/// import graph cannot raise on its own: a file you meant to ship but forgot to
/// import (it silently vanishes from the package), and a file that got pulled in
/// without you deciding to ship it. Declaring the set makes both a build failure
/// naming the file, and makes `jade.toml` an honest inventory of the package.
///
/// It is optional. Omit it and the import graph is taken at its word.
#[derive(Debug, Clone, Deserialize)]
pub struct PackageSection {
    /// Package name. Also the artifact's stem, and the name consumers import.
    pub name: String,
    /// Version of the package. A label recorded for the publisher's benefit —
    /// there is no registry to resolve it against.
    pub version: Option<String>,
    /// Entry module, whose top-level functions form the package's API. Defaults
    /// to `<name>.jde`.
    pub entry: Option<String>,
    /// Every `.jde` file the package is made of, relative to the project root.
    /// Checked against what the entry actually imports; see above.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Functions to bind. Omit to export all of the entry module's.
    #[serde(default)]
    pub exports: Option<Vec<String>>,
}

impl PackageSection {
    /// The entry module, defaulting to `<name>.jde`.
    pub fn entry_file(&self) -> String {
        self.entry.clone().unwrap_or_else(|| format!("{}.jde", self.name))
    }

    /// The artifact this package builds to, using the host's shared-library
    /// extension. Named after the package because `use <name>` resolves by stem.
    pub fn artifact_file(&self) -> String {
        let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        format!("{}.{ext}", self.name)
    }

    /// Check the section is well-formed, naming the offending value in every
    /// error — a manifest is hand-written, so "which line do I fix" has to be
    /// answerable from the message alone.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("[package] in jade.toml has an empty 'name'".to_string());
        }
        // The name becomes a filename and an import name, so the characters an
        // identifier allows are exactly the characters that work here.
        if !self.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "[package] name '{}' in jade.toml is not a usable package name \
                 (letters, digits and underscores only — it becomes both a filename \
                 and the name `use {}` binds)",
                self.name, self.name
            ));
        }

        let entry = self.entry_file();
        if !entry.ends_with(".jde") {
            return Err(format!(
                "[package] entry '{entry}' in jade.toml is not a Jade source file \
                 (expected a .jde file)"
            ));
        }

        if let Some(sources) = &self.sources {
            if sources.is_empty() {
                return Err("[package] sources in jade.toml is empty — omit it entirely to \
                     take the import graph at its word"
                    .to_string());
            }
            for s in sources {
                if !s.ends_with(".jde") {
                    return Err(format!(
                        "[package] source '{s}' in jade.toml is not a Jade source file \
                         (expected a .jde file). A native library is a dependency, \
                         not a source — declare it under [dependencies]"
                    ));
                }
            }
            // The entry is part of the package it heads. Requiring it to be
            // listed keeps `sources` readable as the complete inventory rather
            // than as "the other files".
            if !sources.iter().any(|s| s == &entry) {
                return Err(format!(
                    "[package] sources in jade.toml does not list the entry module \
                     '{entry}'; sources is the package's complete file list, so the \
                     entry belongs in it"
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for s in sources {
                if !seen.insert(s) {
                    return Err(format!(
                        "[package] sources in jade.toml lists '{s}' more than once"
                    ));
                }
            }
        }

        if self.exports.as_ref().is_some_and(|e| e.is_empty()) {
            return Err("[package] exports in jade.toml is empty, which would build a \
                 package binding nothing — omit it to export every function"
                .to_string());
        }

        Ok(())
    }
}

/// Entry in a `[lib.<name>]` section of `jade.toml`.
///
/// ```toml
/// [lib.utils]
/// path  = "src/utils"           # directory, relative to the project root
/// files = ["math.jde"]          # optional allowlist of importable filenames
/// ```
///
/// `files` is an allowlist of importable module filenames, with the `.jde`
/// extension. Omit it to make every `.jde` file in `path` importable.
///
/// A dependency becomes one of these too (see `pkg::resolved_libraries`): a
/// source package's points at its directory, and a Rust package's carries the
/// dependency's name in `rust`, since its functions come from the build rather
/// than a file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LibraryEntry {
    pub path: String,
    #[serde(default)]
    pub files: Option<Vec<String>>,
    /// The module a bare `use <name>` names, relative to `path`. A source
    /// package's entry module, which need not share the package's name.
    #[serde(default)]
    pub entry: Option<String>,
    /// The entry module's names other projects may use, from a source
    /// package's `[package] exports`. `None` shows every top-level name.
    #[serde(default)]
    pub exports: Option<Vec<String>>,
    /// Set for a Rust package: the dependency's name in `jade.toml`. Never
    /// written in a manifest; `[lib]` holds Jade modules alone.
    #[serde(skip)]
    pub rust: Option<String>,
}

/// What a resolved `[lib]` import is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// A Jade source module (`.jde`).
    Jade,
    /// A Rust package, whose functions the build reads out of its library; the
    /// resolved path is the dependency's name.
    Rust,
}

/// A resolved `[lib]` import: the on-disk file plus how to load it.
#[derive(Debug, Clone)]
pub struct ResolvedLib {
    pub path: PathBuf,
    pub kind: ImportKind,
    /// The names an importer may use, when the file is a source package's
    /// entry module and the package lists its exports.
    pub exports: Option<Vec<String>>,
}

impl ResolvedLib {
    /// Purpose: This function makes a resolved import that shows every name.
    ///
    /// Inputs:
    /// - `path`: the file.
    /// - `kind`: how it loads.
    ///
    /// Outputs: the resolved import.
    fn open(path: PathBuf, kind: ImportKind) -> Self {
        ResolvedLib { path, kind, exports: None }
    }
}

/// Split the `.jde` extension off a filename, preserving any subdirectory in
/// the stem. Returns `(stem, "jde")`, or `(name, "")` without one.
fn split_lib_ext(name: &str) -> (&str, &str) {
    match name.strip_suffix(".jde") {
        Some(stem) => (stem, "jde"),
        None => (name, ""),
    }
}

/// Resolve a `use` path against registered `[lib]` libraries, anchored at `root`.
///
/// A path is a *library reference* when it has the form `<lib>/<module>` and
/// `<lib>` names a registered library. Resolution is anchored at the project
/// `root` (not the importing file) — this is what enables cross-directory
/// imports. With a `files` allowlist the module must match one of its entries
/// (by stem); otherwise the directory is probed for `<module>.jde`. A Rust
/// package resolves to its dependency's name, with [`ImportKind::Rust`].
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
    // A Rust package is one module, named by the dependency.
    if let Some(entry) = libs.get(import_path)
        && let Some(dep) = &entry.rust
    {
        return Ok(Some(ResolvedLib::open(PathBuf::from(dep), ImportKind::Rust)));
    }
    let (lib_name, rest) = match import_path.split_once('/') {
        Some(pair) => pair,
        // A source package names its entry module, and a bare `use geometry`
        // means that module whatever it is called.
        None if libs.get(import_path).is_some_and(|e| e.entry.is_some()) => {
            let entry = &libs[import_path];
            let base = if Path::new(&entry.path).is_absolute() {
                PathBuf::from(&entry.path)
            } else {
                root.join(&entry.path)
            };
            let file = entry.entry.as_deref().unwrap_or_default();
            return Ok(Some(ResolvedLib {
                path: base.join(file),
                kind: ImportKind::Jade,
                exports: entry.exports.clone(),
            }));
        }
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
            if stem == module && ext == "jde" {
                return Ok(Some(ResolvedLib::open(base.join(f), ImportKind::Jade)));
            }
            // A shared library was a module until 2.0.5; say what replaced it
            // rather than calling the name unregistered.
            let native = Path::new(f)
                .extension()
                .is_some_and(|e| matches!(e.to_str(), Some("dylib" | "so" | "dll")))
                && Path::new(f).with_extension("") == Path::new(module);
            if native {
                return Err(format!(
                    "module '{f}' in [lib.{lib_name}] of jade.toml is a shared library; [lib] \
                     lists .jde files, and native code is a Rust package under [dependencies] \
                     with abi = \"rust\" (see design/v2-rust-packages.md)"
                ));
            }
        }
        return Err(format!(
            "module '{module}' is not registered in [lib.{lib_name}] of jade.toml \
             (registered files: {:?})",
            files
        ));
    }

    // No allowlist: the module's `.jde` file. Nothing on disk still returns the
    // candidate, so the caller surfaces a normal not-found error.
    Ok(Some(ResolvedLib::open(base.join(format!("{module}.jde")), ImportKind::Jade)))
}

/// Resolve a bare/`::` import that is **not** a registered library to a file
/// relative to the importing file's `dir`. `import_path` is a module stem (no
/// extension), optionally with `/`-separated subdirectories (`sub/helper`).
///
/// This is the fallback that makes `use utils` load `./utils.jde` and
/// `use sub::helper` load `./sub/helper.jde`. A parent path (`..`) is not
/// expressible in `::` notation, so cross-directory imports go through `[lib]`
/// instead.
///
/// Always returns a candidate; a missing file surfaces as a normal not-found
/// error at the read site (mirroring `resolve_library_import`).
pub fn resolve_relative_import(dir: &Path, import_path: &str) -> ResolvedLib {
    ResolvedLib::open(dir.join(format!("{import_path}.jde")), ImportKind::Jade)
}

// ── Dependencies ──────────────────────────────────────────────────────────────

/// What kind of package a dependency is.
///
/// Written out in every entry, since neither kind is a sensible default. Until
/// 2.0.5 there were two more, `jade` and `c`, for prebuilt shared libraries;
/// `design/v2-rust-packages.md` says why Rust replaced both, and reading one
/// stops with a message pointing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// Jade source: a directory, git repository or `.tar.gz` archive holding a
    /// project with a `[package]` section. Its files are compiled into each
    /// program that uses it, the way design decision 11 settles.
    Source,
    /// A Rust crate that marks what Jade may call with `#[jade::export]`,
    /// built by cargo into the program (see `rustpkg`).
    Rust,
}

impl Abi {
    /// The spelling used in `jade.toml` and `jade.lock`. Kept explicit rather
    /// than derived from `Debug` so the formats can't drift when this enum is
    /// refactored.
    pub fn as_str(self) -> &'static str {
        match self {
            Abi::Source => "source",
            Abi::Rust => "rust",
        }
    }
}

/// Reads `"source"` or `"rust"`, and names the replacement for the two kinds
/// 2.0.5 removed, so the message lands on the manifest's own line.
impl<'de> Deserialize<'de> for Abi {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "source" => Ok(Abi::Source),
            "rust" => Ok(Abi::Rust),
            "c" => Err(serde::de::Error::custom(
                "abi = \"c\" was removed in jade 2.0.5: a C library is reached through a Rust \
                 crate that wraps it, such as its -sys crate, named with abi = \"rust\" (see \
                 design/v2-rust-packages.md)",
            )),
            "jade" => Err(serde::de::Error::custom(
                "abi = \"jade\" was removed in jade 2.0.5: a Jade package is shared as source \
                 with abi = \"source\", and native code is a Rust crate with abi = \"rust\" \
                 (see design/v2-rust-packages.md)",
            )),
            other => Err(serde::de::Error::custom(format!(
                "abi = \"{other}\" is not a kind of package; write abi = \"source\" for Jade \
                 source or abi = \"rust\" for a Rust crate"
            ))),
        }
    }
}

/// Entry in a `[dependencies.<name>]` section of `jade.toml`.
///
/// ```toml
/// [dependencies.geometry]
/// path = "../geometry"
/// abi  = "source"
///
/// [dependencies.imgtools]
/// path    = "rust/imgtools"       # a directory holding a Cargo.toml
/// package = "img-tools"           # the Rust package's name, when it differs
/// abi     = "rust"
/// ```
///
/// A source package comes from exactly one of `path`, `url` or `git`. A Rust
/// package comes from a `path`, a `git` repository, or a crates.io `version`.
///
/// **Integrity lives in `jade.lock`, not here** — deliberately, following
/// Cargo. `jade pkg add` hashes a source package once and records the digest in
/// the lock; every later install verifies against it. A Rust package is pinned
/// by cargo, in the generated crate's `Cargo.lock` (see `rustpkg::cargo`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DependencyEntry {
    /// Exact version. Not a range — see [`DependencyEntry::validate`]. For a
    /// Rust package with no other source, the crates.io version to build.
    pub version: Option<String>,
    /// A directory, relative to the project root: a source package's project,
    /// or a Rust package's crate.
    pub path: Option<String>,
    /// Download URL of a source package's `.tar.gz` archive.
    pub url: Option<String>,
    /// A git repository holding the package. One of `rev`, `tag` or `branch`
    /// names the commit.
    pub git: Option<String>,
    pub rev: Option<String>,
    pub tag: Option<String>,
    pub branch: Option<String>,
    /// What kind of package it is. Required; see [`Abi`].
    #[serde(default)]
    pub abi: Option<Abi>,
    /// A Rust package's own name, when it is not the dependency's: a Jade name is
    /// an identifier, and a Rust package's name may hold a `-`.
    pub package: Option<String>,
}

/// Placeholder expanded to a platform tag when resolving a dependency `url`.
pub const PLATFORM_PLACEHOLDER: &str = "{platform}";

/// Characters that only appear in a *range* version requirement. A dependency
/// names one exact version, so a build is the same build on every machine.
/// Rejecting these is friendlier than accepting a range and silently treating
/// it as a literal string that matches nothing.
const RANGE_CHARS: &[char] = &['^', '~', '*', '>', '<', '=', ',', '|'];

impl DependencyEntry {
    /// Whether this dependency's `url` is a per-platform template.
    pub fn is_platform_template(&self) -> bool {
        self.url.as_deref().is_some_and(|u| u.contains(PLATFORM_PLACEHOLDER))
    }

    /// The kind of package. An entry with no `abi` reads as source here, and
    /// [`validate`](Self::validate) refuses it before anything relies on that.
    pub fn kind(&self) -> Abi {
        self.abi.unwrap_or(Abi::Source)
    }

    /// Check the entry is well-formed, naming `name` in every error so the
    /// message is actionable without the user hunting for which table is wrong.
    pub fn validate(&self, name: &str) -> Result<(), String> {
        let Some(abi) = self.abi else {
            return Err(format!(
                "dependency '{name}' has no abi in jade.toml; write abi = \"source\" for a Jade \
                 source package or abi = \"rust\" for a Rust crate"
            ));
        };
        if abi == Abi::Rust {
            return self.validate_rust(name);
        }
        if self.package.is_some() {
            return Err(format!(
                "dependency '{name}' sets 'package' in jade.toml, which names a Rust package; a \
                 Jade source package is named by its own [package] section"
            ));
        }
        let sources = [self.path.is_some(), self.url.is_some(), self.git.is_some()];
        match sources.iter().filter(|s| **s).count() {
            0 => {
                return Err(format!(
                    "dependency '{name}' has no source in jade.toml \
                     (set one of 'path', 'url' or 'git')"
                ));
            }
            1 => {}
            _ => {
                return Err(format!(
                    "dependency '{name}' sets more than one of 'path', 'url' and 'git' in \
                     jade.toml (a dependency has exactly one source)"
                ));
            }
        }
        self.validate_git_ref(name)?;

        // A url dependency is fetched into `libs/<name>-<version>/`, so the
        // version is what makes that directory unique — it can't be omitted.
        if self.url.is_some() && self.version.is_none() {
            return Err(format!(
                "dependency '{name}' has a 'url' but no 'version' in jade.toml \
                 (url dependencies are pinned to an exact version)"
            ));
        }
        self.validate_version(name)?;
        if self.is_platform_template() {
            return Err(format!(
                "dependency '{name}' is Jade source, which is the same on every platform, so its \
                 url in jade.toml takes no {{platform}}"
            ));
        }
        Ok(())
    }

    /// Purpose: This function checks a Rust package's entry: one of `path`, `git`
    /// or a crates.io `version`.
    fn validate_rust(&self, name: &str) -> Result<(), String> {
        if self.url.is_some() {
            return Err(format!(
                "dependency '{name}' is a Rust package with a 'url' in jade.toml; a Rust package \
                 comes from a 'path', a 'git' repository, or a crates.io 'version', and cargo \
                 fetches it"
            ));
        }
        if self.path.is_some() && self.git.is_some() {
            return Err(format!(
                "dependency '{name}' sets both 'path' and 'git' in jade.toml (a dependency has \
                 exactly one source)"
            ));
        }
        if self.path.is_none() && self.git.is_none() && self.version.is_none() {
            return Err(format!(
                "dependency '{name}' is a Rust package with no source in jade.toml; set 'path' to \
                 the directory holding its Cargo.toml, 'git' with a 'rev', 'tag' or 'branch', or \
                 'version' for crates.io"
            ));
        }
        // The name becomes an `extern crate` line in the generated crate, and a
        // Jade program writes it before a dot.
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name.starts_with(|c: char| c.is_ascii_digit())
        {
            return Err(format!(
                "dependency '{name}' is not a name a Jade program can write; name it with \
                 letters, digits and underscores, and give the Rust package's own name as \
                 package = \"{name}\""
            ));
        }
        self.validate_git_ref(name)?;
        self.validate_version(name)
    }

    /// Purpose: This function checks that a git source names one commit, and that
    /// nothing else names one.
    fn validate_git_ref(&self, name: &str) -> Result<(), String> {
        let refs = [&self.rev, &self.tag, &self.branch];
        let named = refs.iter().filter(|r| r.is_some()).count();
        if self.git.is_some() {
            if named != 1 {
                return Err(format!(
                    "dependency '{name}' needs exactly one of 'rev', 'tag' or 'branch' beside \
                     its 'git' in jade.toml, to say which commit to build"
                ));
            }
        } else if named > 0 {
            return Err(format!(
                "dependency '{name}' sets 'rev', 'tag' or 'branch' without 'git' in jade.toml"
            ));
        }
        Ok(())
    }

    /// Purpose: This function checks a version names one exact version.
    fn validate_version(&self, name: &str) -> Result<(), String> {
        if let Some(version) = &self.version {
            if version.trim().is_empty() {
                return Err(format!("dependency '{name}' has an empty 'version' in jade.toml"));
            }
            if let Some(bad) = version.chars().find(|c| RANGE_CHARS.contains(c)) {
                return Err(format!(
                    "dependency '{name}' has version '{version}' in jade.toml, but \
                     version ranges are not supported (found '{bad}') — a dependency names \
                     one exact version, e.g. \"1.2.0\""
                ));
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
        // A package is a project too: its author installs its dependencies and
        // builds it in place, and its own files resolve imports against it.
        self.project.is_some() || self.package.is_some()
    }

    /// The entry-point file for this project (default: `main.jde`).
    pub fn entry_file(&self) -> &str {
        self.project.as_ref().and_then(|p| p.entry.as_deref()).unwrap_or("main.jde")
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
///
/// **A `jade.toml` that cannot be read stops the walk.** It used to be stepped
/// over, on the reasoning that only a manifest we could see a `[project]` in was
/// a project — which turned every typo in a manifest into "there is no project
/// here", said by a command standing in the project's own root directory. The
/// question a broken file cannot answer is whether it has a `[project]`
/// section, and answering it "no" is a guess that is wrong far more often than
/// it is right. Stopping here hands the directory to [`load_project`], which is
/// what every caller reaches for next and is the one place that can name the
/// line.
pub fn find_project_root_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        match read_manifest(&dir) {
            Ok(manifest) if manifest.is_project() => return Some(dir),
            // Not a project: a `jade.toml` that parses and declares no
            // `[project]`, or no `jade.toml` at all. Keep walking up.
            Ok(_) | Err(ManifestError::Missing(_)) => {}
            Err(_) => return Some(dir),
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Why a `jade.toml` could not be turned into a [`ProjectManifest`].
///
/// The distinction that matters is *absent* versus *present and broken*. Both
/// used to collapse into one string, and every caller that could only act on
/// "there is no project here" therefore acted on it for a manifest sitting right
/// in front of them — see [`find_project_root_from`].
#[derive(Debug)]
pub enum ManifestError {
    /// No `jade.toml` in that directory. Not an error on its own: plenty of
    /// directories are not projects.
    Missing(PathBuf),
    /// A `jade.toml` is there and could not be read — permissions, most often.
    Unreadable(PathBuf, std::io::Error),
    /// A `jade.toml` is there and is not valid TOML. The inner error carries the
    /// line and column, which is the whole reason this is worth keeping typed.
    Malformed(PathBuf, Box<toml::de::Error>),
}

impl ManifestError {
    /// Whether the file exists and this is a complaint about its contents.
    ///
    /// A caller that falls back to a default manifest — `jade run` with no
    /// argument, which may legitimately be outside a project — must not do that
    /// silently for a manifest the user wrote and got wrong.
    pub fn is_present(&self) -> bool {
        !matches!(self, ManifestError::Missing(_))
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Missing(p) => write!(f, "no jade.toml at {}", p.display()),
            ManifestError::Unreadable(p, e) => write!(f, "cannot read {}: {e}", p.display()),
            // The path first, because the error's own text names a line and not
            // a file, and the file it names is rarely the one the user was
            // looking at when the command failed.
            ManifestError::Malformed(p, e) => {
                write!(
                    f,
                    "{} is not valid TOML\n{e}\nFix the line above, then re-run.",
                    p.display()
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Load the project manifest from the given root directory.
pub fn load_project(root: &Path) -> Result<ProjectManifest, ManifestError> {
    read_manifest(root)
}

/// The shared read, so root discovery and loading cannot come to disagree about
/// what a readable manifest is.
fn read_manifest(dir: &Path) -> Result<ProjectManifest, ManifestError> {
    let path = dir.join("jade.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ManifestError::Missing(path));
        }
        Err(e) => return Err(ManifestError::Unreadable(path, e)),
    };
    toml::from_str::<ProjectManifest>(&content)
        .map_err(|e| ManifestError::Malformed(path, Box::new(e)))
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
                if let Some(pat) = pattern
                    && !stem.contains(pat)
                {
                    continue;
                }
                out.push(path);
            }
        }
    }
}
