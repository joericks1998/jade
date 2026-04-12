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
    pub model: Option<ManifestModelSection>,
    pub dependencies: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectSection {
    pub name: String,
    pub version: Option<String>,
    pub entry: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestModelSection {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub max_retries: Option<usize>,
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
    let mut dir = std::env::current_dir().ok()?;
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
