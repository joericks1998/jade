//! Format-preserving edits to a project's `jade.toml`.
//!
//! `jade pkg add` and `jade pkg remove` rewrite a file the user wrote by hand, so they
//! go through `toml_edit` rather than a parse-and-reserialize round-trip: the
//! latter would silently discard every comment and all the original layout.

use std::path::Path;

use crate::project::Abi;

/// Read `<root>/jade.toml` as an editable document.
fn document(root: &Path) -> Result<toml_edit::DocumentMut, String> {
    let path = root.join("jade.toml");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("invalid {}: {e}", path.display()))
}

fn save(root: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
    let path = root.join("jade.toml");
    std::fs::write(&path, doc.to_string())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// How a dependency being added is sourced.
pub enum Source<'a> {
    Path(&'a str),
    Url(&'a str),
    /// A git repository, and which commit: `("tag", "v1.0")`, `("rev", …)` or
    /// `("branch", …)`.
    Git(&'a str, (&'a str, &'a str)),
    /// A crates.io version alone, which only a Rust package can come from.
    Registry,
}

/// Add or replace `[dependencies.<name>]`.
///
/// Replacing rather than merging is deliberate: `jade pkg add` states the whole
/// intent of one dependency, and a half-merged entry (an old `url` beside a new
/// `path`) would fail validation in a way the user did not ask for.
pub fn add_dependency(
    root: &Path,
    name: &str,
    source: Source<'_>,
    version: Option<&str>,
    abi: Abi,
    package: Option<&str>,
) -> Result<(), String> {
    let mut doc = document(root)?;

    let deps = doc
        .entry("dependencies")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| "jade.toml has a [dependencies] key that is not a table".to_string())?;
    // Render as `[dependencies.<name>]` headers rather than one inline blob.
    deps.set_implicit(true);

    let mut table = toml_edit::Table::new();
    if let Some(v) = version {
        table.insert("version", toml_edit::value(v));
    }
    match source {
        Source::Path(p) => table.insert("path", toml_edit::value(p)),
        Source::Url(u) => table.insert("url", toml_edit::value(u)),
        Source::Git(u, (key, value)) => {
            table.insert("git", toml_edit::value(u));
            table.insert(key, toml_edit::value(value))
        }
        Source::Registry => None,
    };
    if let Some(p) = package {
        table.insert("package", toml_edit::value(p));
    }
    table.insert("abi", toml_edit::value(abi.as_str()));

    deps.insert(name, toml_edit::Item::Table(table));
    save(root, &doc)
}

/// Purpose: This function writes a whole dependency entry, as a source
/// package's own `jade.toml` declares it, into this project's manifest.
///
/// Inputs:
/// - `root`: the project root.
/// - `name`: the dependency's name.
/// - `entry`: the entry, with any path already relative to `root`.
///
/// Outputs: `Ok(())`, or a message when the manifest cannot be written.
pub fn add_entry(
    root: &Path,
    name: &str,
    entry: &crate::project::DependencyEntry,
) -> Result<(), String> {
    let git_ref = [("rev", &entry.rev), ("tag", &entry.tag), ("branch", &entry.branch)]
        .into_iter()
        .find_map(|(k, v)| v.as_deref().map(|v| (k, v)));
    let source = match (&entry.path, &entry.url, &entry.git) {
        (Some(p), _, _) => Source::Path(p),
        (_, Some(u), _) => Source::Url(u),
        (_, _, Some(g)) => Source::Git(g, git_ref.unwrap_or(("branch", "main"))),
        _ if entry.abi == Some(Abi::Rust) && entry.version.is_some() => Source::Registry,
        _ => return Err(format!("dependency '{name}' has no source")),
    };
    add_dependency(
        root,
        name,
        source,
        entry.version.as_deref(),
        entry.kind(),
        entry.package.as_deref(),
    )
}

/// Remove `[dependencies.<name>]`. Returns whether it was there.
pub fn remove_dependency(root: &Path, name: &str) -> Result<bool, String> {
    let mut doc = document(root)?;

    let Some(deps) = doc.get_mut("dependencies").and_then(|d| d.as_table_mut()) else {
        return Ok(false);
    };
    let removed = deps.remove(name).is_some();

    // Leaving an empty [dependencies] behind is noise in a file the user reads.
    if deps.is_empty() {
        doc.remove("dependencies");
    }

    if removed {
        save(root, &doc)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
