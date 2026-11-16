//! Rust packages: building a project's Rust dependencies and reading what they export.
//!
//! A Rust package is a crate that marks functions and structs with `#[jade::export]`
//! (`jade-rs/`), named in `jade.toml` with `abi = "rust"`. [`build`] writes the
//! project's generated crate and builds it with cargo (`cargo.rs`), reads the
//! description records out of the staticlib (`meta.rs`), and hands back each
//! package's functions and structs with the library to link. `mir::imports` calls it
//! the first time a program's `use` names a Rust package, so `jade check` builds the
//! Rust too: the records are the only place a signature comes from.
//!
//! `design/v2-rust-packages.md` has the design and the reasons for it.

pub mod cargo;
pub mod meta;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::project::{Abi, DependencyEntry, ProjectManifest};

pub use meta::{Function, Param, Struct};

/// Purpose: This struct holds a project's Rust packages, built.
#[derive(Debug, Clone)]
pub struct Build {
    /// The staticlib holding every package, which the program is linked with.
    pub staticlib: PathBuf,
    /// The system libraries the staticlib needs, as linker arguments.
    pub native_libs: Vec<String>,
    /// Each package by its name in `jade.toml`.
    pub packages: BTreeMap<String, Package>,
    /// Every exported struct in the library, by `crate::Name`, which is how a
    /// record's types name one.
    pub structs: BTreeMap<String, Struct>,
}

impl Build {
    /// Purpose: This function says what the backend links for these packages.
    pub fn link(&self) -> Link {
        Link { staticlib: self.staticlib.clone(), native_libs: self.native_libs.clone() }
    }
}

/// Purpose: This struct holds what a program that calls a Rust package is linked
/// with, besides its own object and the C library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The staticlib holding every Rust package of the project.
    pub staticlib: PathBuf,
    /// The system libraries it needs, as rustc's `native-static-libs` note
    /// lists them: `-lSystem`, `-framework Security` and the like.
    pub native_libs: Vec<String>,
}

/// Purpose: This struct holds what one Rust package exports.
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// The crate's name, as rustc gives it and the records carry it.
    pub krate: String,
    /// Each exported function, by name.
    pub functions: BTreeMap<String, Function>,
    /// Each exported struct's own name, with its key into [`Build::structs`].
    pub structs: BTreeMap<String, String>,
}

/// Purpose: This function lists a project's Rust dependencies.
///
/// Inputs:
/// - `manifest`: the project's manifest.
///
/// Outputs: each `abi = "rust"` dependency by its name in `jade.toml`.
pub fn dependencies(manifest: &ProjectManifest) -> BTreeMap<String, DependencyEntry> {
    manifest
        .dependencies
        .iter()
        .flatten()
        .filter(|(_, d)| d.abi == Some(Abi::Rust))
        .map(|(n, d)| (n.clone(), d.clone()))
        .collect()
}

/// Purpose: This function names the class a Rust struct becomes in a program.
///
/// Inputs:
/// - `krate`: the crate that exports it.
/// - `name`: the struct's name.
///
/// Outputs: `<name>$r<crate>`, which messages show as `<name>` and no program can
/// write.
pub fn class_name(krate: &str, name: &str) -> String {
    format!("{name}$r{krate}")
}

/// Purpose: This function returns jade's home directory, `~/.jade`.
pub fn jade_home() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".jade")
}

/// Purpose: This function builds a project's Rust packages and reads what they
/// export.
///
/// Inputs:
/// - `root`: the project's root.
/// - `manifest`: its manifest.
///
/// Outputs: the build, `None` for a project with no Rust dependencies, or a message:
/// cargo is missing, a package did not compile, or a record cannot be read.
pub fn build(root: &Path, manifest: &ProjectManifest) -> Result<Option<Build>, String> {
    let deps = dependencies(manifest);
    if deps.is_empty() {
        return Ok(None);
    }
    for (name, d) in &deps {
        d.validate(name)?;
    }
    let places = cargo::places(&jade_home(), root);
    cargo::write_if_changed(
        &places.crate_dir.join("Cargo.toml"),
        &cargo::manifest(&places.crate_name, root, &deps)?,
    )?;
    cargo::write_if_changed(&places.crate_dir.join("lib.rs"), &cargo::lib_rs(&deps))?;
    let built = cargo::build(&places)?;
    let bytes = std::fs::read(&built.staticlib)
        .map_err(|e| format!("cannot read {} ({e})", built.staticlib.display()))?;
    let records = meta::read_library(&bytes)?;
    meta::check(&records)?;
    let names = cargo::crate_names(&places)?;
    Ok(Some(assemble(&deps, &names, records, built)))
}

/// Purpose: This function sorts the records into packages.
///
/// Inputs:
/// - `deps`: the Rust dependencies, by their names in `jade.toml`.
/// - `names`: the crate each dependency is, from cargo.
/// - `records`: everything the library describes.
/// - `built`: the staticlib and its system libraries.
///
/// Outputs: the build. A dependency cargo did not describe falls back to its own
/// name, the usual crate name, so its functions still arrive if they exist.
pub fn assemble(
    deps: &BTreeMap<String, DependencyEntry>,
    names: &BTreeMap<String, String>,
    records: meta::Records,
    built: cargo::Built,
) -> Build {
    let mut packages = BTreeMap::new();
    for (name, d) in deps {
        let krate = names
            .get(name)
            .cloned()
            .unwrap_or_else(|| d.package.as_deref().unwrap_or(name.as_str()).replace('-', "_"));
        let functions = records.functions.get(&krate).cloned().unwrap_or_default();
        let structs = records
            .structs
            .values()
            .filter(|s| s.krate == krate)
            .map(|s| (s.name.clone(), s.key()))
            .collect();
        packages.insert(name.clone(), Package { krate, functions, structs });
    }
    Build {
        staticlib: built.staticlib,
        native_libs: built.native_libs,
        packages,
        structs: records.structs,
    }
}

#[cfg(test)]
mod tests;
