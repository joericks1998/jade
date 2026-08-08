//! Format-preserving edits to a project's `jade.toml`.
//!
//! `jade pkg add` and `jade pkg remove` rewrite a file the user wrote by hand, so they
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
/// Replacing rather than merging is deliberate: `jade pkg add` states the whole
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
            syms.insert(sym, symbol_item(&symbols[sym]));
        }
        table.insert("symbols", toml_edit::Item::Table(syms));
    }

    deps.insert(name, toml_edit::Item::Table(table));
    save(root, &doc)
}

/// One `[dependencies.<pkg>.symbols.<sym>]` entry.
///
/// A symbol with no known prototype is written as the bare value `sym = "?"`
/// rather than a table of empty fields. It has to be visibly unfinished: the
/// whole point is that a person reads the file and fills it in, and `args = []`
/// beside `ret = "?"` reads like a real binding for a function taking no
/// arguments.
fn symbol_item(spec: &CSymbol) -> toml_edit::Item {
    if spec.is_unresolved() {
        return toml_edit::value(crate::project::UNRESOLVED);
    }

    let mut t = toml_edit::Table::new();
    let mut args = toml_edit::Array::new();
    for a in &spec.args {
        args.push(a.as_str());
    }
    t.insert("args", toml_edit::value(args));
    t.insert("ret", toml_edit::value(spec.ret.as_str()));
    if let Some(f) = spec.fails_when {
        t.insert("fails_when", toml_edit::value(f.as_str()));
    }
    toml_edit::Item::Table(t)
}

/// Write a generated binding into `[dependencies.<name>]`.
///
/// **Merges rather than replaces.** `jade pkg bind --only sqlite3_column` is a
/// normal way to bind a large header a piece at a time, and replacing the table
/// would make the second run delete everything the first produced. Merging also
/// leaves a hand-tuned entry alone unless this run regenerated that same symbol,
/// which is the behavior you want from a generator you are allowed to correct.
///
/// The dependency must already exist. Creating one here would produce an entry
/// with no `path` or `url`, which fails validation later and further from the
/// cause.
pub fn set_bindings(
    root: &Path,
    name: &str,
    symbols: &std::collections::BTreeMap<String, CSymbol>,
    structs: &std::collections::BTreeMap<String, crate::project::CStruct>,
    headers: &[String],
    include_dirs: &[String],
) -> Result<(), String> {
    let mut doc = document(root)?;

    let dep = doc
        .get_mut("dependencies")
        .and_then(|d| d.as_table_mut())
        .and_then(|d| d.get_mut(name))
        .and_then(|d| d.as_table_mut())
        .ok_or_else(|| {
            format!(
                "no [dependencies.{name}] in jade.toml. Add the library first, with\n  \
                 jade pkg add {name} --path <the .so> --c-abi"
            )
        })?;

    dep.insert("abi", toml_edit::value("c"));

    // Merged, like the symbols below, and for the same reason: binding a second
    // header is how a library split across several is bound at all. Replacing
    // the list dropped the first header while keeping the symbols that came
    // from it, so the shim declared none of them — and C lets an undeclared
    // function be called, so it compiled clean and crashed on the first call
    // that returned a pointer. `-Werror=implicit-function-declaration` in
    // `pkg::compile_shim` is the second half of that fix.
    let mut merge_list = |key: &str, values: &[String]| {
        if values.is_empty() {
            return;
        }
        let mut arr = dep
            .get(key)
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_else(toml_edit::Array::new);
        for v in values {
            if !arr.iter().any(|e| e.as_str() == Some(v.as_str())) {
                arr.push(v.as_str());
            }
        }
        dep.insert(key, toml_edit::value(arr));
    };
    merge_list("headers", headers);
    merge_list("include_dirs", include_dirs);

    if !structs.is_empty() {
        let structs_tbl = dep
            .entry("structs")
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| format!("dependency '{name}' has a `structs` key that is not a table"))?;
        structs_tbl.set_implicit(true);
        for (sname, def) in structs {
            let mut t = toml_edit::Table::new();
            let mut fields = toml_edit::Array::new();
            for (f, ty) in &def.fields {
                let mut pair = toml_edit::Array::new();
                pair.push(f.as_str());
                pair.push(ty.as_str());
                fields.push(pair);
            }
            t.insert("fields", toml_edit::value(fields));
            structs_tbl.insert(sname, toml_edit::Item::Table(t));
        }
    }

    let syms = dep
        .entry("symbols")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("dependency '{name}' has a `symbols` key that is not a table"))?;
    syms.set_implicit(true);
    for (sname, spec) in symbols {
        syms.insert(sname, symbol_item(spec));
    }

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
