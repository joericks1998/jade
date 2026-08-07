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
        if !self.assumed.is_empty() {
            s.push_str("\n\nassumed (check these):");
            for (sym, why) in &self.assumed {
                s.push_str(&format!("\n  {sym}: {why}"));
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
        return Err(format!(
            "clang could not parse {}{hint}\n{}",
            header.display(),
            stderr.trim()
        ));
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
}

impl TypeEnv {
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
                let next = format!("{u}*");
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

fn build_env(nodes: &[&Value]) -> TypeEnv {
    let mut env = TypeEnv::default();

    // Records first, by id, so a typedef can be resolved in one pass afterwards.
    let mut by_id: HashMap<&str, (&Value, bool)> = HashMap::new();
    for n in nodes {
        if n.get("kind").and_then(Value::as_str) != Some("RecordDecl") {
            continue;
        }
        let Some(id) = n.get("id").and_then(Value::as_str) else { continue };
        let complete = n.get("completeDefinition").and_then(Value::as_bool).unwrap_or(false);
        by_id.insert(id, (n, complete));
        // A named `struct X { ... }` is usable as `struct X` directly.
        if let Some(name) = n.get("name").and_then(Value::as_str) {
            if complete {
                env.complete.insert(name.to_string(), fields_of(n));
            } else {
                env.opaque.insert(name.to_string());
            }
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
                env.alias.insert(name.to_string(), under.to_string());
            }
            continue;
        };
        // A record id that names no record we collected: still an alias, not a
        // typedef to drop on the floor.
        let Some(&(rec, complete)) = by_id.get(rid) else {
            if let Some(under) = underlying(n) {
                env.alias.insert(name.to_string(), under.to_string());
            }
            continue;
        };
        if complete {
            env.complete.insert(name.to_string(), fields_of(rec));
        } else {
            env.opaque.insert(name.to_string());
        }
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
                    Some((
                        f.get("name")?.as_str()?.to_string(),
                        qual_type(f)?.to_string(),
                    ))
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
    "char", "signedchar", "unsignedchar", "short", "shortint", "unsignedshort",
    "unsignedshortint", "int", "unsigned", "unsignedint", "long", "longint",
    "unsignedlong", "unsignedlongint", "longlong", "longlongint", "unsignedlonglong",
    "unsignedlonglongint", "size_t", "ssize_t", "ptrdiff_t", "intptr_t", "uintptr_t",
    "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t",
    "uint64_t", "off_t", "time_t", "mode_t", "pid_t", "wchar_t",
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
fn map_param(raw_in: &str, next: Option<&str>, env: &TypeEnv, ret: &str) -> Mapped {
    // Expanded before anything is decided, so a library's own typedefs read as
    // the types they stand for. `const` comes from the expansion, not the
    // written parameter, because a typedef can introduce it.
    let raw = env.expand(raw_in);
    let raw = raw.as_str();
    let is_const = raw.contains("const");
    let t = normalize(raw);

    if is_fn_ptr(raw) {
        return match callback_spec(raw, env) {
            Some(spec) => Mapped::One(spec),
            None => Mapped::Reject(
                "takes a callback whose own signature the FFI cannot carry".to_string(),
            ),
        };
    }
    if let Some(s) = scalar_of(&t) {
        return Mapped::One(s.to_string());
    }

    // `sqlite3**` — a handle written through a pointer.
    if let Some(inner) = pointee2(&t) {
        if env.opaque.contains(inner) || env.complete.contains_key(inner) {
            return Mapped::Out(format!("out_handle:{inner}"), None);
        }
        return Mapped::Reject("takes a pointer to a pointer".to_string());
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
    let next_is_len = next.map(|n| is_int(&normalize(&env.expand(n)))).unwrap_or(false);
    let squashed = squash(inner);
    let byte_like =
        matches!(squashed.as_str(), "void" | "char" | "unsignedchar" | "signedchar" | "uint8_t" | "int8_t");

    if next_is_len && !is_const && (byte_like || scalar_of(&squashed).is_some()) {
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

    // An opaque pointer: exactly what a handle is for.
    if env.opaque.contains(inner) {
        return Mapped::One(format!("handle<{inner}>"));
    }

    // A pointer to a struct the header defines. Writable means the call fills
    // it; const means it reads one, which is the direction the shim cannot do.
    if env.complete.contains_key(inner) {
        if is_const {
            return Mapped::Reject("takes a struct by pointer as input".to_string());
        }
        return Mapped::Out(format!("out_struct:{inner}"), None);
    }

    if squash(inner) == "void" {
        return Mapped::Reject("takes a `void *`, which names no type to check".to_string());
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

    // Only what the trampoline can marshal. A `void *` user-data parameter is
    // the common reason a real callback does not fit — it names no type, so
    // there is nothing to hand Jade.
    let carriable = |t: &str| -> bool {
        let n = normalize(&env.expand(t));
        scalar_of(&squash(&n)).is_some()
            || is_int(&n)
            || pointee(&n).map(squash).as_deref() == Some("char")
    };
    if !params.iter().all(|p| carriable(p)) {
        return None;
    }
    let ret_ok = normalize(&ret) == "void" || (carriable(&ret) && pointee(&normalize(&ret)).is_none());
    if !ret_ok {
        return None;
    }

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
        if env.opaque.contains(inner) || env.complete.contains_key(inner) {
            return Ok(format!("handle<{inner}>"));
        }
    }
    Err(format!("returns an unsupported type `{raw_in}`"))
}

/// The failure convention a signature implies, if any is obvious.
///
/// Only the two unambiguous shapes are inferred. Guessing that every `int` is a
/// status would turn a function returning a legitimate count into one that
/// raises on a perfectly good answer.
fn infer_failure(ret: &str, has_out_handle: bool) -> Option<CFailure> {
    if ret.starts_with("handle<") || ret == "str" {
        // A pointer-returning open: NULL is the universal failure.
        return Some(CFailure::Null);
    }
    if has_out_handle && ret == "int" {
        // `x_open(path, &h)` returning a status. The handle is the result, so
        // the status can only be a status.
        return Some(CFailure::Nonzero);
    }
    None
}

// ── Driver ───────────────────────────────────────────────────────────────────

/// Read `header` and produce the tables for a `[dependencies.<name>]` entry.
///
/// `only` filters by substring when given, so a large header can be bound a
/// piece at a time rather than all at once.
pub fn from_header(
    header: &Path,
    include_dirs: &[String],
    only: Option<&str>,
) -> Result<Binding, String> {
    let ast = ast_of(header, include_dirs)?;

    // clang reports a file once and lets following nodes inherit it, so the
    // filter has to carry the last one seen. Without this the binding would
    // include every declaration from every system header the target includes —
    // thousands of them.
    let want = header.to_string_lossy().to_string();
    let empty = Vec::new();
    let top = ast.get("inner").and_then(Value::as_array).unwrap_or(&empty);
    let mut current = String::new();
    let mut mine: Vec<&Value> = Vec::new();
    for n in top {
        if let Some(f) = n.get("loc").and_then(|l| l.get("file")).and_then(Value::as_str) {
            current = f.to_string();
        }
        if current == want {
            mine.push(n);
        }
    }

    if mine.is_empty() {
        return Err(format!(
            "no declarations found in {} — clang parsed it, but every declaration came from \
             somewhere else it includes.",
            header.display()
        ));
    }

    let env = build_env(&mine);
    let mut b = Binding::default();

    for n in &mine {
        if n.get("kind").and_then(Value::as_str) != Some("FunctionDecl") {
            continue;
        }
        let Some(name) = n.get("name").and_then(Value::as_str) else { continue };
        if only.is_some_and(|pat| !name.contains(pat)) {
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

        match map_function(n, &env) {
            Ok((sym, used_structs, assumed)) => {
                for s in used_structs {
                    if let Some(fields) = env.complete.get(&s) {
                        match struct_entry(fields, &env) {
                            Ok(entry) => {
                                b.structs.insert(s, entry);
                            }
                            Err(why) => {
                                b.skipped.push((name.to_string(), why));
                                continue;
                            }
                        }
                    }
                }
                if let Some(why) = assumed {
                    b.assumed.push((name.to_string(), why));
                }
                b.symbols.insert(name.to_string(), sym);
            }
            Err(why) => b.skipped.push((name.to_string(), why)),
        }
    }

    Ok(b)
}

/// Only the fields the FFI can carry. A struct with one unrepresentable field
/// is still worth binding for the rest, so this drops fields rather than the
/// struct — but a struct with *no* usable field is not worth a table.
fn struct_entry(fields: &[(String, String)], env: &TypeEnv) -> Result<CStruct, String> {
    let usable: Vec<(String, String)> = fields
        .iter()
        .filter_map(|(f, t)| {
            let n = normalize(&env.expand(t));
            let jt = if let Some(s) = scalar_of(&n) {
                s
            } else if pointee(&n).map(squash).as_deref() == Some("char") {
                "str"
            } else {
                return None;
            };
            Some((f.clone(), jt.to_string()))
        })
        .collect();
    if usable.is_empty() {
        return Err("fills a struct with no field the FFI can carry".to_string());
    }
    Ok(CStruct { fields: usable })
}

/// Map one `FunctionDecl`. Returns the symbol, the struct types it needs, and
/// anything that was assumed.
fn map_function(
    node: &Value,
    env: &TypeEnv,
) -> Result<(CSymbol, Vec<String>, Option<String>), String> {
    let raw_ret = ret_type_of(node)?;
    let ret = map_ret(&raw_ret, env)?;

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

    let mut args: Vec<String> = Vec::new();
    let mut structs: Vec<String> = Vec::new();
    let mut assumed: Option<String> = None;
    let mut outs = 0usize;
    let mut skip_next = false;

    for (i, t) in raw.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        match map_param(t, raw.get(i + 1).copied(), env, &raw_ret) {
            Mapped::One(s) => args.push(s),
            Mapped::BytesPair(s) => {
                args.push(s);
                // The length rode along with the pointer.
                skip_next = true;
            }
            Mapped::Out(s, why) => {
                outs += 1;
                if outs > 1 {
                    return Err("has more than one out-parameter".to_string());
                }
                if let Some(name) = s.strip_prefix("out_struct:") {
                    structs.push(name.to_string());
                }
                if let Some(w) = why {
                    assumed = Some(w);
                }
                args.push(s);
                // An out_buffer keeps the count as a real Jade argument, since
                // the shim reads it to size the allocation.
            }
            Mapped::Reject(why) => return Err(why),
        }
    }

    let has_out_handle = args.iter().any(|a| a.starts_with("out_handle:"));
    let fails_when = infer_failure(&ret, has_out_handle);

    // The shim reads an out_buffer's count from the following argument and its
    // fill count from the return value; a signature that does not have both is
    // not the shape it can rewrite.
    if let Some(i) = args.iter().position(|a| a.starts_with("out_buffer:"))
        && (args.get(i + 1).map(String::as_str) != Some("int") || ret != "int")
    {
        return Err("takes a writable buffer in a shape the shim cannot rewrite".to_string());
    }

    Ok((CSymbol { args, ret, fails_when }, structs, assumed))
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
