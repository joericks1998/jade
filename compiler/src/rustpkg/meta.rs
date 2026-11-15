//! Reading what a built Rust library says it exports.
//!
//! `#[jade::export]` adds one record to the library for each function and struct
//! (`jade-rs/src/meta.rs`): the bytes `JADEMETA`, a little-endian `u32` length, and
//! JSON, in a section of its own, `__jade_meta` in the `__DATA` segment on macOS and
//! `.jade_meta` on Linux. The staticlib cargo builds is an archive of object files,
//! so this opens each member with the `object` crate, finds the section, and reads
//! every record in it. The records are this crate's header files: they say what each
//! function takes and returns, in Jade's spelling, and how each struct is laid out.
//!
//! A struct's layout is checked here, against the C layout the backend will build
//! from the same field types, so a disagreement stops the build naming the struct
//! rather than reading fields at the wrong offsets.

use std::collections::BTreeMap;

use object::read::archive::ArchiveFile;
use object::{Object, ObjectSection};
use serde::Deserialize;

/// The record format this compiler reads, `jade-rs`'s `meta::FORMAT`.
pub const FORMAT: u64 = 1;

/// The bytes every record starts with.
const MAGIC: &[u8; 8] = b"JADEMETA";

/// Purpose: This struct holds one exported function, as its record describes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Function {
    /// The crate that exports it, as rustc names it.
    #[serde(rename = "crate")]
    pub krate: String,
    /// Its name, which a program writes after the package's name.
    pub name: String,
    /// The `extern "C"` wrapper to call; its `__free` companion has `__free` added.
    pub symbol: String,
    /// Each parameter's name and Jade type, in order.
    pub params: Vec<Param>,
    /// The Jade type of the result.
    pub ret: String,
    /// Whether the function returns a `Result`, so it may raise by design.
    pub raises: bool,
}

/// Purpose: This struct holds one parameter of an exported function.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Param {
    pub name: String,
    /// Its Jade type, such as `[str]` or `geo::Point?`.
    #[serde(rename = "type")]
    pub ty: String,
}

/// Purpose: This struct holds one exported struct and its C layout, as rustc laid it
/// out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Struct {
    #[serde(rename = "crate")]
    pub krate: String,
    pub name: String,
    pub size: u64,
    pub align: u64,
    pub fields: Vec<Field>,
}

/// Purpose: This struct holds one field of an exported struct.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub offset: u64,
}

impl Struct {
    /// Purpose: This function names the struct the way records' types do.
    pub fn key(&self) -> String {
        format!("{}::{}", self.krate, self.name)
    }
}

/// Purpose: This struct holds every record a library carries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Records {
    /// Each exported function, by crate, then name.
    pub functions: BTreeMap<String, BTreeMap<String, Function>>,
    /// Each exported struct, by `crate::Name`.
    pub structs: BTreeMap<String, Struct>,
}

/// Purpose: This function reads every record out of a staticlib, or an object file.
///
/// Inputs:
/// - `bytes`: the file's contents.
///
/// Outputs: the records, or a message naming what could not be read.
pub fn read_library(bytes: &[u8]) -> Result<Records, String> {
    let mut sections: Vec<Vec<u8>> = Vec::new();
    if let Ok(archive) = ArchiveFile::parse(bytes) {
        for member in archive.members() {
            let member = member.map_err(|e| format!("cannot read the Rust library: {e}"))?;
            let Ok(data) = member.data(bytes) else { continue };
            // A member that is not an object file, such as a symbol table, has no
            // records to give.
            if let Ok(file) = object::File::parse(data) {
                sections.extend(meta_sections(&file));
            }
        }
    } else {
        let file =
            object::File::parse(bytes).map_err(|e| format!("cannot read the Rust library: {e}"))?;
        sections.extend(meta_sections(&file));
    }
    let mut out = Records::default();
    for s in sections {
        for json in split_records(&s)? {
            add_record(&mut out, json)?;
        }
    }
    Ok(out)
}

/// Purpose: This function returns the contents of an object's record section.
fn meta_sections(file: &object::File) -> Vec<Vec<u8>> {
    file.sections()
        .filter(|s| matches!(s.name(), Ok("__jade_meta") | Ok(".jade_meta")))
        .filter_map(|s| s.data().ok().map(<[u8]>::to_vec))
        .collect()
}

/// Purpose: This function splits a section into its records' JSON.
///
/// Inputs:
/// - `s`: the section's bytes: records one after another, with any zero padding
///   the linker put between them.
///
/// Outputs: each record's JSON, or a message for a section that is not records.
pub fn split_records(s: &[u8]) -> Result<Vec<&str>, String> {
    let bad = || "a Rust package's description section is damaged".to_string();
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < s.len() {
        if s[at] == 0 {
            at += 1;
            continue;
        }
        if s.get(at..at + MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Err(bad());
        }
        let len = s.get(at + 8..at + 12).ok_or_else(bad)?;
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let body = s.get(at + 12..at + 12 + len).ok_or_else(bad)?;
        out.push(std::str::from_utf8(body).map_err(|_| bad())?);
        at += 12 + len;
    }
    Ok(out)
}

/// Purpose: This function adds one record.
pub(crate) fn add_record(out: &mut Records, json: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Head {
        v: u64,
        kind: String,
    }
    let head: Head = serde_json::from_str(json)
        .map_err(|e| format!("a Rust package's description cannot be read ({e}): {json}"))?;
    if head.v != FORMAT {
        return Err(format!(
            "a Rust package was built with a jade-rs whose descriptions are format {}, and this \
             compiler reads format {FORMAT}; depend on the jade-rs from the same release as this \
             compiler (jade {})",
            head.v,
            env!("CARGO_PKG_VERSION")
        ));
    }
    let wrap = |e: serde_json::Error| format!("a Rust package's description is damaged ({e})");
    match head.kind.as_str() {
        "fn" => {
            let f: Function = serde_json::from_str(json).map_err(wrap)?;
            out.functions.entry(f.krate.clone()).or_default().insert(f.name.clone(), f);
        }
        "struct" => {
            let s: Struct = serde_json::from_str(json).map_err(wrap)?;
            out.structs.insert(s.key(), s);
        }
        other => {
            return Err(format!(
                "a Rust package describes a '{other}', which this compiler does not know; \
                 depend on the jade-rs from the same release as this compiler"
            ));
        }
    }
    Ok(())
}

/// Purpose: This function computes the C size and alignment of a Jade type's C form,
/// as the backend builds it.
///
/// Inputs:
/// - `ty`: the type, as a record spells it.
/// - `structs`: every exported struct, for a struct embedded by value.
/// - `seen`: the structs being laid out, which catches a struct holding itself.
///
/// Outputs: `(size, align)`, or a message naming a type with no C form.
pub fn c_layout(
    ty: &str,
    structs: &BTreeMap<String, Struct>,
    seen: &mut Vec<String>,
) -> Result<(u64, u64), String> {
    let ty = ty.trim();
    if let Some(inner) = ty.strip_suffix('?') {
        if inner.trim().ends_with('?') {
            return Err(format!(
                "'{ty}' is an Option of an Option, which Jade cannot hold: it has one nil"
            ));
        }
        let (s, a) = c_layout(inner, structs, seen)?;
        // `{ uint8_t some; T value; }`.
        let a = a.max(1);
        return Ok((round_up(round_up(1, a) + s, a), a));
    }
    if ty.starts_with('[') || ty == "str" || ty == "bytes" {
        return Ok((24, 8));
    }
    Ok(match ty {
        "int" | "ulong" | "float" => (8, 8),
        "int32" | "uint32" | "float32" | "char" => (4, 4),
        "short" | "ushort" => (2, 2),
        "byte" | "bool" | "nil" => (1, 1),
        key => {
            let s = structs.get(key).ok_or_else(|| {
                format!("'{key}' is not a type a Rust package can export to Jade")
            })?;
            if seen.iter().any(|k| k == key) {
                return Err(format!("the struct {key} holds itself"));
            }
            seen.push(key.to_string());
            let r = struct_layout(s, structs, seen).map(|(size, align, _)| (size, align));
            seen.pop();
            r?
        }
    })
}

/// Purpose: This function lays a struct out by C's rules.
///
/// Outputs: `(size, align, each field's offset)`.
fn struct_layout(
    s: &Struct,
    structs: &BTreeMap<String, Struct>,
    seen: &mut Vec<String>,
) -> Result<(u64, u64, Vec<u64>), String> {
    let (mut at, mut align) = (0u64, 1u64);
    let mut offsets = Vec::new();
    for f in &s.fields {
        let (fs, fa) = c_layout(&f.ty, structs, seen)?;
        at = round_up(at, fa);
        offsets.push(at);
        at += fs;
        align = align.max(fa);
    }
    Ok((round_up(at, align), align, offsets))
}

fn round_up(n: u64, to: u64) -> u64 {
    n.div_ceil(to) * to
}

/// Purpose: This function checks every struct's layout against rustc's, and every
/// type a record names.
///
/// Inputs:
/// - `r`: the records.
///
/// Outputs: `Ok(())`, or a message naming the struct or function and what disagrees.
pub fn check(r: &Records) -> Result<(), String> {
    for s in r.structs.values() {
        let mut seen = vec![s.key()];
        let (size, align, offsets) = struct_layout(s, &r.structs, &mut seen)
            .map_err(|e| format!("the Rust struct {}: {e}", s.key()))?;
        let rustc: Vec<u64> = s.fields.iter().map(|f| f.offset).collect();
        if (size, align) != (s.size, s.align) || offsets != rustc {
            return Err(format!(
                "the Rust struct {} is laid out differently by rustc (size {}, align {}, offsets \
                 {rustc:?}) and by this compiler (size {size}, align {align}, offsets \
                 {offsets:?}). This is a bug in jade-rs or the compiler, not the package; \
                 report it with the struct",
                s.key(),
                s.size,
                s.align
            ));
        }
    }
    for f in r.functions.values().flat_map(BTreeMap::values) {
        for t in f.params.iter().map(|p| &p.ty).chain([&f.ret]) {
            c_layout(t, &r.structs, &mut Vec::new())
                .map_err(|e| format!("the Rust function {}.{}: {e}", f.krate, f.name))?;
        }
    }
    Ok(())
}
