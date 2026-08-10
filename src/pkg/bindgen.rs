//! Generate a `[dependencies.<name>.symbols]` table from a C header.
//!
//! This is what makes "bind any `.so`" true in practice rather than in
//! principle. The ABI can express handles, blobs, structs and errors, but every
//! signature still had to be transcribed into `jade.toml` by hand — and SQLite
//! has around 200 entry points. A design that is only usable for libraries small
//! enough to transcribe is not usable for the libraries people want.
//!
//! ## Why clang, and why over a pipe
//!
//! Parsing C is a tar pit. Real headers are macros, conditionals, typedef
//! chains and compiler extensions, and a hand-rolled parser would misread far
//! more than it read — silently, which is the worst way for a binding generator
//! to be wrong. So the parsing is clang's: `clang -Xclang -ast-dump=json
//! -fsyntax-only` prints the whole translation unit as JSON.
//!
//! Shelling out rather than linking `libclang` is deliberate. A released `jade`
//! binary needs nothing installed to *run* — LLVM is a build-time requirement
//! only — and linking libclang would put a large native dependency into the
//! shipped binary for a feature used at `jade pkg` time. Meanwhile `cc` is
//! *already* required to bind a C library at all, so a toolchain is present by
//! the time anyone reaches this code.
//!
//! The cost is that clang's JSON AST is not a stability-guaranteed format. The
//! fields used here — `kind`, `name`, `type.qualType`, `inner`, `loc.file` —
//! have been stable for many releases, and anything unrecognised is reported
//! rather than guessed at. That is the important half: a generator that emits a
//! plausible-but-wrong binding is worse than one that says it could not.
//!
//! ## The skip report is the feature
//!
//! No generator binds everything. A `void*` user-data pointer, a varargs
//! `printf`, a callback — each is a real signature this ABI cannot express yet.
//! What matters is that the ones it drops are *named*, with the reason, so the
//! output is an honest account of the library rather than a silent subset. A
//! generator that quietly binds two thirds of an API and reports success is how
//! you discover the missing third at run time.
//!
//! Some bindings are *assumed* rather than certain — a non-const `T*` next to a
//! count is almost always an out-buffer, but "almost always" is not "always".
//! Those are bound and listed separately, so the guess is visible instead of
//! buried.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::Value;

use crate::project::{CFailure, CStruct, CSymbol};

/// What a header produced.
#[derive(Debug, Default)]
pub struct Binding {
    /// Symbols that map cleanly, by name.
    pub symbols: BTreeMap<String, CSymbol>,
    /// Structs a bound symbol fills through an out-parameter.
    pub structs: BTreeMap<String, CStruct>,
    /// Bound, but on an inference worth checking. `(symbol, what was assumed)`.
    pub assumed: Vec<(String, String)>,
    /// Not bound, and why. `(symbol, reason)`.
    pub skipped: Vec<(String, String)>,
    /// Set when declarations came from headers the named one includes rather
    /// than from the header itself — see [`bindable`].
    pub swept: Option<Swept>,
}

/// Declarations taken from headers the named one includes, and whether the
/// named header had any of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    /// How many exported declarations came from the included headers.
    pub n: usize,
    /// The named header declared no functions at all, so *every* bound symbol
    /// came from an include.
    pub umbrella: bool,
}

impl Binding {
    /// A short account of what happened, for the user to read before trusting
    /// the table that was just written into their manifest.
    pub fn report(&self) -> String {
        let mut s = format!(
            "{} bound, {} assumed, {} skipped",
            self.symbols.len(),
            self.assumed.len(),
            self.skipped.len()
        );
        if !self.structs.is_empty() {
            s.push_str(&format!("; {} struct(s)", self.structs.len()));
        }
        match self.swept {
            Some(Swept { n, umbrella: true }) => s.push_str(&format!(
                "\n\nthat header declares nothing itself, so the {n} declarations it includes \
                 that\nthe library also exports were bound instead."
            )),
            Some(Swept { n, umbrella: false }) => s.push_str(&format!(
                "\n\n{n} of these are declared in headers that one includes, and are bound \
                 because\nthe library exports them."
            )),
            None => {}
        }
        if !self.assumed.is_empty() {
            // Grouped by reason, like the skips below. Every out-scalar carries
            // the same in/out caveat, so a library with thirty of them printed
            // thirty copies of one sentence — which is how a section meant to
            // be read teaches people to skip it.
            let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (sym, why) in &self.assumed {
                by_reason.entry(why).or_default().push(sym);
            }
            s.push_str("\n\nassumed (check these):");
            for (why, syms) in by_reason {
                s.push_str(&format!("\n  {} — {why}", syms.len()));
                let shown: Vec<&str> = syms.iter().take(6).copied().collect();
                s.push_str(&format!("\n      {}", shown.join(", ")));
                if syms.len() > shown.len() {
                    s.push_str(&format!(", and {} more", syms.len() - shown.len()));
                }
            }
        }
        if !self.skipped.is_empty() {
            // Grouped by reason: a hundred symbols skipped for the same cause
            // is one fact, and printing it a hundred times hides the others.
            let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (sym, why) in &self.skipped {
                by_reason.entry(why).or_default().push(sym);
            }
            s.push_str("\n\nskipped:");
            for (why, syms) in by_reason {
                s.push_str(&format!("\n  {} — {why}", syms.len()));
                let shown: Vec<&str> = syms.iter().take(6).copied().collect();
                s.push_str(&format!("\n      {}", shown.join(", ")));
                if syms.len() > shown.len() {
                    s.push_str(&format!(", and {} more", syms.len() - shown.len()));
                }
            }
        }
        s
    }
}

// ── clang ────────────────────────────────────────────────────────────────────

/// The directories to search for the headers a header includes.
///
/// A header is rarely self-contained, and the two ways it reaches its
/// neighbours need two different directories. Both were missing, and each one
/// cost a library outright:
///
/// - `libfdt.h` does `#include <libfdt_env.h>`, which sits *beside* it. An
///   angled include does not search the including file's own directory, so the
///   header's directory has to be passed explicitly.
/// - `brotli/encode.h` does `#include <brotli/port.h>`, which resolves against
///   the directory *above* the header. Without the parent, the grandparent of
///   the included file, that fails too.
///
/// The second is why the parent is here as well as the header's own directory;
/// on its own it would look like superstition. Both are verified against the
/// real headers by the tests.
///
/// Order matters: a directory the caller named explicitly wins over one guessed
/// from the path, since adding a wide root like `/opt/homebrew/include` can
/// otherwise shadow the header the caller meant. Absolute, for the same reason
/// `cli::pkg::header_locations` is — the shim is compiled somewhere else
/// entirely, and these are replayed as its `-I` flags.
pub fn include_roots(header: &Path, extra: &[String]) -> Vec<String> {
    let abs = |p: &Path| -> String {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
    };

    let mut out: Vec<String> = Vec::new();
    let mut push = |d: String| {
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    };

    for d in extra {
        push(abs(Path::new(d)));
    }
    let own = header.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = own {
        push(abs(dir));
        if let Some(up) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            push(abs(up));
        }
    } else {
        // A bare filename: the header is in the working directory.
        push(abs(Path::new(".")));
    }
    out
}

/// Run clang over `header` and return the translation unit as JSON.
fn ast_of(header: &Path, include_dirs: &[String]) -> Result<Value, String> {
    let mut cmd = std::process::Command::new("clang");
    cmd.args(["-Xclang", "-ast-dump=json", "-fsyntax-only"]);
    for inc in include_dirs {
        cmd.arg(format!("-I{inc}"));
    }
    cmd.arg(header);

    let out = cmd.output().map_err(|e| {
        format!(
            "cannot run clang ({e}) — `jade pkg bind` reads the header with clang's parser \
             rather than guessing at C syntax. Install clang, or write the \
             [dependencies.<name>.symbols] table by hand."
        )
    })?;

    // clang exits non-zero on a header it cannot parse, but still prints the
    // AST for what it did parse. A missing include is the common cause and the
    // resulting binding would be silently incomplete, so it is an error here.
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let hint = if stderr.contains("file not found") {
            "\n  A header it includes was not found. Pass its directory with -I."
        } else {
            ""
        };
        return Err(format!("clang could not parse {}{hint}\n{}", header.display(), stderr.trim()));
    }

    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "could not read clang's JSON AST ({e}). This is the one part of `jade pkg bind` that \
             depends on a clang-internal format; if your clang is much newer than this Jade, \
             writing the symbols table by hand is the workaround."
        )
    })
}

// ── Type environment ─────────────────────────────────────────────────────────

/// What the header says about the struct types it declares.
#[derive(Default)]
struct TypeEnv {
    /// Type name → its fields, for structs with a complete definition.
    complete: HashMap<String, Vec<(String, String)>>,
    /// Type names declared but never defined — the opaque-handle idiom,
    /// `typedef struct sqlite3 sqlite3;`.
    opaque: std::collections::HashSet<String>,
    /// Typedefs of anything that is *not* a struct, so a chain can be followed
    /// to the type underneath: `sqlite3_int64` → `sqlite_int64` → `long long`.
    ///
    /// Without this a library that names its own integer types — which is most
    /// of them — looks unbindable. On SQLite alone it accounts for twenty
    /// symbols, `sqlite3_last_insert_rowid` among them.
    ///
    /// Struct typedefs are deliberately excluded: their *name* is what a handle
    /// carries, so resolving `sqlite3` to `struct sqlite3` would throw away the
    /// thing being kept.
    alias: HashMap<String, String>,
    /// Names reachable **only** through a tag, mapped to the keyword that
    /// introduces them (`struct` or `union`).
    ///
    /// `normalize` strips those keywords, which is right for looking a type up
    /// and wrong for writing one down. `struct Ctx_s` and `Ctx_s` are the same
    /// type to compare and are not interchangeable in C source: the bare name
    /// is only a type if some typedef made it one. Since the recorded name goes
    /// straight into the generated shim, the two uses need different spellings,
    /// and this is what tells them apart.
    tagged: HashMap<String, String>,
}

impl TypeEnv {
    /// How to write `n` in C source.
    ///
    /// Every name reaching a `handle<>`, `out_handle:` or `out_struct:` spec has
    /// been through [`normalize`], which drops `struct`/`union`/`enum` so a
    /// lookup does not have to care how the type was spelled. That is the right
    /// key and the wrong source text: `struct Ctx_s *` normalizes to `Ctx_s`,
    /// and `Ctx_s` on its own is not a type unless a typedef made it one.
    ///
    /// The generated shim is C, so it needs the keyword back. Libraries that
    /// name the tag and the typedef alike — `typedef struct sqlite3 sqlite3;` —
    /// never noticed, which is why this was invisible until a library used the
    /// far more common `typedef struct X_s X;` shape.
    fn c_name(&self, n: &str) -> String {
        match self.tagged.get(n) {
            Some(kw) => format!("{kw} {n}"),
            None => n.to_string(),
        }
    }

    /// Follow typedef chains to the spelling underneath, keeping `const`.
    ///
    /// Const has to survive because it is what distinguishes an input string
    /// from a writable buffer, and a typedef can introduce it: SQLite's
    /// `sqlite3_filename` *is* `const char *`, so a parameter written without
    /// `const` is still one.
    fn expand(&self, t: &str) -> String {
        let mut s = t.trim().to_string();
        // A cycle cannot occur in valid C, but a malformed AST must not hang.
        for _ in 0..16 {
            let key = normalize(&s);
            if let Some(u) = self.alias.get(&key) {
                let keep_const = s.contains("const") && !u.contains("const");
                let next = if keep_const { format!("const {u}") } else { u.clone() };
                if normalize(&next) == key {
                    break;
                }
                s = next;
                continue;
            }
            if let Some(u) = pointee(&key).and_then(|inner| self.alias.get(inner)) {
                // `const` has to be carried across here as well as in the branch
                // above. `normalize` strips it to make the lookup key, so
                // rebuilding from the key alone turned `const uint8_t *` into
                // `unsigned char *` — and a read-only input buffer read as a
                // writable one is scratch the shim allocates and the caller's
                // data never reaches the library.
                let next = if s.contains("const") && !u.contains("const") {
                    format!("const {u}*")
                } else {
                    format!("{u}*")
                };
                if normalize(&next) == key {
                    break;
                }
                s = next;
                continue;
            }
            break;
        }
        s
    }
}

/// Follow a `TypedefDecl` to the `RecordDecl` it names, if it names one.
///
/// The kind check matters. A typedef of another typedef — `typedef sqlite_int64
/// sqlite3_int64;` — also has a `decl` in the same position, pointing at a
/// `TypedefDecl`. Returning that id made the caller look for a record, not find
/// one, and drop the typedef entirely instead of recording it as an alias; the
/// symptom was every `sqlite3_int64`-returning function reported unbindable.
fn typedef_record_id(node: &Value) -> Option<&str> {
    let inner = node.get("inner")?.as_array()?.first()?;
    let is_record = |d: &Value| d.get("kind").and_then(Value::as_str) == Some("RecordDecl");

    if let Some(d) = inner.get("ownedTagDecl").filter(|d| is_record(d)) {
        return d.get("id")?.as_str();
    }
    let d = inner.get("inner")?.as_array()?.first()?.get("decl")?;
    if is_record(d) { d.get("id")?.as_str() } else { None }
}

/// Whether a typedef names an enum.
///
/// Both spellings clang gives have to be checked. For `typedef enum { ... }
/// lzma_ret;` the `qualType` is `enum lzma_ret` and the `desugaredQualType` is
/// the bare `lzma_ret` — and [`underlying`] prefers the desugared one, which is
/// exactly the spelling with the keyword already gone.
fn typedef_names_enum(node: &Value) -> bool {
    let names_enum =
        |s: &str| s.trim().trim_start_matches("const ").trim_start().starts_with("enum ");
    let Some(ty) = node.get("type") else { return false };
    ["qualType", "desugaredQualType"]
        .iter()
        .filter_map(|k| ty.get(k).and_then(Value::as_str))
        .any(names_enum)
}

fn build_env(nodes: &[&Value]) -> TypeEnv {
    let mut env = TypeEnv::default();

    // An enum is an integer, and the FFI carries integers. Recording that here
    // rather than in the mapper means it works everywhere a type is looked up —
    // return, parameter, struct field — because they all resolve through
    // `expand`. Libraries lean on enums heavily for status codes, so leaving
    // them unbindable cost more than any other single gap: on liblzma alone it
    // was 60 of 114 symbols, `lzma_code` among them.
    for n in nodes {
        if n.get("kind").and_then(Value::as_str) != Some("EnumDecl") {
            continue;
        }
        if let Some(name) = n.get("name").and_then(Value::as_str) {
            env.alias.insert(name.to_string(), "int".to_string());
        }
    }

    // Records first, by id, so a typedef can be resolved in one pass afterwards.
    let mut by_id: HashMap<&str, (&Value, bool)> = HashMap::new();
    for n in nodes {
        if n.get("kind").and_then(Value::as_str) != Some("RecordDecl") {
            continue;
        }
        let Some(id) = n.get("id").and_then(Value::as_str) else { continue };
        let complete = n.get("completeDefinition").and_then(Value::as_bool).unwrap_or(false);
        by_id.insert(id, (n, complete));
        // A named `struct X { ... }` is usable as `struct X` directly — and, so
        // far, only that way. A typedef giving it a bare name is a separate
        // declaration, handled in the pass below.
        if let Some(name) = n.get("name").and_then(Value::as_str) {
            if complete {
                env.complete.insert(name.to_string(), fields_of(n));
            } else {
                env.opaque.insert(name.to_string());
            }
            let kw = n.get("tagUsed").and_then(Value::as_str).unwrap_or("struct");
            env.tagged.insert(name.to_string(), kw.to_string());
        }
    }

    // `typedef struct { ... } SF_INFO;` — the record is anonymous and the
    // typedef is what names it, so the fields have to be reached through the
    // link rather than by position.
    for n in nodes {
        if n.get("kind").and_then(Value::as_str) != Some("TypedefDecl") {
            continue;
        }
        let Some(name) = n.get("name").and_then(Value::as_str) else { continue };
        let Some(rid) = typedef_record_id(n) else {
            // Not a struct: an alias for something else. `type.qualType` on a
            // TypedefDecl is the type underneath, which may itself be a typedef.
            if let Some(under) = underlying(n) {
                // `typedef enum lzma_ret lzma_ret;` would otherwise alias the
                // name to itself once the keyword is stripped, and resolve to
                // nothing. An enum is an int wherever it appears.
                let target = if typedef_names_enum(n) { "int" } else { under };
                env.alias.insert(name.to_string(), target.to_string());
            }
            continue;
        };
        // A record id that names no record we collected: still an alias, not a
        // typedef to drop on the floor.
        let Some(&(rec, complete)) = by_id.get(rid) else {
            if let Some(under) = underlying(n) {
                // `typedef enum lzma_ret lzma_ret;` would otherwise alias the
                // name to itself once the keyword is stripped, and resolve to
                // nothing. An enum is an int wherever it appears.
                let target = if typedef_names_enum(n) { "int" } else { under };
                env.alias.insert(name.to_string(), target.to_string());
            }
            continue;
        };
        if complete {
            env.complete.insert(name.to_string(), fields_of(rec));
        } else {
            env.opaque.insert(name.to_string());
        }
        // `typedef struct sqlite3 sqlite3;` — the tag and the typedef share a
        // name, and the bare one is now a type. Drop the tag requirement, which
        // the record pass added before this declaration was seen.
        env.tagged.remove(name);
    }

    env
}

fn fields_of(rec: &Value) -> Vec<(String, String)> {
    rec.get("inner")
        .and_then(Value::as_array)
        .map(|fs| {
            fs.iter()
                .filter(|f| f.get("kind").and_then(Value::as_str) == Some("FieldDecl"))
                .filter_map(|f| {
                    Some((f.get("name")?.as_str()?.to_string(), qual_type(f)?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn qual_type(node: &Value) -> Option<&str> {
    node.get("type")?.get("qualType")?.as_str()
}

/// What a typedef stands for, with clang's own chain resolution when it offers
/// it.
///
/// `desugaredQualType` is present exactly when the written type is not already
/// the underlying one, and it collapses the whole chain in a single step:
/// `sqlite3_int64` → `sqlite_int64` → `long long` arrives as `long long`.
/// Following the chain by hand would work too, and does as the fallback, but
/// taking clang's answer avoids re-deriving something it already computed.
fn underlying(node: &Value) -> Option<&str> {
    let ty = node.get("type")?;
    ty.get("desugaredQualType")
        .and_then(Value::as_str)
        .or_else(|| ty.get("qualType").and_then(Value::as_str))
}

// ── C type → Jade FFI spelling ───────────────────────────────────────────────

/// Strip `const`/`volatile`/`struct`/`enum` and collapse whitespace, so the
/// matcher below sees one spelling per type rather than six.
fn normalize(t: &str) -> String {
    let mut s = t.trim().to_string();
    for kw in ["const ", "volatile ", "restrict ", "struct ", "enum ", "union "] {
        while let Some(i) = s.find(kw) {
            s.replace_range(i..i + kw.len(), "");
        }
    }
    // `char *` and `char*` are the same type.
    let compact: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.replace(" *", "*").trim().to_string()
}

/// Drop every space, so `unsigned char` and `unsignedchar` compare equal.
///
/// Multi-word C type names are the reason this exists: normalizing keeps the
/// space (it is load-bearing in `unsigned char`), so every comparison against a
/// fixed spelling has to squash first. Doing it in one place is what keeps
/// `is_int` and the buffer check from disagreeing about `unsigned char *` —
/// which they did, and the symptom was `crc` reported as unbindable.
fn squash(t: &str) -> String {
    t.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Every C spelling that reads as a Jade `int`.
///
/// Width is lost on purpose: the FFI has one integer type, and a `short`
/// widening to 64 bits is exact. The reverse — a Jade int narrowing into a
/// `short` parameter — is the C compiler's ordinary implicit conversion, which
/// is what a hand-written binding would have done too.
const INT_TYPES: &[&str] = &[
    "char",
    "signedchar",
    "unsignedchar",
    "short",
    "shortint",
    "unsignedshort",
    "unsignedshortint",
    "int",
    "unsigned",
    "unsignedint",
    "long",
    "longint",
    "unsignedlong",
    "unsignedlongint",
    "longlong",
    "longlongint",
    "unsignedlonglong",
    "unsignedlonglongint",
    "size_t",
    "ssize_t",
    "ptrdiff_t",
    "intptr_t",
    "uintptr_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "off_t",
    "time_t",
    "mode_t",
    "pid_t",
    "wchar_t",
];

fn is_int(t: &str) -> bool {
    INT_TYPES.contains(&squash(t).as_str())
}

fn is_float(t: &str) -> bool {
    matches!(t.trim(), "float" | "double" | "long double")
}

fn is_bool(t: &str) -> bool {
    matches!(t.trim(), "_Bool" | "bool")
}

/// A scalar Jade type, or `None` if this is not one.
fn scalar_of(t: &str) -> Option<&'static str> {
    if is_bool(t) {
        Some("bool")
    } else if is_int(t) {
        Some("int")
    } else if is_float(t) {
        Some("float")
    } else {
        None
    }
}

/// The pointee of a single-pointer type, or `None`.
fn pointee(t: &str) -> Option<&str> {
    let s = t.strip_suffix('*')?;
    if s.ends_with('*') { None } else { Some(s) }
}

/// The pointee of a double-pointer type (`sqlite3**`), or `None`.
fn pointee2(t: &str) -> Option<&str> {
    let s = t.strip_suffix('*')?.strip_suffix('*')?;
    if s.ends_with('*') { None } else { Some(s) }
}

fn is_fn_ptr(t: &str) -> bool {
    t.contains("(*)") || t.contains("(^)")
}

/// What one parameter maps to.
enum Mapped {
    /// A plain Jade argument.
    One(String),
    /// A plain Jade argument decided on an inference worth checking, plus what
    /// was inferred. `Out` already carries one of these; a parameter that is not
    /// an out-parameter needed somewhere to put it too.
    Assumed(String, String),
    /// A `handle<T>` for a struct the *caller* allocates. Distinct from `One`
    /// only in that the struct's table has to be written out with `held = true`,
    /// which is what makes the generator emit `<T>_new` and the rest.
    Held(String),
    /// A `bytes` argument that also swallows the following length parameter.
    BytesPair(String),
    /// An out-parameter, plus what was assumed to decide it (if anything).
    Out(String, Option<String>),
    /// Not representable; the string says why.
    Reject(String),
}

/// Map one parameter of a function, given what follows it.
///
/// `next` matters because C encodes two-parameter idioms positionally: a
/// pointer followed by a length is a buffer, and the pair has to be recognised
/// together or not at all.
/// Words that appear in the name of a number counting something, taken from
/// every such parameter across the survey headers rather than invented:
/// `srcSize`, `dstCapacity`, `namelen`, `buflen`, `props_size`, `in_size`.
const COUNT_WORDS: [&str; 5] = ["len", "size", "capacity", "count", "num"];

/// Words that appear in the name of a number saying *where*, which is the shape
/// a count is most often confused with. `nodeoffset` is the single most common
/// name to follow a byte pointer in these headers, and it starts with the same
/// `n` that `nbytes` does.
const POSITION_WORDS: [&str; 4] = ["offset", "index", "idx", "pos"];

/// Whether a parameter's *name* says it counts what came before it.
///
/// Consulted for two questions the types cannot answer on their own: whether the
/// pointer before it is a buffer of this length, and whether it is an array of
/// this many structs. `fdt_getprop(const void *fdt, int nodeoffset, …)` takes a
/// blob and a position; `cs_op_count(csh, const cs_insn *insn, unsigned
/// op_type)` takes one struct and a flag; `ares_process_fds(ch, const
/// ares_fd_events_t *events, size_t nevents)` really does take an array.
///
/// The `n`-plus-noun convention is why a leading `n` counts, and `nodeoffset` is
/// why saying *where* takes it back. Both halves are drawn from the names that
/// actually appear in these headers.
///
/// A header that names nothing keeps the old behaviour, since there is no
/// evidence either way and the pairing was the standing assumption.
/// Whether a parameter's *name* says it holds a position rather than a thing.
///
/// Asked of the pointer this time, not of the integer after it. `size_t *in_pos,
/// size_t in_size` has exactly the shape of a buffer and its count, and is a
/// position beside an unrelated size — reading it as a buffer allocated
/// `in_size` of them and handed the library the wrong pointer entirely. `short
/// *buf, int n` is the shape that has to survive it.
fn names_a_position(name: Option<&str>) -> bool {
    let Some(n) = name else { return false };
    let n = n.to_ascii_lowercase();
    POSITION_WORDS.iter().any(|w| n.contains(w))
}

fn names_a_count(name: Option<&str>) -> bool {
    let Some(n) = name else { return true };
    let n = n.to_ascii_lowercase();
    let counts = n.starts_with('n') || COUNT_WORDS.iter().any(|w| n.contains(w));
    counts && !POSITION_WORDS.iter().any(|w| n.contains(w))
}

/// Whether a parameter's *name* says it holds a length.
///
/// Narrower than [`names_a_count`], and used for a different question: which
/// parameter sizes a returned pointer. Nothing in the types tells `int *lenp`
/// apart from the second value a call happens to write back, so a name that
/// does not say "length" leaves the symbol refused rather than sizing a blob
/// from an unrelated number.
fn names_a_length(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("len") || n.contains("size")
}

/// What every parameter of one function is decided against.
///
/// Grouped rather than passed one by one because it is genuinely per-function:
/// the same environment, the same return type, the same arity, for every
/// parameter in the list. Only `next` and its name change between calls.
struct FnCtx<'a> {
    env: &'a TypeEnv,
    /// The function's *raw* return type. An out-buffer is only an out-buffer if
    /// the call reports how much it filled.
    ret: &'a str,
    /// Struct types the library hands out, which are handles rather than
    /// anything the caller builds.
    produced: &'a std::collections::HashSet<String>,
    /// How many functions take each struct type, which is half of what
    /// identifies caller-held state.
    counts: &'a HashMap<String, usize>,
    /// How many parameters the function has. A lone `void *` is a deallocator.
    n_params: usize,
}

fn map_param(
    raw_in: &str,
    prev: Option<&str>,
    own_name: Option<&str>,
    next: Option<&str>,
    next_name: Option<&str>,
    cx: &FnCtx<'_>,
) -> Mapped {
    let FnCtx { env, ret, produced, counts, n_params } = *cx;
    // Expanded before anything is decided, so a library's own typedefs read as
    // the types they stand for. `const` comes from the expansion, not the
    // written parameter, because a typedef can introduce it.
    let raw = env.expand(raw_in);
    let raw = raw.as_str();
    let is_const = raw.contains("const");
    let t = normalize(raw);

    if is_fn_ptr(raw) {
        return match callback_spec(raw, env) {
            // A stored callback works now — the registration outlives the call
            // and the Jade function is kept alive by `native::CallbackBus`. What
            // is still assumed is *which* function an answer belongs to: the
            // shim keeps one slot per symbol, so two outstanding registrations
            // collide unless the library offers a context parameter to route
            // through. Reported rather than guessed, because filling that slot
            // means the library must hand back exactly what it was given.
            Some(spec) => Mapped::Assumed(
                spec,
                concat!(
                    "takes a callback. The library may store it and call back later, which ",
                    "works — but the shim keeps one registration per symbol, so calling ",
                    "this twice with different functions sends both answers to the ",
                    "second. If the library has a context parameter beside the callback, ",
                    "write `callback_data` for it instead of `null_ptr` and each gets its own"
                )
                .to_string(),
            ),
            None => Mapped::Reject(
                "takes a callback whose own signature the FFI cannot carry. If the library \
                 accepts a null pointer there — brotli's allocator hooks do, and fall back on \
                 malloc without one — write `null_ptr` for it in jade.toml"
                    .to_string(),
            ),
        };
    }
    // The `void *` that follows a callback is the context C has instead of
    // closures: the library stores it and hands it back to the callback. A Jade
    // function already carries its own environment, so there is nothing to put
    // there — the shim passes null, and it is not a Jade argument at all.
    //
    // Decided by position rather than by name, because the position is the
    // convention: `ares_set_socket_callback(ch, cb, void *data)`,
    // `BrotliDecoderSetMetadataCallbacks(state, start, chunk, void *opaque)`.
    if squash(&t) == "void*" && prev.is_some_and(|p| is_fn_ptr(&env.expand(p))) {
        return Mapped::One("null_ptr".to_string());
    }

    if let Some(s) = scalar_of(&t) {
        return Mapped::One(s.to_string());
    }

    // `sqlite3**` — a handle written through a pointer.
    if let Some(inner) = pointee2(&t) {
        if env.opaque.contains(inner) || env.complete.contains_key(inner) {
            return Mapped::Out(format!("out_handle:{}", env.c_name(inner)), None);
        }
        // A name pointed at rather than allocated. `fdt_getprop_by_offset(const
        // void *fdt, int off, const char **namep, int *lenp)` points `namep`
        // into the device tree it was handed, so nothing was allocated and
        // nothing has to be released — which is the whole difference between
        // this and the pointers a library mallocs for you.
        //
        // `const` is what says which of the two it is.
        if is_const && squash(inner) == "char" {
            return Mapped::Out(format!("out_str:{inner}"), None);
        }
        // And a writable `char **` is the allocating shape, with the same C.
        // Which it is, and who then releases it, are things the header does not
        // record — so the spelling is named rather than guessed. Guessing one
        // way leaks on every call and the other frees memory that was never
        // allocated.
        if squash(inner) == "char" {
            return Mapped::Reject(
                "hands back a string through a `char **`, and the header does not say whether the \
                 caller then owns it. If they do, write `out_alloc_str:char` for it in jade.toml, \
                 with `frees_with` naming the library's own free function"
                    .to_string(),
            );
        }
        return Mapped::Reject(
            "takes a pointer to a pointer, which is how a library hands back memory it \
             allocated. Who releases it is not something the header says"
                .to_string(),
        );
    }

    let Some(inner) = pointee(&t) else {
        // A struct passed by value.
        if env.complete.contains_key(&t) {
            return Mapped::Reject("takes a struct by value".to_string());
        }
        return Mapped::Reject(format!("takes an unsupported type `{raw}`"));
    };

    // `const char *` is text, whatever follows it.
    //
    // Not conditional on the next parameter, deliberately. Reading a following
    // int as a length is right for `(const void*, size_t)` and wrong for
    // `sf_open(const char* path, int mode, ...)`, where it turned the path into
    // a blob and swallowed the mode. Plain `char` is the C convention for text
    // and `void`/`unsigned char` for bytes, so the element type decides and the
    // position does not. A genuine text-plus-length pair such as
    // `sqlite3_bind_text` still works: the length stays an ordinary argument.
    if squash(inner) == "char" && is_const {
        return Mapped::One("str".to_string());
    }

    // A pointer next to a length is a buffer, and `const` says which direction.
    //
    // "Next to a length" is decided by the parameter's *name* as well as its
    // type, because plenty of pointers are followed by an int that counts
    // nothing: `fdt_getprop(const void *fdt, int nodeoffset, …)` takes a blob
    // and an offset, and `cs_op_count(csh, const cs_insn *insn, unsigned
    // op_type)` takes one struct and a flag.
    //
    // The two mistakes are not symmetric, which is what makes the name worth
    // trusting. Reading a real length as an ordinary argument costs nothing —
    // the int is still passed, the caller just supplies it — while reading an
    // offset as a length *drops* it, and hands the library a size it never
    // computed. A header that names no parameters keeps the old behaviour.
    let next_is_int = next.map(|n| is_int(&normalize(&env.expand(n)))).unwrap_or(false);
    let next_is_len = next_is_int && names_a_count(next_name);
    let squashed = squash(inner);
    let byte_like = matches!(
        squashed.as_str(),
        "void" | "char" | "unsignedchar" | "signedchar" | "uint8_t" | "int8_t"
    );

    if next_is_len
        && !is_const
        && (byte_like || scalar_of(&squashed).is_some())
        && !names_a_position(own_name)
    {
        // Writable, so almost certainly filled by the call — but "almost" is
        // why this is reported as an assumption rather than done silently.
        if is_int(&normalize(&env.expand(ret))) {
            // Everything here is in *elements*: the shim allocates
            // `count * sizeof(elem)` and sizes the result by what the call
            // reports, so a `short*` buffer works exactly as a `char*` one does.
            let elem = if squashed == "void" { "char" } else { inner };
            return Mapped::Out(
                format!("out_buffer:{elem}"),
                Some(format!(
                    "`{raw}` next to a length was read as a buffer the call fills; if the library \
                     reads it instead, change it to `bytes`"
                )),
            );
        }
        return Mapped::Reject(
            "takes a writable buffer, but does not return a count for it".to_string(),
        );
    }

    if next_is_len && is_const {
        // Only a byte-shaped input becomes `bytes`. A blob's length is in bytes
        // and a typed buffer's count is in elements, so handing a `const short*`
        // a byte count would tell the library twice as many shorts as there
        // are. Saying so beats a binding that reads past the end.
        if byte_like {
            return Mapped::BytesPair("bytes".to_string());
        }
        // `const char *` already returned above as text.
        return Mapped::Reject(format!(
            "takes a `{raw}` input buffer, whose length is counted in elements rather than bytes"
        ));
    }

    // A writable pointer to a bare scalar, with no length beside it: the C way
    // of returning a second value. `lzma_get_progress(strm, uint64_t*,
    // uint64_t*)` and `fdt_next_tag(fdt, offset, int *nextoffset)` are both
    // this, and there are a lot of them.
    //
    // Reported as an assumption rather than done silently, exactly as the
    // buffer guess above is. Some of these are read *and* written — a position
    // the caller sets and the library advances, `size_t *out_pos` — and a
    // zeroed local is right for one call and wrong on the second. That is the
    // kind of wrong that shows up as corrupt output rather than as an error, so
    // the note names the spelling that fixes it.
    // Reaching here with a count beside it means the buffer reading above was
    // declined — the pointer's own name said it holds a position — so the count
    // belongs to something else and this is one value written back.
    if !is_const && scalar_of(&squashed).is_some() {
        // Except a byte. `uint8_t *out` with nothing beside it is overwhelmingly
        // a buffer whose size the caller is expected to know from the
        // documentation — `lzma_stream_footer_encode` writes exactly twelve —
        // and treating it as one out-parameter would declare a one-byte local
        // and hand the library its address. That is a stack overflow the
        // compiler cannot see and the report would call an assumption.
        // A byte pointer alone is a buffer whose extent only the documentation
        // gives — `lzma_stream_header_encode` writes exactly twelve — so the
        // caller states it. That is what the C underneath required of them
        // anyway, and the alternative is that the symbol cannot be called.
        //
        // Reading one of these as a single value written back is what it used to
        // do, and that declared a one-byte local and handed the library its
        // address.
        if byte_like {
            return Mapped::Out(
                format!("sized_buffer:{inner}"),
                Some(format!(
                    "`{raw}` has no length beside it, so how much the call writes is yours to \
                     say: the binding takes the count as its own argument and hands the whole \
                     buffer back. Passing less than the library writes corrupts memory"
                )),
            );
        }
        return Mapped::Out(
            format!("out_scalar:{inner}"),
            Some(format!(
                "`{raw}` was read as a value the call writes; if the library reads it first and \
                 advances it, change it to `inout_scalar:{inner}`"
            )),
        );
    }

    // An opaque pointer: exactly what a handle is for.
    if env.opaque.contains(inner) {
        return Mapped::One(format!("handle<{}>", env.c_name(inner)));
    }

    // A pointer to a struct the header defines. Three different things wear this
    // shape, and treating them all as out-parameters is how `lzma_code` came to
    // bind into a shim that ran and did nothing.
    if env.complete.contains_key(inner) {
        // Read-only, so the library takes what the struct says and forgets it.
        // Jade builds one, the shim copies it into a real C local and passes its
        // address — no ownership crosses the boundary in either direction.
        //
        // Only when *every* field can make the trip. Dropping one would hand the
        // library the zero the local was memset to, in a position where the
        // caller believed they had set it; unlike a dropped output, nothing
        // afterwards shows that it went missing.
        if is_const {
            let fields = env.complete.get(inner).expect("checked above");
            // Unless a field would go missing. Then the caller cannot build a
            // complete one, so they hold one instead: allocated on the C heap,
            // reached through a handle, and filled by whichever library calls
            // know how to fill it.
            if struct_loses_a_field(fields, env) {
                return Mapped::Held(format!("handle<{}>", env.c_name(inner)));
            }
            return Mapped::One(format!("in_struct:{}", env.c_name(inner)));
        }

        // The library allocates it, so Jade should hold it rather than build
        // one. This is the same answer `map_ret` already gives the type in
        // return position.
        if produced.contains(inner) {
            return Mapped::One(format!("handle<{}>", env.c_name(inner)));
        }

        // Caller-held state. Two signals, and both are needed.
        //
        // An `out_struct` shim declares a *zeroed local* every call and reads
        // the carryable fields back out. That is right for a record one call
        // fills. It is wrong for a struct the caller threads through a sequence
        // of calls, because the fields the FFI cannot carry — the pointers a
        // codec keeps its position in — are dropped, and the next call gets a
        // fresh zeroed struct instead of the state it left.
        //
        // Losing a field alone is not enough: a record with one `void*` in it
        // is still a record, and dropping that field is the documented
        // behaviour. Appearing in several functions alone is not enough either:
        // libsndfile's `SF_INFO` is passed to three `sf_open` variants and is
        // exactly what out-parameters exist for. Together they identify a
        // struct that is both threaded and unrepresentable, which is the
        // combination that cannot work.
        // Which is a struct Jade holds rather than one it builds. The
        // allocation happens once, on the C heap, and every call is handed the
        // same pointer — so the fields the FFI cannot carry stay exactly where
        // the library put them, which is the whole difficulty.
        let threaded = counts.get(inner).copied().unwrap_or(0) > 1;
        let lossy = env.complete.get(inner).is_some_and(|f| struct_loses_a_field(f, env));
        if threaded && lossy {
            return Mapped::Held(format!("handle<{}>", env.c_name(inner)));
        }

        return Mapped::Out(format!("out_struct:{}", env.c_name(inner)), None);
    }

    // A read-only byte pointer with no count beside it. Some libraries take a
    // blob whose extent is written *inside* it — every `libfdt` call takes
    // `const void *fdt` alone and reads the length out of the header — and
    // others take one of a size the documentation fixes, like an IPv6 address.
    // There is nowhere to pass a length, so refusing the shape refused most of
    // libfdt.
    //
    // Borrowed for the call, exactly as a `str` is. Reported as an assumption
    // because Jade cannot check the extent: the library takes it from the data,
    // and a truncated blob reads past the end.
    if is_const && byte_like {
        return Mapped::Assumed(
            "bytes_ptr".to_string(),
            format!(
                "`{raw}` with no length beside it was read as a borrowed blob; the library takes \
                 its extent from the data itself, so a truncated one reads past the end"
            ),
        );
    }

    // A writable byte pointer with no count beside it, where the *whole* shape
    // is a buffer the library revises in place. Every `libfdt` writer takes
    // `void *fdt` and edits the device tree where it sits.
    //
    // Distinct from the `out_buffer` case above, which allocates scratch the
    // caller never filled: here the caller's own bytes are the starting point,
    // so they are copied in and the edited copy comes back. Distinct from
    // `bytes_ptr` too, because a Jade blob is immutable and cannot be lent out
    // to be scribbled on.
    if byte_like {
        // A bare `void *` on its own is a deallocator, not a buffer: c-ares
        // spells `ares_free_string(void *str)` and `ares_free_data(void *)` that
        // way. Handing one of those the shim's own scratch would have the
        // library free it, and the shim free it again on the way out. Nothing in
        // the type says which of the two it is, so the one-argument shape is
        // refused and the multi-argument one — every `libfdt` writer, which
        // takes the tree plus what to do to it — is not.
        if squashed == "void" && n_params == 1 && normalize(&env.expand(ret)) == "void" {
            return Mapped::Reject(
                "takes a `void *` on its own and reports nothing, which is the shape of a call \
                 that frees what it is given. Passing it a buffer would leave the shim freeing \
                 memory the library already released"
                    .to_string(),
            );
        }
        return Mapped::Assumed(
            "inout_bytes".to_string(),
            format!(
                "`{raw}` with no length beside it was read as a buffer the call revises in place, \
                 so the edited copy comes back; if the library only reads it, change it to \
                 `bytes_ptr`"
            ),
        );
    }

    Mapped::Reject(format!("takes an unsupported type `{raw}`"))
}

/// Turn a C function-pointer type into a `callback:` spelling, or `None` when
/// its own signature is not one the FFI can carry.
///
/// The signature is kept in the library's **C** types rather than translated to
/// Jade's. The shim declares a function pointer the library will store and
/// call, so `int` has to stay `int`: widening it to Jade's 64-bit integer is
/// not a truncation but an incompatible function pointer, and a call through
/// the wrong ABI.
fn callback_spec(raw: &str, env: &TypeEnv) -> Option<String> {
    // clang spells these `ret (*)(params)`.
    let expanded = env.expand(raw);
    let open = expanded.find("(*)")?;
    let ret = expanded[..open].trim().to_string();
    let rest = expanded[open + 3..].trim();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();

    let params: Vec<String> = if inner.is_empty() || inner == "void" {
        Vec::new()
    } else {
        inner.split(',').map(|p| p.trim().to_string()).collect()
    };

    // Only what the trampoline can marshal, written as `category:spelling`
    // wherever the two differ.
    //
    // The spelling has to survive because the trampoline is declared with it: a
    // typedef expanded to its underlying type makes a function pointer C
    // considers incompatible with the library's, so the shim would not compile.
    // The category is what Jade marshals as, and it comes from expanding — which
    // is how `ares_socket_t` and `ares_bool_t` become carriable at all. Before
    // this, checking the expanded type and emitting the written one produced a
    // spec the generator accepted and the shim refused, and refusing on the
    // written name alone lost every callback whose signature names a typedef.
    let category = |t: &str| -> Option<&'static str> {
        let n = normalize(&env.expand(t));
        if squash(&n) == "void*" {
            return Some("void*");
        }
        if pointee(&n).map(squash).as_deref() == Some("char") {
            return Some("const char*");
        }
        if is_int(&n) {
            return Some("int");
        }
        scalar_of(&squash(&n)).map(|s| match s {
            "float" => "double",
            "bool" => "_Bool",
            _ => "int",
        })
    };

    // A pointer beside a length is one blob, the same idiom an argument list
    // uses. `ares_callback` delivers every DNS answer that way, so without it
    // c-ares can register a query and never see its result.
    let mut spec: Vec<String> = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let p = &params[i];
        let pn = normalize(&env.expand(p));
        // The user-data slot first, and that order matters: a `void *` is
        // byte-like, so `void *arg, int status` would otherwise read as a blob
        // and its length. `ares_callback` begins with exactly that.
        let byte_like = squash(&pn) != "void*"
            && pointee(&pn).map(squash).is_some_and(|e| {
                matches!(e.as_str(), "void" | "unsignedchar" | "signedchar" | "uint8_t" | "int8_t")
            });
        let next_is_len = params.get(i + 1).is_some_and(|q| is_int(&normalize(&env.expand(q))));
        if byte_like && next_is_len {
            spec.push(format!("bytes:{p}"));
            let len = &params[i + 1];
            spec.push(if squash("int") == squash(len) {
                len.clone()
            } else {
                format!("int:{len}")
            });
            i += 2;
            continue;
        }
        // Only prefixed when the two genuinely differ. `void *` and `void*` are
        // one type spelled two ways, and `void*:void *` is noise in a manifest
        // a person reads.
        let cat = category(p)?;
        spec.push(if squash(cat) == squash(p) { p.clone() } else { format!("{cat}:{p}") });
        i += 1;
    }
    let params = spec;

    let ret_cat = if normalize(&ret) == "void" { "void" } else { category(&ret)? };
    if ret_cat == "void*" || ret_cat == "const char*" {
        // A callback may only give back a scalar; a pointer would have to be
        // released inside the library's own frame.
        return None;
    }
    let ret = if ret_cat == "void" || squash(ret_cat) == squash(&ret) {
        ret
    } else {
        format!("{ret_cat}:{ret}")
    };

    Some(format!("callback:{ret}({})", params.join(", ")))
}

/// Map a return type.
fn map_ret(raw_in: &str, env: &TypeEnv) -> Result<String, String> {
    let raw = env.expand(raw_in);
    let raw = raw.as_str();
    let is_const = raw.contains("const");
    let t = normalize(raw);
    if t == "void" {
        return Ok("nil".to_string());
    }
    if is_fn_ptr(raw) {
        return Err("returns a function pointer".to_string());
    }
    if let Some(s) = scalar_of(&t) {
        return Ok(s.to_string());
    }
    if let Some(inner) = pointee(&t) {
        // A returned string has no length beside it, so NUL-terminated text is
        // the only representation available. `const` is what makes that safe to
        // assume: a *writable* `unsigned char *` is a buffer the caller owns —
        // sqlite3_serialize returns one — and reading it as text would truncate
        // it at the first zero byte.
        if matches!(squash(inner).as_str(), "char" | "unsignedchar" | "signedchar") && is_const {
            return Ok("str".to_string());
        }
        // A writable `char *` is the allocating spelling, and it is the single
        // largest thing a header refuses: 125 of glib's symbols come back this
        // way, `g_strdup` and `g_uri_escape_string` among them. Whether the
        // caller owns it is documentation rather than type — `g_basename` points
        // into its argument and `g_strdup` mallocs, and both are `gchar *` — so
        // the choice is named rather than guessed. Guessing "borrowed" leaks on
        // every call, which is what a hand-written `ret = "str"` does today, and
        // guessing "owned" frees a static string.
        if squash(inner) == "char" {
            return Err(
                "returns a string the caller may own, and the header does not say whether they \
                 do. If the library allocated it for you, write `ret = \"alloc_str\"` in \
                 jade.toml with `frees_with` naming the library's own free function. If the \
                 library keeps it, `ret = \"str\"`"
                    .to_string(),
            );
        }
        if env.opaque.contains(inner) || env.complete.contains_key(inner) {
            return Ok(format!("handle<{}>", env.c_name(inner)));
        }
    }
    // A struct returned by value. Nothing crosses the boundary but the value
    // itself — it arrives in registers or on the stack, whichever the ABI says,
    // and the shim reads the fields straight out of it. No allocation and no
    // ownership, which makes this the simplest of the struct shapes once the
    // header is there to declare the type.
    if env.complete.contains_key(&t) {
        return Ok(format!("struct:{}", env.c_name(&t)));
    }
    Err(format!("returns an unsupported type `{raw_in}`"))
}

/// The failure convention a signature implies, and what was assumed to say so.
///
/// Only the unambiguous shapes are inferred. Guessing that every `int` is a
/// status would turn a function returning a legitimate count into one that
/// raises on a perfectly good answer — which is exactly what happened to
/// `cs_disasm`, where the return is how many instructions were written and a
/// successful disassembly of three raised.
///
/// The discrimination is the *C* spelling, not the Jade one. Both collapse to
/// `int`, so by the time a return type reads as "int" the difference between
/// `int` and `size_t` is gone — and that difference is the convention: a status
/// is an `int`, a count is a `size_t`. Enums come through as `int` because
/// `build_env` aliases them, which is right: `cs_err` and `lzma_ret` are
/// statuses.
fn infer_failure(
    ret: &str,
    c_ret: &str,
    has_out_handle: bool,
) -> (Option<CFailure>, Option<String>) {
    if ret.starts_with("handle<") || ret == "str" {
        // A pointer-returning open: NULL is the universal failure.
        return (Some(CFailure::Null), None);
    }
    if has_out_handle && ret == "int" {
        if squash(c_ret) == "int" {
            return (
                Some(CFailure::Nonzero),
                Some(
                    "the handle is the result, so a non-zero return was read as a failure; if it                      is a count, drop `fails_when`"
                        .to_string(),
                ),
            );
        }
        // A wider integer beside a handle is a count far more often than a
        // status — `size_t cs_disasm(…, cs_insn **insn)` is how many were
        // written. Not inferred, and said out loud, because the reverse guess
        // is a call that raises on success.
        return (
            None,
            Some(format!(
                "`{c_ret}` beside a handle was read as a value rather than a status, so this call                  never raises; if it does report failure, add `fails_when = \"nonzero\"` or                  `\"zero\"`"
            )),
        );
    }
    (None, None)
}

// ── Driver ───────────────────────────────────────────────────────────────────

/// Which of a translation unit's declarations to bind.
///
/// Normally: the ones the named header declares itself. Everything a header
/// includes is in the same translation unit, and binding all of it would pull
/// in the whole C standard library.
///
/// Some libraries have no such header. `lzma.h`, `alsa/asoundlib.h` and
/// `git2.h` are *umbrellas* — they declare nothing and exist to include the
/// twenty headers that do. Pointing at one used to report "no declarations
/// found", and pointing at a sub-header failed differently, because a
/// sub-header on its own is usually not compilable.
///
/// So the library's export table decides instead: bind what the translation
/// unit declares *and* the artifact exports. That is an exact test rather than
/// a guess about which paths count as system ones — `fopen` is declared in this
/// translation unit and is not in liblzma, so it does not get bound. The named
/// header stays the one the shim includes, which is what it is for.
///
/// This runs whether or not the named header declares functions of its own,
/// because plenty of libraries do both. `ares.h` declares seventy-odd symbols
/// and includes `ares_dns_record.h`, which declares sixty-three more — the
/// whole modern DNS record API, invisible for as long as the rule was
/// all-or-nothing. Its own declarations are kept either way, so no library
/// loses a symbol by this being additive.
///
/// Without an export table there is nothing exact to test against, so only the
/// header's own declarations are bound, and an umbrella is an error naming the
/// artifact it needs.
///
/// Returns the nodes to bind, and what was swept in from the includes.
fn bindable<'a>(
    header: &Path,
    top: &'a [Value],
    exported: Option<&std::collections::HashSet<String>>,
) -> Result<(Vec<&'a Value>, Option<Swept>), String> {
    let is_fn = |n: &Value| n.get("kind").and_then(Value::as_str) == Some("FunctionDecl");

    // clang names a file once and lets the nodes after it inherit that name, so
    // the filter has to carry the last one seen.
    let want = header.to_string_lossy().to_string();
    let mut current = String::new();
    let mut own: Vec<&Value> = Vec::new();
    for n in top {
        if let Some(f) = n.get("loc").and_then(|l| l.get("file")).and_then(Value::as_str) {
            current = f.to_string();
        }
        if current == want {
            own.push(n);
        }
    }

    let has_own = own.iter().any(|n| is_fn(n));

    let Some(exported) = exported else {
        if has_own {
            return Ok((own, None));
        }
        return Err(format!(
            "{} declares no functions of its own — it is an umbrella header that only includes \
             others.\n  Which of those to bind can be settled by the library's own export table, \
             so point at\n  the artifact as well:\n    \
             jade pkg add <name> --path <the .so> --header {}",
            header.display(),
            header.display()
        ));
    };

    // A declaration the named header already carries is not swept in again.
    // Its own come first and unconditionally: they are what the user pointed
    // at, and an exported-only rule would silently drop a symbol declared there
    // that the artifact happens not to export.
    let own_names: std::collections::HashSet<&str> = own
        .iter()
        .filter(|n| is_fn(n))
        .filter_map(|n| n.get("name").and_then(Value::as_str))
        .collect();

    let swept: Vec<&Value> = top
        .iter()
        .filter(|n| is_fn(n))
        .filter(|n| {
            n.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| exported.contains(name) && !own_names.contains(name))
        })
        .collect();

    if !has_own && swept.is_empty() {
        return Err(format!(
            "nothing to bind in {}: it declares no functions of its own, and none of the \
             declarations it includes are symbols this library exports.",
            header.display()
        ));
    }

    if swept.is_empty() {
        return Ok((own, None));
    }

    let n = swept.len();
    let umbrella = !has_own;
    let mut all = own;
    all.extend(swept);
    Ok((all, Some(Swept { n, umbrella })))
}

/// Read `header` and produce the tables for a `[dependencies.<name>]` entry.
///
/// `only` filters by substring when given, so a large header can be bound a
/// piece at a time rather than all at once.
///
/// `exported` is the library's symbol table when one could be read. It is what
/// makes an umbrella header work — see [`bindable`].
pub fn from_header(
    header: &Path,
    include_dirs: &[String],
    only: Option<&str>,
    exported: Option<&std::collections::HashSet<String>>,
) -> Result<Binding, String> {
    // Resolved here rather than at each call site: `discover_header` below
    // passes no directories at all, so a candidate needing one was silently
    // demoted to a fallback instead of being read.
    let dirs = include_roots(header, include_dirs);
    let ast = ast_of(header, &dirs)?;

    let empty = Vec::new();
    let top = ast.get("inner").and_then(Value::as_array).unwrap_or(&empty);
    let (mine, swept) = bindable(header, top, exported)?;

    // Types come from the *whole* translation unit, functions only from the
    // headers chosen above. The two need different scopes: a library splits its
    // types out into `git2/types.h` and declares functions against them in
    // twenty other headers, so an environment built from one file alone reports
    // every one of those functions as taking an unsupported type. Types are
    // safe to take from everywhere because nothing is emitted for a type on its
    // own — one is only ever recorded because a bound function reached it.
    let all: Vec<&Value> = top.iter().collect();
    let env = build_env(&all);

    // Two questions about the library as a whole, asked once before any symbol
    // is mapped: which struct types it hands out, and which it takes. A single
    // declaration cannot answer either.
    let produced = produced_types(&mine, &env);
    let counts = struct_param_counts(&mine, &env);

    let mut b = Binding { swept, ..Default::default() };

    for n in &mine {
        if n.get("kind").and_then(Value::as_str) != Some("FunctionDecl") {
            continue;
        }
        let Some(name) = n.get("name").and_then(Value::as_str) else { continue };
        if only.is_some_and(|pat| !name.contains(pat)) {
            continue;
        }
        // A header can declare more than the library actually ships — it is
        // written for the newest version, while the built artifact may have
        // been configured without some of it. Binding one of those produces a
        // shim that compiles and then fails to *link*, and the linker takes the
        // whole dependency down over it. libbrotlienc's header declares two
        // such symbols. The export table is the authority on what is really in
        // there, so when it can be read it decides.
        if exported.is_some_and(|e| !e.contains(name)) {
            b.skipped.push((
                name.to_string(),
                "is declared by the header but not exported by the library".into(),
            ));
            continue;
        }
        // A definition in a header is `static inline`; there is no exported
        // symbol to bind against.
        if n.get("inner").and_then(Value::as_array).is_some_and(|inner| {
            inner.iter().any(|c| c.get("kind").and_then(Value::as_str) == Some("CompoundStmt"))
        }) {
            b.skipped.push((name.to_string(), "is defined inline, so it exports no symbol".into()));
            continue;
        }
        if n.get("variadic").and_then(Value::as_bool).unwrap_or(false) {
            b.skipped.push((name.to_string(), "takes varargs".into()));
            continue;
        }

        match map_function(n, &env, &produced, &counts) {
            Ok((sym, used_structs, assumed)) => {
                // Every struct the symbol names has to come out with it. A
                // symbol whose `out_struct:` has no field table is one the shim
                // generator refuses — and it refuses the *whole dependency*,
                // not the one symbol, so a single unrepresentable struct made
                // an otherwise fine library uninstallable. Resolve them all
                // first and drop the symbol if any fails, rather than emitting
                // a reference to a table that was never written.
                let mut entries = Vec::new();
                let mut failed = None;
                for (s, held) in used_structs {
                    // The spec carries the C spelling (`struct Info`); the
                    // environment is keyed by the normalized one. The table
                    // that comes out is keyed to match the spec, since that is
                    // what the shim looks the definition up by.
                    let Some(fields) = env.complete.get(&normalize(&s)) else { continue };
                    match struct_entry(fields, &env, held) {
                        Ok(entry) => entries.push((s, entry)),
                        Err(why) => {
                            failed = Some(why);
                            break;
                        }
                    }
                }
                if let Some(why) = failed {
                    b.skipped.push((name.to_string(), why));
                    continue;
                }
                // `held` is sticky. One library takes the same struct by value
                // in a one-shot call and threads it through a sequence in
                // another, and if either use needs it held then it is held —
                // otherwise the last symbol read would decide, and the handle
                // the other calls expect would stop being generated.
                for (name, entry) in entries {
                    match b.structs.get_mut(&name) {
                        Some(prior) => prior.held |= entry.held,
                        None => {
                            b.structs.insert(name, entry);
                        }
                    }
                }
                for why in assumed {
                    b.assumed.push((name.to_string(), why));
                }
                b.symbols.insert(name.to_string(), sym);
            }
            Err(why) => b.skipped.push((name.to_string(), why)),
        }
    }

    Ok(b)
}

/// Struct types the library *hands out*, rather than expecting the caller to
/// supply. A `T*` return, or a `T**` out-parameter, means the library owns the
/// allocation — which is precisely what a handle is.
///
/// `map_ret` already answers this question for a return value. Reading it here
/// too removes an asymmetry that made the same type a handle coming back and an
/// out-parameter going in.
fn produced_types(fns: &[&Value], env: &TypeEnv) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut note = |spelling: &str, stars: usize| {
        let t = normalize(&env.expand(spelling));
        let inner = t.trim_end_matches('*');
        if t.len() - inner.len() == stars && env.complete.contains_key(inner) {
            out.insert(inner.to_string());
        }
    };
    for n in fns {
        if let Ok(r) = ret_type_of(n) {
            note(&r, 1);
        }
        let empty = Vec::new();
        for p in n.get("inner").and_then(Value::as_array).unwrap_or(&empty) {
            if p.get("kind").and_then(Value::as_str) != Some("ParmVarDecl") {
                continue;
            }
            if let Some(q) = qual_type(p) {
                note(q, 2);
            }
        }
    }
    out
}

/// How many functions take a `T*` for each complete struct `T`.
///
/// This never causes a refusal on its own — see [`map_param`]. A record filled
/// by a call is commonly filled by several of them: libsndfile's `SF_INFO`
/// appears in `sf_open`, `sf_open_fd` and `sf_open_virtual`, and it is the case
/// out-parameters were built for.
fn struct_param_counts(fns: &[&Value], env: &TypeEnv) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for n in fns {
        let empty = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in n.get("inner").and_then(Value::as_array).unwrap_or(&empty) {
            if p.get("kind").and_then(Value::as_str) != Some("ParmVarDecl") {
                continue;
            }
            let Some(q) = qual_type(p) else { continue };
            let t = normalize(&env.expand(q));
            let Some(inner) = t.strip_suffix('*') else { continue };
            if env.complete.contains_key(inner) && seen.insert(inner.to_string()) {
                *counts.entry(inner.to_string()).or_default() += 1;
            }
        }
    }
    counts
}

/// Whether reading this struct back into Jade would lose a field.
///
/// `struct_entry` drops what the FFI cannot carry, which is the right answer
/// for a record read once and discarded. It is the wrong answer for a struct
/// the caller keeps: an `out_struct` shim zeroes a fresh local every call, so a
/// dropped field is state the next call needed and no longer has.
fn struct_loses_a_field(fields: &[(String, String)], env: &TypeEnv) -> bool {
    if fields.iter().any(|(_, t)| field_type(t, env).is_none()) {
        return true;
    }
    // A struct with nothing in it but rows is a buffer rather than a record.
    // There are no named values to read out; the thing it is for is being
    // handed back to the library, which means it has to survive between calls —
    // and an out-parameter cannot, because the shim declares a zeroed local
    // every time.
    //
    // `fd_set` is exactly this: one `int fds_bits[32]`, filled by `ares_fds`
    // and read by `ares_process`. It was held by handle until rows became
    // carryable, at which point it stopped being lossy and started being an
    // out-parameter — so `ares_process` began receiving an empty set. Nothing
    // failed; it simply did nothing, which is the failure this whole file is
    // organised against.
    !fields.is_empty()
        && fields.iter().all(|(_, t)| field_type(t, env).is_some_and(|j| j.starts_with("array<")))
}

/// The Jade spelling a struct field reads as, or `None` when the FFI cannot
/// carry it.
///
/// One predicate, deliberately. `struct_entry` and `struct_loses_a_field` ask
/// the same question — can this field make the trip — and used to answer it with
/// two copies of the same test. That survived only because neither had changed:
/// widening one and not the other would make a struct simultaneously carryable
/// and lossy, and lossy is what decides whether the caller holds it by handle.
fn field_type(ty: &str, env: &TypeEnv) -> Option<String> {
    let n = normalize(&env.expand(ty));
    if let Some(s) = scalar_of(&n) {
        return Some(s.to_string());
    }
    if pointee(&n).map(squash).as_deref() == Some("char") {
        return Some("str".to_string());
    }
    // A fixed-size array: `char mnemonic[32]`, `uint8_t bytes[24]`, `int
    // reserved[4]`. An array of things Jade has maps to an array of them, and
    // the element type decides what they are — one rule rather than a special
    // case per element.
    //
    // Split before expanding, not after: clang leaves the element typedef
    // alone, so the written type is `uint8_t[24]` and `expand` finds no alias
    // for that whole string.
    if let Some((elem, count)) = array_of(ty) {
        // Plain `char` is text and `unsigned char` is data, the same rule
        // `map_param` uses for a pointer. The element type decides, not the
        // position.
        let e = normalize(&env.expand(elem));
        let jade_elem = if squash(&e) == "char" { "char" } else { scalar_of(&e)? };
        return Some(format!("array<{jade_elem}>:{count}"));
    }
    None
}

/// Split a fixed-size array type into its element and extent.
///
/// `char[32]` → `("char", 32)`. An array with no size — a flexible member, or a
/// parameter that decayed — has no extent to read and is not one of these.
fn array_of(ty: &str) -> Option<(&str, usize)> {
    let open = ty.rfind('[')?;
    let rest = ty[open + 1..].strip_suffix(']')?;
    let count: usize = rest.trim().parse().ok()?;
    // Zero-length arrays are a GCC extension used as a flexible member; there
    // is nothing to read and nothing to write.
    (count > 0).then(|| (ty[..open].trim(), count))
}

/// Only the fields the FFI can carry. A struct with one unrepresentable field
/// is still worth binding for the rest, so this drops fields rather than the
/// struct — but a struct with *no* usable field is not worth a table.
fn struct_entry(fields: &[(String, String)], env: &TypeEnv, held: bool) -> Result<CStruct, String> {
    let usable: Vec<(String, String)> =
        fields.iter().filter_map(|(f, t)| Some((f.clone(), field_type(t, env)?))).collect();
    // A struct passed by value has to carry *something*, or there is nothing to
    // hand back. A held one does not: it is reached through a handle, so the
    // library can keep whatever it likes in there and Jade never needs to see
    // it. Refusing an all-opaque held struct would refuse the shape handles
    // exist for.
    if usable.is_empty() && !held {
        return Err("fills a struct with no field the FFI can carry".to_string());
    }
    Ok(CStruct {
        fields: usable,
        held,
        buffers: if held { buffer_fields(fields, env) } else { Vec::new() },
    })
}

/// The buffer fields of a held struct: a byte pointer, and the count declared
/// next to it.
///
/// The same positional idiom the parameter list uses, applied to a struct
/// definition, because C encodes it the same way in both. `lzma_stream` declares
/// `next_in` then `avail_in`, then `next_out` then `avail_out`; `ZSTD_outBuffer`
/// declares `dst` then `size`.
///
/// These are exactly the fields that make a held struct necessary — the pointers
/// a codec keeps its position in — so a held struct without them is a handle you
/// can make and never feed.
fn buffer_fields(fields: &[(String, String)], env: &TypeEnv) -> Vec<crate::project::CBuffer> {
    let mut out = Vec::new();
    for (a, b) in fields.iter().zip(fields.iter().skip(1)) {
        let ta = env.expand(&a.1);
        let na = normalize(&ta);
        let byte_like = pointee(&na).map(squash).is_some_and(|s| {
            matches!(
                s.as_str(),
                "void" | "char" | "unsignedchar" | "signedchar" | "uint8_t" | "int8_t"
            )
        });
        if !byte_like || !is_int(&normalize(&env.expand(&b.1))) {
            continue;
        }
        // A field the library set aside for its own future use is not a buffer,
        // whatever its type. `lzma_stream` ends in four `void *reserved_ptr` and
        // several `reserved_int`, and two of them happen to sit next to each
        // other in the pointer-then-count order. Generating a setter for one
        // would offer a way to write where the library requires a zero.
        if a.0.starts_with("reserved") || b.0.starts_with("reserved") {
            continue;
        }
        out.push(crate::project::CBuffer {
            ptr: a.0.clone(),
            len: b.0.clone(),
            // `const` is what says which direction it runs, exactly as it does
            // for a parameter: a read-only pointer is data going in, a writable
            // one is room for the library to fill.
            writable: !ta.contains("const"),
        });
    }
    out
}

/// Map one `FunctionDecl`. Returns the symbol, the struct types it needs, and
/// anything that was assumed.
fn map_function(
    node: &Value,
    env: &TypeEnv,
    produced: &std::collections::HashSet<String>,
    counts: &HashMap<String, usize>,
) -> Result<(CSymbol, Vec<(String, bool)>, Vec<String>), String> {
    let raw_ret = ret_type_of(node)?;
    // A returned pointer whose length arrives through a parameter cannot be
    // decided from the return type alone, so the refusal is held until the
    // parameters have been read. `returns_a_blob` below either rescues it or
    // hands back exactly this error.
    let mapped_ret = map_ret(&raw_ret, env);
    let mut ret = match &mapped_ret {
        Ok(r) => r.clone(),
        Err(_) => String::new(),
    };
    // A struct handed back by value needs its field table written out too, the
    // same as one filled through a parameter.
    let mut structs: Vec<(String, bool)> = match ret.strip_prefix("struct:") {
        Some(name) => vec![(name.to_string(), false)],
        None => Vec::new(),
    };

    let empty = Vec::new();
    let parms: Vec<&Value> = node
        .get("inner")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|c| c.get("kind").and_then(Value::as_str) == Some("ParmVarDecl"))
        .collect();

    let raw: Vec<&str> = parms.iter().filter_map(|p| qual_type(p)).collect();
    if raw.len() != parms.len() {
        return Err("has a parameter clang did not give a type for".to_string());
    }

    // The header's own names for its parameters. With more than one
    // out-parameter each result needs a key, and inventing `out0`/`out1` is
    // exactly the objection that kept multiple outs out of the design. The
    // library already named them.
    let parm_names: Vec<Option<&str>> =
        parms.iter().map(|p| p.get("name").and_then(Value::as_str)).collect();

    let mut args: Vec<String> = Vec::new();
    let mut assumed: Vec<String> = Vec::new();
    // Indices into `args` of the out-parameters, and the source parameter each
    // came from, so a name can be attached afterwards — how many there are is
    // not known until the loop has finished.
    let mut out_at: Vec<(usize, usize)> = Vec::new();
    let mut skip_next = false;

    let cx = FnCtx { env, ret: &raw_ret, produced, counts, n_params: raw.len() };

    for (i, t) in raw.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        let next_name = parm_names.get(i + 1).copied().flatten();
        let prev = i.checked_sub(1).and_then(|k| raw.get(k)).copied();
        let own_name = parm_names.get(i).copied().flatten();
        match map_param(t, prev, own_name, raw.get(i + 1).copied(), next_name, &cx) {
            Mapped::One(s) => {
                // An `in_struct` needs its field table written out with it, the
                // same way an `out_struct` does. It is an ordinary argument
                // rather than an out-parameter, so it is collected here.
                if let Some(name) = s.strip_prefix("in_struct:") {
                    structs.push((name.to_string(), false));
                }
                args.push(s);
            }
            Mapped::Held(s) => {
                if let Some(name) = s.strip_prefix("handle<").and_then(|n| n.strip_suffix('>')) {
                    structs.push((name.to_string(), true));
                }
                args.push(s);
            }
            Mapped::Assumed(s, why) => {
                assumed.push(why);
                // An `inout_bytes` is an argument *and* a result, so it counts
                // towards the rule that several results each need a key.
                // `fdt_overlay_apply(void *fdt, void *fdto)` has two, and
                // leaving them out here let the symbol reach the shim generator
                // unnamed — which refuses the whole dependency rather than the
                // one symbol.
                if s == "inout_bytes" {
                    out_at.push((args.len(), i));
                }
                args.push(s);
            }
            Mapped::BytesPair(s) => {
                args.push(s);
                // The length rode along with the pointer.
                skip_next = true;
            }
            Mapped::Out(s, why) => {
                if let Some(name) = s.strip_prefix("out_struct:") {
                    structs.push((name.to_string(), false));
                }
                // A handle written through `T**` where the header *defines* `T`
                // is a struct the caller can read, not an opaque token — so it
                // needs a field table and the accessors that come with being
                // held. Without this `size_t d_disasm(…, insn **out)` hands back
                // a handle nothing in the package can look inside, which is a
                // pointer and not an answer. An opaque `sqlite3**` is unaffected:
                // there is nothing to read.
                if let Some(name) = s.strip_prefix("out_handle:")
                    && env.complete.contains_key(&normalize(name))
                {
                    structs.push((name.to_string(), true));
                }
                if let Some(w) = why {
                    assumed.push(w);
                }
                out_at.push((args.len(), i));
                args.push(s);
                // An out_buffer keeps the count as a real Jade argument, since
                // the shim reads it to size the allocation.
            }
            Mapped::Reject(why) => return Err(why),
        }
    }

    // A returned pointer, sized by one of the parameters.
    //
    // The mirror of `out_buffer`: there the return value is the count and the
    // bytes went in through a parameter, here the bytes are the return value and
    // the count comes back through one. `const void *fdt_getprop(const void
    // *fdt, int off, const char *name, int *lenp)` is the main read call in
    // libfdt and has no other spelling.
    //
    // Only rescues a return the type alone could not represent, and only when
    // exactly one parameter is a writable integer the header *named* like a
    // length. Without the name there is nothing to tell `int *lenp` from the
    // second value a call happens to write back, and guessing would size a blob
    // from an unrelated number.
    if let Err(why) = &mapped_ret {
        let is_blob = {
            let expanded = env.expand(&raw_ret);
            let t = normalize(&expanded);
            expanded.contains("const")
                && pointee(&t).map(squash).is_some_and(|s| {
                    matches!(
                        s.as_str(),
                        "void" | "unsignedchar" | "signedchar" | "uint8_t" | "int8_t"
                    )
                })
        };
        let lengths: Vec<usize> = out_at
            .iter()
            .filter(|(k, p)| {
                args[*k].starts_with("out_scalar:") && parm_names[*p].is_some_and(names_a_length)
            })
            .map(|(k, _)| *k)
            .collect();
        if !is_blob || lengths.len() != 1 {
            return Err(why.clone());
        }
        let k = lengths[0];
        let c_type = args[k].trim_start_matches("out_scalar:").to_string();
        args[k] = format!("ret_len:{c_type}");
        out_at.retain(|(i, _)| *i != k);
        // The note `out_scalar` attached no longer applies: it is not coming
        // back on its own, it is sizing the blob.
        assumed.retain(|w| !w.contains(&format!("inout_scalar:{c_type}")));
        ret = "bytes".to_string();
    }

    // Two out-parameters that both want the C return value cannot coexist: an
    // out_buffer reads it as an element count and an out_handle folds it into
    // the failure convention. The shim refuses this too — mirroring it here
    // matters because the shim refuses the whole *dependency*, not the symbol.
    let consumes_ret = |a: &String| a.starts_with("out_buffer:") || a.starts_with("out_handle:");
    if out_at.iter().filter(|(k, _)| consumes_ret(&args[*k])).count() > 1 {
        return Err("has two out-parameters that both read the C return value".to_string());
    }

    if out_at.len() > 1 {
        for &(k, p) in &out_at {
            let Some(n) = parm_names[p].filter(|n| !n.is_empty()) else {
                return Err(
                    "has several out-parameters and the header does not name them, so there is \
                     nothing to call the results"
                        .to_string(),
                );
            };
            args[k] = format!("{}@{n}", args[k]);
        }
    }

    let has_out_handle = args.iter().any(|a| a.starts_with("out_handle:"));
    let (fails_when, why_failure) =
        infer_failure(&ret, &normalize(&env.expand(&raw_ret)), has_out_handle);
    if let Some(w) = why_failure {
        assumed.push(w);
    }

    // The shim reads an out_buffer's count from the following argument and its
    // fill count from the return value; a signature that does not have both is
    // not the shape it can rewrite.
    if let Some(i) = args.iter().position(|a| a.starts_with("out_buffer:"))
        && (args.get(i + 1).map(String::as_str) != Some("int") || ret != "int")
    {
        return Err("takes a writable buffer in a shape the shim cannot rewrite".to_string());
    }

    Ok((CSymbol { args, ret, fails_when, frees_with: None }, structs, assumed))
}

/// The return type, taken from the function's `qualType` by removing the
/// parameter list. clang gives no separate field for it.
fn ret_type_of(node: &Value) -> Result<String, String> {
    let q = qual_type(node).ok_or("has no type")?;
    // `int (const char *, sqlite3 **)` → `int`. Splitting on the *last* `(`
    // would break a function-pointer return, which is rejected anyway; the
    // first is correct for every shape that survives.
    let cut = q.find('(').ok_or_else(|| format!("has an unreadable type `{q}`"))?;
    Ok(q[..cut].trim().to_string())
}

#[cfg(test)]
mod tests;

// ── Finding the header, and checking we found the right one ──────────────────

/// The symbols a shared library actually exports.
///
/// This is all a `.so` can tell us. It has no headers in it and, unless it
/// shipped with debug info that nothing strips, no types either — and C does
/// not mangle names, so `sqlite3_open` says nothing about its signature. Which
/// is why binding needs a header at all.
///
/// What the table *is* good for is checking a header against the library it is
/// supposed to describe. A header that declares symbols the library does not
/// export is the wrong header, and that is worth catching before the shim fails
/// to link.
///
/// The names come back as C wrote them, with whatever the object format spelled
/// them with taken off — see [`plain_name`]. That is what every caller is
/// comparing against: a header's declaration, `jade_pkg_init`, or a symbol the
/// generated shim is about to write into a C identifier.
///
/// `None` means the symbol table could not be read, which is a reason to skip
/// the check rather than to fail: an unreadable table proves nothing.
pub fn exported_symbols(lib: &Path) -> Option<std::collections::HashSet<String>> {
    let syms = nm_symbols(lib)?;
    let out: std::collections::HashSet<String> = syms.into_iter().map(|(_, name)| name).collect();
    (!out.is_empty()).then_some(out)
}

/// The object format a library is in.
///
/// Only two things about reading a symbol table depend on it, but both are the
/// difference between a binding and a false "not exported" — see
/// [`plain_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    /// Mach-O, including a universal ("fat") archive of them.
    MachO,
    /// ELF.
    Elf,
}

/// Which format a file is, read from its first four bytes.
///
/// Read from the file rather than from `cfg!(target_os)`, because the two
/// answer different questions. `cfg!` says which platform this build of `jade`
/// runs on; what matters here is what the *file* is. Jade is Unix-only and a
/// Mac being handed a `.so` is ordinary — a checked-in Linux artifact, a
/// container's `/usr/lib` mounted to look at — and a host-shaped guess reads
/// every one of its names wrongly. Four bytes are the only source that is right
/// in both cases.
///
/// `None` for anything that is not an object file, which is not the same as an
/// error: an archive or a linker stub can still have a readable symbol table.
pub fn object_format(lib: &Path) -> Option<ObjectFormat> {
    use std::io::Read;
    let mut f = std::fs::File::open(lib).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    magic_format(&magic)
}

/// The same test against bytes already in hand.
fn magic_format(bytes: &[u8]) -> Option<ObjectFormat> {
    let magic = bytes.get(..4)?;
    match [magic[0], magic[1], magic[2], magic[3]] {
        // Mach-O, 64- and 32-bit, either byte order.
        [0xcf, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xcf]
        | [0xce, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xce]
        // Mach-O universal ("fat") binary.
        | [0xca, 0xfe, 0xba, 0xbe] | [0xbe, 0xba, 0xfe, 0xca] => Some(ObjectFormat::MachO),
        [0x7f, b'E', b'L', b'F'] => Some(ObjectFormat::Elf),
        _ => None,
    }
}

/// The format to read `lib`'s names by, with a fallback for the files the magic
/// number does not classify.
///
/// A file `nm` can read and the magic cannot name is, in practice, a static
/// archive — and an archive on this machine was built for this machine, so the
/// host is the best answer available. This is the one place `cfg!` is right:
/// the file has been asked first and had nothing to say.
fn symbol_format(lib: &Path) -> ObjectFormat {
    object_format(lib).unwrap_or(if cfg!(target_os = "macos") {
        ObjectFormat::MachO
    } else {
        ObjectFormat::Elf
    })
}

/// The name a symbol table entry stands for, with what the format spelled it
/// with taken back off.
///
/// Two rules, and only one of them depends on the format.
///
/// **A version suffix is never part of the name.** A library built with a
/// version script exports `lzma_version_number@@XZ_5.0`; `@@` is the default
/// version and a single `@` a non-default one, and both forms sit in the same
/// table. Nothing downstream asks for the suffixed string: `dlsym` and the
/// linker both resolve the plain name to the default version, and `@` is not a
/// character a C identifier may contain, so a name carrying one cannot even be
/// written into the shim. Cutting at the first `@` is safe whatever the format,
/// because no format puts one in a C function's name.
///
/// **The leading underscore is Mach-O's, and only Mach-O's.** There a C
/// function `foo` is `_foo` in the table, so removing one is reading the name
/// back. ELF adds no prefix, and applying the rule there is wrong in both
/// directions: `__gmpz_init` — the whole of GMP's public API — stops matching
/// the header that declares it, and a library exporting `_alpha` starts
/// matching a header declaring `alpha`, which binds cleanly and then cannot
/// load.
fn plain_name(name: &str, format: ObjectFormat) -> &str {
    let bare = name.split('@').next().unwrap_or(name);
    match format {
        ObjectFormat::MachO => bare.strip_prefix('_').unwrap_or(bare),
        ObjectFormat::Elf => bare,
    }
}

/// Everything `nm` reports as defined in `lib`, as (type letter, name) pairs.
///
/// The letter is kept because callers want different subsets of it: checking a
/// header against a library cares about every defined symbol, while generating
/// placeholders cares only about the ones that are code.
fn nm_symbols(lib: &Path) -> Option<Vec<(String, String)>> {
    let run = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("nm").args(args).arg(lib).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // Every spelling, unioned, because no one of them covers both platforms.
    //
    // `-g` reads the *static* symbol table, which is what macOS keeps and what
    // Linux distributions strip out of their shared libraries: `nm -g` on
    // Debian's libglib-2.0.so reports no symbols at all. `-D` reads the dynamic
    // table, which is what a shared library actually exports and is never
    // stripped — and which macOS `nm` does not accept.
    //
    // Getting this wrong is quiet rather than loud. Without an export table an
    // umbrella header has nothing to select against, so `lzma.h` and `glib.h`
    // were unbindable on Linux while working on a Mac, and every other header
    // silently fell back to binding only its own declarations.
    let mut text = String::new();
    for args in
        [&["-g", "--defined-only"][..], &["-D", "--defined-only"][..], &["-gU"][..], &["-g"][..]]
    {
        if let Some(t) = run(args) {
            text.push_str(&t);
            text.push('\n');
        }
    }
    if text.trim().is_empty() {
        return None;
    }

    // Read from the artifact, not from the platform this build runs on: the
    // rule for a leading underscore is the file's, and the file may not be
    // native. `--with-symbol-versions` is deliberately not asked for — the
    // versions are stripped either way, and the flag is GNU-only, so requesting
    // it would only add noise on Linux and a rejected invocation on a Mac.
    let format = symbol_format(lib);

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        // "<addr> T _name" — the type letter is what says it is defined here.
        let mut it = line.split_whitespace();
        let (Some(_addr), Some(kind), Some(name)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if !matches!(kind, "T" | "t" | "D" | "S" | "B" | "W" | "i") {
            continue;
        }
        let name = plain_name(name, format).to_string();
        if name.is_empty() {
            continue;
        }
        // Unioning several listings means the same symbol arrives more than
        // once; callers count these, so a duplicate would inflate the total.
        // Several *versions* of one symbol arrive that way too, now that the
        // version is not part of the name.
        if seen.insert((kind.to_string(), name.clone())) {
            out.push((kind.to_string(), name));
        }
    }
    Some(out)
}

/// A symbol table of names with no prototypes, read from the library's exports.
///
/// This is the answer to "there is no header anywhere on this machine". The
/// export table is always readable and always incomplete — see
/// [`crate::project::UNRESOLVED`] for why the missing half cannot be recovered
/// — so the names go into `jade.toml` with `"?"` where the signature belongs,
/// and the user fills in blanks instead of going looking for a header.
///
/// Only *code* symbols are listed. A data export cannot be called, so offering
/// it as something to write a prototype for would be an invitation to a
/// mistake. Beyond that the list is what the library exports — see
/// [`is_callable_name`] for the little that is left out and why.
///
/// Empty when `nm` is missing or the library exports nothing bindable, which
/// the caller reports rather than treating as a table.
pub fn placeholder_symbols(lib: &Path) -> BTreeMap<String, CSymbol> {
    let Some(syms) = nm_symbols(lib) else { return Default::default() };
    syms.into_iter()
        .filter(|(kind, _)| matches!(kind.as_str(), "T" | "t" | "W"))
        .filter(|(_, name)| is_callable_name(name))
        .map(|(_, name)| (name, CSymbol::unresolved()))
        .collect()
}

/// Whether a name is worth offering as something to write a prototype for.
///
/// The rule used to be "no leading underscore", which is the Mach-O prefix rule
/// applied a second time and wrong on ELF for the same reason: `__gmpz_init` is
/// not a private name, it is the whole of GMP's public API. An underscore says
/// who reserved a name, not whether it can be called — and a placeholder is
/// inert, since it lands in `jade.toml` as `"?"` and every command that would
/// use the binding refuses it by name. So an extra entry costs a line in a list
/// the user reads, while a missing one is exactly the false "not exported by
/// the library" this path exists to stop producing.
///
/// What is left out is what no prototype could rescue. A C++ mangled name
/// (`_Z3fooi`) is not a C function: the shim would declare it `extern "C"` and
/// call it with the wrong ABI. And `_init`, `_fini` and `_start` belong to the
/// loader rather than to the library's API.
fn is_callable_name(name: &str) -> bool {
    // Itanium mangling is `_Z` then a digit or an upper-case tag letter, which
    // is what keeps a C name like `_Zebra` out of the test.
    let mangled = name.strip_prefix("_Z").is_some_and(|rest| {
        rest.starts_with(|c: char| c.is_ascii_digit() || c.is_ascii_uppercase())
    });
    !mangled && !matches!(name, "_init" | "_fini" | "_start")
}

/// Whether a file is a shared library this platform could load.
///
/// Read from the first four bytes rather than by asking `nm`, because the answer
/// has to be trustworthy when `nm` is missing and because "not an object file"
/// and "no tools installed" are different problems with different fixes.
///
/// This exists because the failure it catches is otherwise reported by the
/// dynamic loader, at run time, in a program that built without complaint. A
/// header compiled by mistake (`clang -o libadd.dylib add.h` emits a precompiled
/// header, not a library) reads as an ordinary file with an ordinary name, and
/// every stage before `dlopen` is happy to pass it along.
pub fn is_loadable_object(lib: &Path) -> bool {
    object_format(lib).is_some()
}

/// The same test against bytes already in hand, for `pkg::materialize`, which
/// has read the artifact and is about to write it into `libs/`.
pub fn bytes_are_loadable_object(bytes: &[u8]) -> bool {
    magic_format(bytes).is_some()
}

/// Header names a library called `lib` might plausibly ship.
///
/// `libsqlite3.dylib` → `sqlite3.h` is close to universal, and a wrong guess is
/// cheap because [`exported_symbols`] can check it.
fn header_candidates(lib: &Path, dep_name: &str) -> Vec<String> {
    let mut stems = Vec::new();
    if let Some(file) = lib.file_name().and_then(|f| f.to_str()) {
        // Strip the extension and any version tail: libfoo.1.2.dylib → foo.
        let mut stem = file.split('.').next().unwrap_or(file);
        stem = stem.strip_prefix("lib").unwrap_or(stem);
        if !stem.is_empty() {
            stems.push(stem.to_string());
        }
    }
    if !stems.iter().any(|s| s == dep_name) {
        stems.push(dep_name.to_string());
    }
    stems.into_iter().map(|s| format!("{s}.h")).collect()
}

/// Directories worth looking in, most specific first.
fn header_search_dirs(lib: &Path, root: &Path, dep_name: &str) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();

    // Beside the library, and beside the project — where a vendored library's
    // own header almost always is.
    if let Some(p) = lib.parent().filter(|p| !p.as_os_str().is_empty()) {
        dirs.push(p.to_path_buf());
        dirs.push(p.join("include"));
    }
    dirs.push(root.to_path_buf());
    dirs.push(root.join("include"));

    // What the library itself says, when it ships the standard description of
    // where its headers are.
    if let Ok(out) = std::process::Command::new("pkg-config").args(["--cflags", dep_name]).output()
        && out.status.success()
    {
        for tok in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if let Some(d) = tok.strip_prefix("-I") {
                dirs.push(std::path::PathBuf::from(d));
            }
        }
    }

    for d in ["/opt/homebrew/include", "/usr/local/include", "/usr/include"] {
        dirs.push(std::path::PathBuf::from(d));
    }

    // On macOS the system headers live in the SDK, not /usr/include.
    if let Ok(out) = std::process::Command::new("xcrun").arg("--show-sdk-path").output()
        && out.status.success()
    {
        let sdk = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !sdk.is_empty() {
            dirs.push(std::path::PathBuf::from(sdk).join("usr/include"));
        }
    }

    dirs
}

/// Look for the header describing `lib`, preferring one whose declarations the
/// library actually exports.
///
/// The export table is what turns a guess into an answer. Several `foo.h` files
/// can exist on a machine; the one that matters is the one declaring symbols
/// this library has. A candidate that matches nothing is not silently accepted.
pub fn discover_header(lib: &Path, root: &Path, dep_name: &str) -> Option<std::path::PathBuf> {
    let exported = exported_symbols(lib);
    let names = header_candidates(lib, dep_name);
    let dirs = header_search_dirs(lib, root, dep_name);

    let mut fallback = None;
    for dir in &dirs {
        for name in &names {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            let Some(exported) = &exported else {
                // Nothing to check against; first hit wins.
                return Some(path);
            };
            match from_header(&path, &[], None, Some(exported)) {
                Ok(b) if b.symbols.keys().any(|s| exported.contains(s)) => return Some(path),
                // Parsed, but describes some other library of the same name.
                Ok(_) => fallback.get_or_insert(path),
                // Did not parse here; it may still work with the -I flags the
                // caller supplies, so keep it as a last resort.
                Err(_) => fallback.get_or_insert(path),
            };
        }
    }
    fallback
}

/// How much of what the library exports the binding actually covers.
///
/// Reported because it is the one number that says whether a binding is usable,
/// and it is invisible otherwise: "181 bound" reads as success whether the
/// library has 190 entry points or 900.
pub fn coverage(binding: &Binding, exported: &std::collections::HashSet<String>) -> (usize, usize) {
    let bound = binding.symbols.keys().filter(|s| exported.contains(*s)).count();
    (bound, exported.len())
}
