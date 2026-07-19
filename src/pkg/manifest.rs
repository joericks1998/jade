//! Format-preserving edits to a project's `jade.toml`.
//!
//! `jade add` and `jade remove` rewrite a file the user wrote by hand, so they
//! go through `toml_edit` rather than a parse-and-reserialize round-trip: the
//! latter would silently discard every comment and all the original layout.

use std::path::Path;

use crate::project::{Abi, CSymbol};

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
}

/// Add or replace `[dependencies.<name>]`.
///
/// Replacing rather than merging is deliberate: `jade add` states the whole
/// intent of one dependency, and a half-merged entry (an old `url` beside a new
/// `path`) would fail validation in a way the user did not ask for.
pub fn add_dependency(
    root: &Path,
    name: &str,
    source: Source<'_>,
    version: Option<&str>,
    abi: Abi,
    symbols: Option<&std::collections::HashMap<String, CSymbol>>,
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
    };
    if abi == Abi::C {
        table.insert("abi", toml_edit::value("c"));
    }

    if let Some(symbols) = symbols {
        let mut syms = toml_edit::Table::new();
        syms.set_implicit(true);
        // Sorted so a regenerated manifest does not churn on HashMap order.
        let mut names: Vec<&String> = symbols.keys().collect();
        names.sort();
        for sym in names {
            let spec = &symbols[sym];
            let mut t = toml_edit::Table::new();
            let mut args = toml_edit::Array::new();
            for a in &spec.args {
                args.push(a.as_str());
            }
            t.insert("args", toml_edit::value(args));
            t.insert("ret", toml_edit::value(spec.ret.as_str()));
            syms.insert(sym, toml_edit::Item::Table(t));
        }
        table.insert("symbols", toml_edit::Item::Table(syms));
    }

    deps.insert(name, toml_edit::Item::Table(table));
    save(root, &doc)
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
