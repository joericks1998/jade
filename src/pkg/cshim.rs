//! Generate a Jade-ABI binding shim for a plain C library.
//!
//! The loader requires a `jade_pkg_init` symbol (`src/native/mod.rs`), which an
//! ordinary C library like `libz` does not have. Rather than teach the runtime
//! to dispatch arbitrary C signatures — which would mean a `libffi` dependency
//! and *two* implementations of the marshalling, one for the VM and one for the
//! AOT runtime — this emits a small C file that wraps each declared symbol and
//! exports a real `jade_pkg_init`. It compiles with `cc`, which `jade build`
//! already shells out to, and the result is an ordinary Jade-ABI package that
//! both backends already know how to load.
//!
//! The declared `args` list describes the **C** signature, and the Jade
//! signature is derived from it — deliberately a different length. A `bytes`
//! argument becomes two C parameters; an out-parameter becomes one C parameter
//! and no Jade argument at all, which is what lets `x_read(handle, buf, n)` be
//! called as `x_read(handle, n)` and hand back the bytes. Without that rewrite
//! most real C signatures are simply not callable.
//!
//! `design.md` beside this file has the rules and the reasoning: how a buffer is
//! sized, how two results come back, and why a struct out-parameter requires the
//! library's header rather than a declared layout.
//!
//! An unrepresentable type is rejected by name rather than silently marshalled
//! to nil, which is the failure mode this generator exists to avoid.

use std::collections::HashMap;

use crate::project::{CStruct, CSymbol};

/// How one Jade FFI type appears in generated C.
struct CType {
    /// The C spelling used in the extern declaration.
    decl: &'static str,
    /// `JADE_FFI_*` tag constant.
    tag: &'static str,
    /// Field of `JadeValData` holding it.
    field: &'static str,
}

fn map_type(t: &str) -> Option<CType> {
    Some(match t {
        "int" => CType { decl: "int64_t", tag: "JADE_FFI_INT", field: "as_int" },
        "float" => CType { decl: "double", tag: "JADE_FFI_FLOAT", field: "as_float" },
        "bool" => CType { decl: "uint8_t", tag: "JADE_FFI_BOOL", field: "as_bool" },
        "str" => CType { decl: "const char*", tag: "JADE_FFI_STR", field: "as_str" },
        _ => return None,
    })
}

const SUPPORTED_TYPES: &str =
    "int, float, bool, str, bytes, handle<Type>, out_buffer:<ctype>, out_struct:<Type>, \
     out_handle:<Type> (and nil for a return type)";

/// What one entry of a symbol's `args` list means.
///
/// The list describes the *C* signature, not the Jade one, and the two are not
/// the same length. A `bytes` argument becomes two C parameters; an out-parameter
/// becomes one C parameter and *no* Jade argument at all. That mismatch is the
/// whole point — it is what lets `x_read(handle, buf, n)` be called from Jade as
/// `x_read(handle, n)` and hand back the bytes.
enum ArgSpec {
    /// `int` / `float` / `bool` / `str` — one Jade argument, one C parameter.
    Scalar(CType),
    /// `bytes` — one Jade blob, expanding to the C pair `(const void*, size_t)`.
    /// The pointer is borrowed for the duration of the call, like a `str`.
    Bytes,
    /// `out_buffer:<ctype>` — no Jade argument. The shim allocates scratch, the
    /// library fills it, and the filled prefix comes back as a fresh `bytes`.
    ///
    /// A Jade `bytes` is immutable and has exactly three methods; letting a C
    /// library write into one would break that for the sake of the FFI. So the
    /// shim owns the buffer and Jade only ever sees the finished blob.
    OutBuffer { elem: String },
    /// `out_struct:<Type>` — no Jade argument. The shim declares a zeroed local
    /// of the real C type and passes its address.
    OutStruct { name: String },
    /// `handle<T>` — one Jade handle, unwrapped to the `T*` the library issued.
    ///
    /// The type name is checked before the pointer is used: two handles are
    /// structurally identical, so without the check a `sqlite3_stmt` passed
    /// where a `sqlite3` belongs is a dereference of the wrong object inside the
    /// library rather than anything Jade could report.
    Handle { name: String },
    /// `out_handle:<T>` — no Jade argument. The shim declares a null `T*`,
    /// passes its address, and hands the filled pointer back as a handle.
    ///
    /// This is the shape of every SQLite connection: `sqlite3_open(path,
    /// &db)`. Without it a generated binding could not produce a handle at all,
    /// which would leave the libraries handles exist for still unbindable.
    OutHandle { name: String },
    /// `callback:<ret>(<arg>,…)` — one Jade function, passed as a C function
    /// pointer of exactly that signature.
    ///
    /// No `libffi` is involved, and that is the point of generating the shim
    /// from a declaration: the signature is known when the C is written, so the
    /// shim can declare a real static function of that shape. Synthesising one
    /// at run time would need a trampoline compiler.
    Callback { ret: String, params: Vec<String> },
}

impl ArgSpec {
    /// Whether this consumes one of the Jade call's arguments.
    fn takes_jade_arg(&self) -> bool {
        matches!(
            self,
            ArgSpec::Scalar(_) | ArgSpec::Bytes | ArgSpec::Handle { .. } | ArgSpec::Callback { .. }
        )
    }

    /// How it is spelled in the `extern` prototype.
    fn c_decl(&self) -> String {
        match self {
            ArgSpec::Scalar(t) => t.decl.to_string(),
            ArgSpec::Bytes => "const void*, size_t".to_string(),
            ArgSpec::OutBuffer { elem } => format!("{elem}*"),
            ArgSpec::OutStruct { name } => format!("{name}*"),
            ArgSpec::Handle { name } => format!("{name}*"),
            ArgSpec::OutHandle { name } => format!("{name}**"),
            ArgSpec::Callback { ret, params } => {
                // Verbatim: the library's own C types, so the function pointer
                // this declares is the one it expects.
                let r = if ret == "nil" || ret == "void" { "void" } else { ret.as_str() };
                let ps = if params.is_empty() { "void".to_string() } else { params.join(", ") };
                format!("{r} (*)({ps})")
            }
        }
    }
}

/// How a C type in a callback signature crosses into a `JadeVal`.
///
/// A callback signature is written in the library's **own C types**, not Jade's.
/// It has to be: the shim declares a function pointer that the library will
/// store and call, so `int` must be `int` and not the `int64_t` Jade widens it
/// to. Getting that wrong is not a silent truncation — it is an incompatible
/// function pointer, which is a call through the wrong ABI.
///
/// Returns the tag, the union field, and the cast used when reading the value
/// back out of Jade.
fn c_scalar(t: &str) -> Option<(&'static str, &'static str)> {
    let squashed: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    Some(match squashed.as_str() {
        "char" | "signedchar" | "unsignedchar" | "short" | "unsignedshort" | "int"
        | "unsigned" | "unsignedint" | "long" | "unsignedlong" | "longlong"
        | "unsignedlonglong" | "size_t" | "ssize_t" | "int8_t" | "int16_t" | "int32_t"
        | "int64_t" | "uint8_t" | "uint16_t" | "uint32_t" | "uint64_t" => {
            ("JADE_FFI_INT", "as_int")
        }
        "float" | "double" => ("JADE_FFI_FLOAT", "as_float"),
        "_Bool" | "bool" => ("JADE_FFI_BOOL", "as_bool"),
        "constchar*" | "char*" | "constchar *" => ("JADE_FFI_STR", "as_str"),
        _ => return None,
    })
}

/// Parse `callback:<ret>(<arg>,…)` into its C type names.
fn parse_callback(pkg: &str, sym: &str, spec: &str) -> Result<ArgSpec, String> {
    let body = spec.strip_prefix("callback:").expect("checked by the caller");
    let bad = || {
        format!(
            "dependency '{pkg}': symbol '{sym}' has `{spec}`, which is not a callback signature.              Write it as `callback:<ret>(<arg>, …)`, e.g. `callback:int(int, str)`."
        )
    };
    let open = body.find('(').ok_or_else(bad)?;
    if !body.ends_with(')') {
        return Err(bad());
    }
    let ret = body[..open].trim().to_string();
    let inner = body[open + 1..body.len() - 1].trim();
    let params: Vec<String> = if inner.is_empty() || inner == "void" {
        Vec::new()
    } else {
        inner.split(',').map(|p| p.trim().to_string()).collect()
    };

    // A callback may only give back a scalar. Anything else would have to be
    // released inside the C library's own frame, by code that has no idea it is
    // holding a Jade value.
    let ret_ok = ret == "void" || ret == "nil"
        || matches!(c_scalar(&ret), Some((t, _)) if t != "JADE_FFI_STR");
    if !ret_ok {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' has a callback returning '{ret}'. A callback may \
             return a C integer, float, bool, or void — anything else would have to be freed \
             inside the C library's own frame."
        ));
    }
    for p in &params {
        if c_scalar(p).is_none() {
            return Err(format!(
                "dependency '{pkg}': symbol '{sym}' has a callback parameter '{p}'. A callback \
                 signature is written in the library's own C types, e.g. \
                 `callback:int(int, const char*)`, and '{p}' is not one the FFI can carry."
            ));
        }
        check_c_ident(pkg, sym, &p.replace('*', ""), "callback")?;
    }
    if !(ret == "nil" || ret == "void") {
        check_c_ident(pkg, sym, &ret.replace('*', ""), "callback")?;
    }
    Ok(ArgSpec::Callback { ret, params })
}

/// The C type a `handle<T>` names, or `None` if `spec` is not one.
fn handle_target(spec: &str) -> Option<&str> {
    spec.strip_prefix("handle<")?.strip_suffix('>')
}

/// What a symbol gives back, before any out-parameter is folded in.
enum RetSpec {
    Nil,
    Scalar(CType),
    /// `handle<T>` — the library's `T*`, wrapped for Jade to hold.
    Handle(String),
}

impl RetSpec {
    /// How it is spelled in the `extern` prototype.
    fn c_decl(&self) -> String {
        match self {
            RetSpec::Nil => "void".to_string(),
            RetSpec::Scalar(t) => t.decl.to_string(),
            RetSpec::Handle { 0: name } => format!("{name}*"),
        }
    }
}

fn parse_ret(pkg: &str, sym: &str, spec: &str) -> Result<RetSpec, String> {
    if spec == "nil" {
        return Ok(RetSpec::Nil);
    }
    if let Some(name) = handle_target(spec) {
        return check_c_ident(pkg, sym, name, "handle").map(|_| RetSpec::Handle(name.to_string()));
    }
    map_type(spec).map(RetSpec::Scalar).ok_or_else(|| bad_type_msg(pkg, sym, spec))
}

fn parse_arg(pkg: &str, sym: &str, spec: &str) -> Result<ArgSpec, String> {
    if let Some(elem) = spec.strip_prefix("out_buffer:") {
        return check_c_ident(pkg, sym, elem, "out_buffer").map(|_| ArgSpec::OutBuffer {
            elem: elem.to_string(),
        });
    }
    if let Some(name) = spec.strip_prefix("out_struct:") {
        return check_c_ident(pkg, sym, name, "out_struct").map(|_| ArgSpec::OutStruct {
            name: name.to_string(),
        });
    }
    if let Some(name) = spec.strip_prefix("out_handle:") {
        return check_c_ident(pkg, sym, name, "out_handle").map(|_| ArgSpec::OutHandle {
            name: name.to_string(),
        });
    }
    if let Some(name) = handle_target(spec) {
        return check_c_ident(pkg, sym, name, "handle").map(|_| ArgSpec::Handle {
            name: name.to_string(),
        });
    }
    if spec.starts_with("callback:") {
        return parse_callback(pkg, sym, spec);
    }
    if spec == "bytes" {
        return Ok(ArgSpec::Bytes);
    }
    map_type(spec).map(ArgSpec::Scalar).ok_or_else(|| bad_type_msg(pkg, sym, spec))
}

/// Refuse anything that is not a plain C identifier (with spaces allowed for
/// `unsigned char`).
///
/// This text is pasted straight into generated C, so without the check a
/// declaration could inject arbitrary code into the shim — and a typo would
/// surface as an incomprehensible compiler error rather than as the manifest
/// problem it is.
fn check_c_ident(pkg: &str, sym: &str, s: &str, what: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
        && !s.contains("  ");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "dependency '{pkg}': symbol '{sym}' has `{what}:{s}`, which is not a C type name. \
             It is written straight into the generated shim, so it must be a plain identifier \
             such as `short` or `SF_INFO`."
        ))
    }
}

/// The ABI declarations every shim needs. Must stay byte-compatible with
/// `JadeVal`/`JadeBinding`/`JadeNativePkg` in `src/native/mod.rs` — note the
/// seven padding bytes, which are load-bearing for the struct layout.
const PREAMBLE: &str = r#"/* Generated by `jade pkg install` — do not edit.
 * Binding shim: wraps a plain C library in the Jade native package ABI. */
#include <stdint.h>
#include <stddef.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>

#define JADE_FFI_NIL    0
#define JADE_FFI_INT    1
#define JADE_FFI_FLOAT  2
#define JADE_FFI_BOOL   3
#define JADE_FFI_STR    4
#define JADE_FFI_ERROR  5
#define JADE_FFI_STRUCT 8
#define JADE_FFI_BYTES  9
#define JADE_FFI_HANDLE 10
#define JADE_FFI_FN     11

typedef struct JadeStruct JadeStruct;
typedef struct JadeBytes JadeBytes;
typedef struct JadeHandle JadeHandle;
typedef struct JadeFn JadeFn;

typedef union {
    int64_t      as_int;
    double       as_float;
    uint8_t      as_bool;
    const char*  as_str;
    uint64_t     as_nil;
    JadeStruct*  as_struct;
    JadeBytes*   as_bytes;
    JadeHandle*  as_handle;
    JadeFn*      as_fn;
} JadeValData;

typedef struct { uint8_t tag; uint8_t _pad[7]; JadeValData data; } JadeVal;

/* Counted, not NUL-terminated: a blob may contain NUL bytes and need not be
 * valid UTF-8. Allocated with libc malloc so Jade's ffi_free can reclaim it —
 * the process holds two allocators that must not free each other's memory. */
struct JadeBytes { unsigned char* data; size_t len; };
struct JadeStruct { const char* type_name; const char** keys; JadeVal* vals; size_t len; };

/* An opaque pointer plus the C type it came from. The wrapper and the name are
 * libc-owned and Jade reclaims them; `ptr` is the library's and Jade never
 * frees it. The name is what keeps handle<sqlite3> and handle<sqlite3_stmt>
 * from being interchangeable. */
struct JadeHandle { void* ptr; const char* type_name; };

/* A Jade function the library may call back. `invoke` answers 0 on success, and
 * non-zero when the Jade side raised — which must never propagate out of it,
 * because the library is mid-operation and unwinding through its frames would
 * leave it however it happens to be. */
struct JadeFn {
    void* host;
    int (*invoke)(void* host, size_t argc, const JadeVal* argv, JadeVal* out);
};
typedef int (*JadeNativeFnPtr)(size_t argc, const JadeVal* argv, JadeVal* out);
typedef struct { const char* name; JadeNativeFnPtr func; } JadeBinding;
typedef struct { const char* name; const JadeBinding* bindings; size_t binding_count; } JadeNativePkg;

"#;


/// Emitted only when some symbol declares a failure convention. Without the
/// gate a shim whose symbols cannot fail carries a function nothing calls,
/// which is the `-Wunused-function` noise that makes a real warning easy to
/// miss.
const ERRNO_HELPER: &str = r#"
/* Describe the failure the library just reported.
 *
 * The buffer is _Thread_local because Jade tasks are real OS threads and two of
 * them can be inside different failing calls at once. Returning it borrowed is
 * safe under the ABI's output-string rule: both engines copy the message before
 * the native call returns.
 *
 * strerror_r is used where POSIX guarantees it, because plain strerror shares
 * one buffer across threads. The two signatures disagree about what they
 * return, hence the split — GNU hands back a pointer that may not be `buf` at
 * all, and ignoring that is how a message ends up empty. */
static const char* jade_shim_errmsg(void) {
    static _Thread_local char buf[256];
    int e = errno;
    if (e == 0) {
        /* The convention said failure but errno was never set. Say exactly
         * that rather than inventing "Success", which is what strerror(0)
         * would return and reads as a bug in Jade. */
        snprintf(buf, sizeof buf, "the call reported failure but set no errno");
        return buf;
    }
#if defined(__GLIBC__) && defined(_GNU_SOURCE)
    char tmp[192];
    const char* msg = strerror_r(e, tmp, sizeof tmp);
    snprintf(buf, sizeof buf, "%s (errno %d)", msg ? msg : "unknown error", e);
#elif (defined(_POSIX_C_SOURCE) && _POSIX_C_SOURCE >= 200112L) || defined(__APPLE__)
    char tmp[192];
    if (strerror_r(e, tmp, sizeof tmp) != 0) snprintf(tmp, sizeof tmp, "unknown error");
    snprintf(buf, sizeof buf, "%s (errno %d)", tmp, e);
#else
    snprintf(buf, sizeof buf, "%s (errno %d)", strerror(e), e);
#endif
    return buf;
}
"#;

/// Emitted only when some symbol returns a filled buffer. A shim that never
/// needs it should not carry it: dead code in generated output invites exactly
/// the `-Wunused-function` noise that makes a real warning easy to miss.
const BYTES_HELPER: &str = r#"
/* Copy `n` bytes into a fresh JadeBytes for Jade to take ownership of.
 *
 * Everything here is libc malloc, because Jade releases it with ffi_free and
 * the two runtimes in the process must not free each other's allocations. The
 * `n ? n : 1` is not paranoia: malloc(0) may legitimately return NULL, which
 * the free path cannot tell apart from a failure. */
static JadeBytes* jade_shim_bytes(const void* src, size_t n) {
    JadeBytes* b = (JadeBytes*)malloc(sizeof(JadeBytes));
    if (!b) return NULL;
    b->data = (unsigned char*)malloc(n ? n : 1);
    if (!b->data) { free(b); return NULL; }
    b->len = n;
    if (n && src) memcpy(b->data, src, n);
    return b;
}
"#;

/// Emitted only when some symbol takes or produces a handle.
const HANDLE_HELPER: &str = r#"
/* Wrap a library pointer as a handle Jade can hold. libc heap, because Jade's
 * ffi_free reclaims the wrapper and the name — but never `ptr`, which stays the
 * library's to close. */
static JadeHandle* jade_shim_handle(void* p, const char* type_name) {
    JadeHandle* h = (JadeHandle*)malloc(sizeof(JadeHandle));
    if (!h) return NULL;
    h->ptr = p;
    h->type_name = strdup(type_name);
    if (!h->type_name) { free(h); return NULL; }
    return h;
}

/* Unwrap a handle argument, checking the type it claims.
 *
 * The check is the point of carrying a name: two handles are structurally
 * identical, so passing a statement where a connection belongs would otherwise
 * be a dereference of the wrong object inside the library, with nothing for
 * Jade to report. Returns 0 on a mismatch. */
static int jade_shim_unwrap(const JadeVal* v, const char* want, void** out) {
    if (v->tag != JADE_FFI_HANDLE || !v->data.as_handle) return 0;
    const JadeHandle* h = v->data.as_handle;
    if (!h->type_name || strcmp(h->type_name, want) != 0) return 0;
    *out = h->ptr;
    return 1;
}
"#;

/// Emitted only when some symbol fills a struct out-parameter.
const STRUCT_HELPER: &str = r#"
/* An empty JadeStruct of `n` fields, named `type_name`. The caller fills keys
 * and vals. Same libc-heap rule as above. */
static JadeStruct* jade_shim_struct(const char* type_name, size_t n) {
    JadeStruct* s = (JadeStruct*)malloc(sizeof(JadeStruct));
    if (!s) return NULL;
    s->type_name = strdup(type_name);
    s->keys = (const char**)malloc((n ? n : 1) * sizeof(char*));
    s->vals = (JadeVal*)malloc((n ? n : 1) * sizeof(JadeVal));
    if (!s->type_name || !s->keys || !s->vals) return NULL;
    s->len = n;
    return s;
}
"#;

/// A symbol's parsed shape.
struct Parsed {
    args: Vec<ArgSpec>,
    /// Position in `args` of the single out-parameter, if there is one.
    out_at: Option<usize>,
}

/// Parse and validate one symbol's argument list.
///
/// The constraints all exist to keep a mistake in `jade.toml` from becoming a
/// wrong pointer at run time, where it would be a crash inside the library
/// rather than anything Jade could report.
fn parse_symbol(
    pkg: &str,
    sym: &str,
    spec: &CSymbol,
    structs: &HashMap<String, CStruct>,
    headers: &[String],
) -> Result<Parsed, String> {
    let args: Vec<ArgSpec> = spec
        .args
        .iter()
        .map(|a| parse_arg(pkg, sym, a))
        .collect::<Result<_, _>>()?;

    let outs: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.takes_jade_arg())
        .map(|(i, _)| i)
        .collect();

    if outs.len() > 1 {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' has {} out-parameters, and a symbol may have at \
             most one. Two would have to come back as a pair with no obvious names, which is \
             worse than splitting the binding.",
            outs.len()
        ));
    }

    let out_at = outs.first().copied();

    if let Some(i) = out_at {
        match &args[i] {
            ArgSpec::OutBuffer { .. } => {
                // The element count is the next declared argument. That is the
                // shape essentially every buffer-filling C function has —
                // read(fd, buf, n), gzread, sf_read_short — and the shim has to
                // know how much to allocate before it can call anything.
                let next = args.get(i + 1);
                let is_int = matches!(next, Some(ArgSpec::Scalar(t)) if t.tag == "JADE_FFI_INT");
                if !is_int {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' has an `out_buffer` that is not \
                         followed by an `int`. The argument after the buffer is how many \
                         elements it holds, so the shim knows how much to allocate."
                    ));
                }
                if spec.ret != "int" {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' has an `out_buffer` but returns \
                         '{}'. The return value is read as the number of elements written, so \
                         it must be `int`.",
                        spec.ret
                    ));
                }
            }
            ArgSpec::OutStruct { name } => {
                if !structs.contains_key(name) {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' fills an `out_struct:{name}`, but \
                         there is no [dependencies.{pkg}.structs.{name}] table saying which \
                         fields to read back."
                    ));
                }
                if headers.is_empty() {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' fills an `out_struct:{name}`, so the \
                         dependency needs `headers = [\"<the library's header>\"]`. The shim has \
                         to declare a real {name}, and taking its layout from the field list \
                         instead would write at the wrong offsets whenever the two disagree."
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(Parsed { args, out_at })
}

/// Emit the shim's C source for `symbols`.
///
/// Symbols are emitted in sorted order so a reinstall produces an identical
/// file rather than churning with `HashMap` iteration order.
pub fn generate(
    name: &str,
    symbols: &HashMap<String, CSymbol>,
    structs: &HashMap<String, CStruct>,
    headers: &[String],
) -> Result<String, String> {
    if symbols.is_empty() {
        return Err(format!("dependency '{name}': no symbols declared for the C binding shim"));
    }

    let mut names: Vec<&String> = symbols.keys().collect();
    names.sort();

    let mut out = String::from(PREAMBLE);

    // Only the helpers some symbol actually reaches. Dead code in generated
    // output invites the `-Wunused-function` noise that makes a real warning
    // easy to miss.
    let parsed: Vec<Parsed> = names
        .iter()
        .map(|s| parse_symbol(name, s, &symbols[*s], structs, headers))
        .collect::<Result<_, _>>()?;
    if names.iter().any(|s| symbols[*s].fails_when.is_some_and(|f| f.test().is_some())) {
        out.push_str(ERRNO_HELPER);
    }
    let out_specs = || parsed.iter().filter_map(|p| p.out_at.map(|i| &p.args[i]));
    if out_specs().any(|a| matches!(a, ArgSpec::OutBuffer { .. })) {
        out.push_str(BYTES_HELPER);
    }
    if out_specs().any(|a| matches!(a, ArgSpec::OutStruct { .. })) {
        out.push_str(STRUCT_HELPER);
    }
    // A handle can arrive as an argument, a return, or an out-parameter, so all
    // three have to be checked before deciding the helper is dead.
    let uses_handle = parsed.iter().any(|p| {
        p.args.iter().any(|a| matches!(a, ArgSpec::Handle { .. } | ArgSpec::OutHandle { .. }))
    }) || names
        .iter()
        .any(|s| handle_target(&symbols[*s].ret).is_some());
    if uses_handle {
        out.push_str(HANDLE_HELPER);
    }

    // The library's own headers, so a struct out-parameter is declared with the
    // real type rather than a guess at its layout. Sorted for a stable file.
    if !headers.is_empty() {
        let mut hs: Vec<&String> = headers.iter().collect();
        hs.sort();
        out.push('\n');
        for h in hs {
            check_c_header(name, h)?;
            out.push_str(&format!("#include <{h}>\n"));
        }
    }
    out.push('\n');

    for sym in &names {
        out.push_str(&declare(name, sym, &symbols[*sym], structs, headers)?);
    }
    for sym in &names {
        // The trampoline first: the wrapper names it as the C argument.
        let parsed = parse_symbol(name, sym, &symbols[*sym], structs, headers)?;
        for a in &parsed.args {
            if let ArgSpec::Callback { ret, params } = a {
                out.push_str(&trampoline(sym, ret, params));
            }
        }
        out.push_str(&wrapper(name, sym, &symbols[*sym], structs, headers)?);
    }

    out.push_str("static const JadeBinding BINDINGS[] = {\n");
    for sym in &names {
        out.push_str(&format!("    {{ \"{sym}\", jade_shim_{sym} }},\n"));
    }
    out.push_str("};\n\n");

    out.push_str(&format!(
        "int jade_pkg_init(JadeNativePkg* out) {{\n\
         \x20   out->name = \"{name}\";\n\
         \x20   out->bindings = BINDINGS;\n\
         \x20   out->binding_count = sizeof(BINDINGS) / sizeof(BINDINGS[0]);\n\
         \x20   return 0;\n\
         }}\n"
    ));

    Ok(out)
}

/// Refuse a header name that is not a plain path, for the reason
/// [`check_c_ident`] gives: it is pasted into an `#include` line.
fn check_c_header(pkg: &str, h: &str) -> Result<(), String> {
    let ok = !h.is_empty()
        && !h.contains(['>', '<', '"', '\n', '\\'])
        && h.chars().all(|c| c.is_ascii_alphanumeric() || "._-/+".contains(c));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "dependency '{pkg}': '{h}' is not a usable header name. It goes straight into an \
             #include, so it must be a plain path such as `sndfile.h`."
        ))
    }
}

/// `extern` prototype for the target library's symbol.
///
/// Skipped entirely when the dependency supplies headers: the header already
/// declares the symbol, and a second declaration that disagrees with it — one
/// `int` where the real one is `long` — is a compile error rather than the
/// silent truncation it would be at run time. Letting the header win is the
/// whole reason to have one.
fn declare(
    pkg: &str,
    sym: &str,
    spec: &CSymbol,
    structs: &HashMap<String, CStruct>,
    headers: &[String],
) -> Result<String, String> {
    let p = parse_symbol(pkg, sym, spec, structs, headers)?;

    if spec.ret == "nil" && spec.fails_when.is_some_and(|f| f.test().is_some()) {
        // A void call returns no sentinel, so there is nothing for a failure
        // convention to test. Saying so beats generating a shim that quietly
        // ignores the declaration.
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' declares `fails_when` but returns nil, so there \
             is no return value to test. Give it a return type, or drop `fails_when`."
        ));
    }
    let ret = parse_ret(pkg, sym, &spec.ret)?;

    if !headers.is_empty() {
        return Ok(String::new());
    }

    let ret = ret.c_decl();
    let args = if p.args.is_empty() {
        "void".to_string()
    } else {
        p.args.iter().map(ArgSpec::c_decl).collect::<Vec<_>>().join(", ")
    };
    Ok(format!("extern {ret} {sym}({args});\n"))
}

/// Assign one JadeVal slot inside a container from a C expression.
///
/// A string here must be **copied**, unlike a top-level string return: a value
/// inside a container is container-owned, so Jade's `ffi_free` frees it. Handing
/// over a pointer into a stack local would be a free of the stack; handing over
/// a pointer into the library's memory would be a free of the library's.
fn emit_field(target: &str, i: usize, key: &str, jade_ty: &CType, expr: &str) -> String {
    let value = match jade_ty.field {
        "as_str" => format!("strdup(({expr}) ? ({expr}) : \"\")"),
        "as_bool" => format!("(uint8_t)(({expr}) ? 1 : 0)"),
        _ => format!("({}){expr}", jade_ty.decl),
    };
    format!(
        "    {target}->keys[{i}] = strdup(\"{key}\");\n\
         \x20   {target}->vals[{i}].tag = {};\n\
         \x20   {target}->vals[{i}].data.{} = {value};\n",
        jade_ty.tag, jade_ty.field
    )
}

/// Build the JadeStruct for a filled out-parameter.
fn emit_out_struct(
    pkg: &str,
    sym: &str,
    var: &str,
    type_name: &str,
    def: &CStruct,
    cleanup: &str,
) -> Result<String, String> {
    let n = def.fields.len();
    let mut b = format!(
        "    JadeStruct* {var}_j = jade_shim_struct(\"{type_name}\", {n});\n\
         \x20   if (!{var}_j) {{{cleanup} return 1; }}\n"
    );
    for (i, (field, ty)) in def.fields.iter().enumerate() {
        let t = map_type(ty).ok_or_else(|| {
            format!(
                "dependency '{pkg}': symbol '{sym}' reads field '{field}' of {type_name} as \
                 '{ty}', which the Jade FFI cannot represent. Supported types are \
                 {SUPPORTED_TYPES}."
            )
        })?;
        b.push_str(&emit_field(
            &format!("{var}_j"),
            i,
            field,
            &t,
            &format!("{var}.{field}"),
        ));
    }
    Ok(b)
}

/// The static C function of exactly the declared shape, which the library will
/// call and which forwards into Jade.
///
/// No libffi: because the shim is generated from a declaration, the signature is
/// known when this C is written, so a real function of that shape can simply be
/// declared. Synthesising one at run time would need a trampoline compiler.
///
/// The slot is `_Thread_local` and set only for the duration of the call, which
/// is the honest scope. A library that stores the callback and invokes it later
/// finds an empty slot and gets the neutral answer rather than a stale pointer —
/// an asynchronous registration is not supported, and pretending otherwise would
/// mean calling into an interpreter that has moved on.
fn trampoline(sym: &str, ret: &str, params: &[String]) -> String {
    let n = params.len();
    let c_params: Vec<String> =
        params.iter().enumerate().map(|(i, p)| format!("{p} a{i}")).collect();
    let c_ret = if ret == "nil" || ret == "void" { "void".to_string() } else { ret.to_string() };
    let sig = if c_params.is_empty() { "void".to_string() } else { c_params.join(", ") };

    let mut b = format!(
        "\n/* Set for the duration of one call; see the note in cshim.rs. */\n\
         static _Thread_local const JadeFn* jade_cb_{sym} = NULL;\n\
         static _Thread_local int jade_cb_failed_{sym} = 0;\n\
         \n\
         static {c_ret} jade_cbt_{sym}({sig}) {{\n"
    );

    // Nothing registered: answer neutrally rather than dereferencing.
    let is_void = ret == "nil" || ret == "void";
    let neutral = if is_void { "return;".to_string() } else { "return 0;".to_string() };
    b.push_str(&format!("    if (!jade_cb_{sym}) {{ {neutral} }}\n"));

    if n > 0 {
        b.push_str(&format!("    JadeVal cbargs[{n}];\n"));
        for (i, p) in params.iter().enumerate() {
            let (tag, field) = c_scalar(p).expect("validated");
            // Cast on the way in: the C parameter is the library's width, the
            // JadeVal field is Jade's.
            let cast = match field {
                "as_int" => "(int64_t)",
                "as_float" => "(double)",
                "as_bool" => "(uint8_t)!!",
                _ => "",
            };
            b.push_str(&format!(
                "    cbargs[{i}].tag = {tag};\n    cbargs[{i}].data.{field} = {cast}a{i};\n"
            ));
        }
    }
    b.push_str("    JadeVal cbout;\n    cbout.tag = JADE_FFI_NIL;\n    cbout.data.as_nil = 0;\n");
    let argv = if n > 0 { "cbargs" } else { "NULL" };
    b.push_str(&format!(
        "    if (jade_cb_{sym}->invoke(jade_cb_{sym}->host, {n}, {argv}, &cbout) != 0) {{\n\
         \x20       /* The Jade side raised. It must not travel out of here — the\n\
         \x20        * library is mid-operation and unwinding through its frames\n\
         \x20        * would leave it however it happens to be. Recorded, and\n\
         \x20        * turned into a Jade error once the call has returned. */\n\
         \x20       jade_cb_failed_{sym} = 1;\n\
         \x20       {}\n\
         \x20   }}\n",
        if is_void { "return;".to_string() } else { "return 1;".to_string() }
    ));

    if is_void {
        b.push_str("    (void)cbout;\n");
    } else {
        let (tag, field) = c_scalar(ret).expect("validated");
        b.push_str(&format!(
            "    if (cbout.tag != {tag}) return 0;\n    return ({ret})cbout.data.{field};\n"
        ));
    }
    b.push_str("}\n");
    b
}

/// The `JadeNativeFnPtr`-shaped wrapper that marshals in and out.
fn wrapper(
    pkg: &str,
    sym: &str,
    spec: &CSymbol,
    structs: &HashMap<String, CStruct>,
    headers: &[String],
) -> Result<String, String> {
    let p = parse_symbol(pkg, sym, spec, structs, headers)?;
    let jade_arity = p.args.iter().filter(|a| a.takes_jade_arg()).count();

    let mut body = format!(
        "\nstatic int jade_shim_{sym}(size_t argc, const JadeVal* argv, JadeVal* out) {{\n\
         \x20   if (argc != {jade_arity}) return 1;\n"
    );

    // Arity and tags are checked before anything is allocated or called: a
    // wrong-typed argument would otherwise be reinterpreted through the union
    // and hand the C function garbage.
    let mut jade_idx: Vec<Option<usize>> = Vec::new();
    let mut j = 0usize;
    for a in &p.args {
        match a {
            ArgSpec::Scalar(t) => {
                body.push_str(&format!("    if (argv[{j}].tag != {}) return 1;\n", t.tag));
            }
            ArgSpec::Bytes => {
                body.push_str(&format!("    if (argv[{j}].tag != JADE_FFI_BYTES) return 1;\n"));
            }
            ArgSpec::Callback { .. } => {
                body.push_str(&format!("    if (argv[{j}].tag != JADE_FFI_FN) return 1;\n"));
            }
            ArgSpec::Handle { name } => {
                // Unwrapped here rather than at the call, so a wrong type is
                // caught before anything is allocated and the library never
                // sees the pointer at all.
                body.push_str(&format!(
                    "    void* h{j} = NULL;\n\
                     \x20   if (!jade_shim_unwrap(&argv[{j}], \"{name}\", &h{j})) return 1;\n"
                ));
            }
            _ => {
                jade_idx.push(None);
                continue;
            }
        }
        jade_idx.push(Some(j));
        j += 1;
    }

    // Scratch for an out-parameter, declared after every check has passed.
    let mut cleanup = String::new();
    for (i, a) in p.args.iter().enumerate() {
        match a {
            ArgSpec::OutBuffer { elem } => {
                let count_at = jade_idx[i + 1].expect("validated: the next arg is an int");
                body.push_str(&format!(
                    "    int64_t n_elem = argv[{count_at}].data.as_int;\n\
                     \x20   if (n_elem < 0) return 1;\n\
                     \x20   {elem}* obuf = ({elem}*)malloc((size_t)(n_elem ? n_elem : 1) * sizeof({elem}));\n\
                     \x20   if (!obuf) return 1;\n"
                ));
                cleanup = " free(obuf);".to_string();
            }
            ArgSpec::OutStruct { name } => {
                // Zeroed, because a library is entitled to fill only the fields
                // it knows about and leave the rest as the caller set them.
                body.push_str(&format!("    {name} ostruct;\n    memset(&ostruct, 0, sizeof ostruct);\n"));
            }
            ArgSpec::OutHandle { name } => {
                // Null to start, so a library that fails without writing leaves
                // something recognisable rather than a stack pointer.
                body.push_str(&format!("    {name}* ohandle = NULL;\n"));
            }
            _ => {}
        }
    }

    let mut call_args: Vec<String> = Vec::new();
    for (i, a) in p.args.iter().enumerate() {
        match a {
            ArgSpec::Scalar(t) => {
                call_args.push(format!("argv[{}].data.{}", jade_idx[i].unwrap(), t.field))
            }
            ArgSpec::Bytes => {
                let k = jade_idx[i].unwrap();
                // A blob is one Jade value and two C parameters. The pointer is
                // borrowed for the call, exactly as a `str` argument is.
                call_args.push(format!("argv[{k}].data.as_bytes ? (const void*)argv[{k}].data.as_bytes->data : NULL"));
                call_args.push(format!("argv[{k}].data.as_bytes ? argv[{k}].data.as_bytes->len : (size_t)0"));
            }
            ArgSpec::OutBuffer { .. } => call_args.push("obuf".to_string()),
            ArgSpec::OutStruct { .. } => call_args.push("&ostruct".to_string()),
            ArgSpec::Handle { name } => {
                call_args.push(format!("({name}*)h{}", jade_idx[i].unwrap()))
            }
            ArgSpec::OutHandle { .. } => call_args.push("&ohandle".to_string()),
            ArgSpec::Callback { .. } => call_args.push(format!("jade_cbt_{sym}")),
        }
    }

    let call = format!("{sym}({})", call_args.join(", "));

    // Register the callback for exactly the duration of the call.
    let cb_at = p.args.iter().position(|a| matches!(a, ArgSpec::Callback { .. }));
    if let Some(i) = cb_at {
        let k = jade_idx[i].expect("a callback consumes a Jade argument");
        body.push_str(&format!(
            "    jade_cb_{sym} = argv[{k}].data.as_fn;\n    jade_cb_failed_{sym} = 0;\n"
        ));
    }

    // Cleared right before the call so a stale value from an earlier, unrelated
    // failure cannot be reported as this one's reason. A successful call is
    // allowed to leave errno set, which is why only the failure branch reads it.
    let fail_test = spec.fails_when.and_then(|f| f.test());
    if fail_test.is_some() {
        body.push_str("    errno = 0;\n");
    }

    let ret_t = parse_ret(pkg, sym, &spec.ret)?;

    match &ret_t {
        RetSpec::Nil => body.push_str(&format!("    {call};\n")),
        other => body.push_str(&format!("    {} r = {call};\n", other.c_decl())),
    }
    if let Some(test) = fail_test {
        // Status 1 with an ERROR tag is what both engines turn into a catchable
        // Jade raise; the message is borrowed, and both copy it before
        // returning. Scratch is released first — a raise must not leak it.
        body.push_str(&format!("    if ({test}) {{\n"));
        if !cleanup.is_empty() {
            body.push_str(&format!("       {cleanup}\n"));
        }
        body.push_str("        out->tag = JADE_FFI_ERROR;\n");
        body.push_str("        out->data.as_str = jade_shim_errmsg();\n");
        body.push_str("        return 1;\n");
        body.push_str("    }\n");
    }

    // Assign the plain return value into `out`. Shared by the no-out-parameter
    // case and by the `.ret` field of a two-result struct, which differ only in
    // where the value lands.
    let emit_ret = |target: &str| -> String {
        match &ret_t {
            RetSpec::Nil => format!("    {target}tag = JADE_FFI_NIL;\n    {target}data.as_nil = 0;\n"),
            RetSpec::Scalar(t) => {
                format!("    {target}tag = {};\n    {target}data.{} = r;\n", t.tag, t.field)
            }
            RetSpec::Handle(name) => format!(
                "    JadeHandle* rh = jade_shim_handle((void*)r, \"{name}\");\n\
                 \x20   if (!rh) return 1;\n\
                 \x20   {target}tag = JADE_FFI_HANDLE;\n\
                 \x20   {target}data.as_handle = rh;\n"
            ),
        }
    };

    // The library has returned, so the registration ends here. A raise from
    // inside the callback surfaces now — after the library finished cleanly,
    // never by unwinding through it mid-operation.
    if cb_at.is_some() {
        body.push_str(&format!(
            "    jade_cb_{sym} = NULL;\n\
             \x20   if (jade_cb_failed_{sym}) {{\n\
             \x20       jade_cb_failed_{sym} = 0;\n\
             \x20       out->tag = JADE_FFI_ERROR;\n\
             \x20       out->data.as_str = \"the callback raised\";\n\
             \x20       return 1;\n\
             \x20   }}\n"
        ));
    }

    // What Jade gets back.
    match p.out_at.map(|i| &p.args[i]) {
        // No out-parameter: the return value, as before.
        None => body.push_str(&emit_ret("out->")),

        // A filled buffer. The return value said how many elements the library
        // wrote, so it sizes the blob and does not come back separately — a
        // counted buffer already carries its length.
        Some(ArgSpec::OutBuffer { elem }) => {
            body.push_str(
                "    /* Clamp: a library reporting more than it was given would\n\
                 \x20    * otherwise make this read past the scratch. */\n\
                 \x20   int64_t got = r < 0 ? 0 : (r > n_elem ? n_elem : r);\n",
            );
            body.push_str(&format!(
                "    JadeBytes* b = jade_shim_bytes(obuf, (size_t)got * sizeof({elem}));\n"
            ));
            body.push_str("    free(obuf);\n");
            body.push_str("    if (!b) return 1;\n");
            body.push_str("    out->tag = JADE_FFI_BYTES;\n");
            body.push_str("    out->data.as_bytes = b;\n");
        }

        // A filled struct. With nothing else to report it is the result; with a
        // return value as well the two come back as `.ret` and `.out`, because
        // a C function that fills a struct *and* returns something has two
        // results and Jade has one slot.
        Some(ArgSpec::OutStruct { name }) => {
            let def = &structs[name];
            body.push_str(&emit_out_struct(pkg, sym, "ostruct", name, def, "")?);
            match &ret_t {
                RetSpec::Nil => {
                    body.push_str("    out->tag = JADE_FFI_STRUCT;\n");
                    body.push_str("    out->data.as_struct = ostruct_j;\n");
                }
                _ => {
                    body.push_str(&format!(
                        "    JadeStruct* res = jade_shim_struct(\"{sym}_result\", 2);\n\
                         \x20   if (!res) return 1;\n"
                    ));
                    body.push_str(&emit_ret("res->vals[0]."));
                    body.push_str("    res->keys[0] = strdup(\"ret\");\n");
                    body.push_str(
                        "    res->keys[1] = strdup(\"out\");\n\
                         \x20   res->vals[1].tag = JADE_FFI_STRUCT;\n\
                         \x20   res->vals[1].data.as_struct = ostruct_j;\n",
                    );
                    body.push_str("    out->tag = JADE_FFI_STRUCT;\n");
                    body.push_str("    out->data.as_struct = res;\n");
                }
            }
        }

        // A handle written through a pointer — sqlite3_open(path, &db). The
        // return value is a status, so the handle is what Jade gets and the
        // status is folded into the failure convention.
        Some(ArgSpec::OutHandle { name }) => {
            if !matches!(ret_t, RetSpec::Nil) {
                // The status only ever feeds `fails_when`; the handle is the
                // result. Marked used so a symbol without a failure convention
                // does not generate a warning about it.
                body.push_str("    (void)r;\n");
            }
            body.push_str(&format!(
                "    if (!ohandle) {{\n\
                 \x20       out->tag = JADE_FFI_NIL;\n\
                 \x20       out->data.as_nil = 0;\n\
                 \x20       return 0;\n\
                 \x20   }}\n\
                 \x20   JadeHandle* oh = jade_shim_handle((void*)ohandle, \"{name}\");\n\
                 \x20   if (!oh) return 1;\n\
                 \x20   out->tag = JADE_FFI_HANDLE;\n\
                 \x20   out->data.as_handle = oh;\n"
            ));
        }

        Some(_) => unreachable!("only out-parameters land here"),
    }

    body.push_str("    return 0;\n}\n");
    Ok(body)
}

fn bad_type_msg(pkg: &str, sym: &str, t: &str) -> String {
    format!(
        "dependency '{pkg}': symbol '{sym}' uses type '{t}', which the Jade FFI cannot \
         represent. Supported types are {SUPPORTED_TYPES}."
    )
}

#[cfg(test)]
mod tests;
