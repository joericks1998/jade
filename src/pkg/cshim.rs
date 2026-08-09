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
//! `README.md` in this directory has the rules and the reasoning: how a buffer
//! is sized, how two results come back, and why a struct out-parameter requires
//! the library's header rather than a declared layout.
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
    "int, float, bool, str, bytes, handle<Type>, in_struct:<Type>, out_buffer:<ctype>, \
     out_struct:<Type>, out_handle:<Type>, out_scalar:<ctype>, inout_scalar:<ctype> \
     (and nil for a return type)";

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
    /// `bytes_ptr` — one Jade blob, expanding to a single `const void*`.
    ///
    /// The same borrow as `bytes` without the count beside it, for the libraries
    /// that take a blob whose extent is written *inside* it. A device tree blob
    /// is the example: every `libfdt` call takes `const void *fdt` alone and
    /// reads the length out of the header. There is nowhere to pass a size, so
    /// refusing the shape refused most of the library.
    BytesPtr,
    /// `inout_bytes` — one Jade blob in, and the edited blob back out.
    ///
    /// For the libraries that revise a buffer in place. Every `libfdt` writer
    /// takes `void *fdt` and edits the device tree where it sits, and there is
    /// no other shape for that: a Jade blob is immutable, so the library cannot
    /// be given one to scribble on.
    ///
    /// The shim copies the caller's bytes into scratch it owns, lets the library
    /// work on that, and hands the result back as a fresh blob. Jade's value is
    /// untouched, which is what immutable has to mean, and the edit is visible
    /// as a return rather than as a mutation nothing declared.
    InoutBytes { name: Option<String> },
    /// `out_buffer:<ctype>` — no Jade argument. The shim allocates scratch, the
    /// library fills it, and the filled prefix comes back as a fresh `bytes`.
    ///
    /// A Jade `bytes` is immutable and has exactly three methods; letting a C
    /// library write into one would break that for the sake of the FFI. So the
    /// shim owns the buffer and Jade only ever sees the finished blob.
    OutBuffer { elem: String, name: Option<String> },
    /// `out_struct:<Type>` — no Jade argument. The shim declares a zeroed local
    /// of the real C type and passes its address.
    OutStruct { type_name: String, name: Option<String> },
    /// `in_struct:<Type>` — one Jade struct, copied into a real C local whose
    /// address is passed. The mirror of `out_struct`, and the shape of every
    /// `const S*` parameter: the library reads the struct and forgets it.
    ///
    /// A *copy* rather than a borrow, because there is no C struct on the Jade
    /// side to lend. Jade's value is a bag of named fields with no layout; the
    /// shim declares the real type from the header, so the compiler places every
    /// field. That is the same reason `out_struct` needs the header.
    ///
    /// Only carried for a struct whose fields the FFI can *all* represent. One
    /// it cannot — a `void*` the caller was meant to fill in — would arrive as
    /// the zero the local was memset to, and a library reading a NULL where the
    /// caller meant something is the silent-wrong-answer failure this generator
    /// exists to avoid. `out_struct` tolerates a dropped field because losing an
    /// output is visible; losing an input is not.
    InStruct { type_name: String },
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
    OutHandle { type_name: String, name: Option<String> },
    /// `out_scalar:<ctype>` — no Jade argument. The shim declares a zeroed local
    /// of the C type, passes its address, and hands the value back.
    ///
    /// The payload is the library's own C type rather than a Jade one, for the
    /// same reason `out_buffer` and `callback` carry theirs: the shim declares a
    /// real local, so `uint32_t` widened to `int64_t` would take the address of
    /// the wrong-sized object and let the library write past it.
    OutScalar { c_type: String, name: Option<String> },
    /// `inout_scalar:<ctype>` — one Jade argument *and* a result. The local is
    /// seeded from what Jade passed rather than zeroed.
    ///
    /// Plenty of C looks like an out-parameter and is really this: a position
    /// the caller sets and the library advances, `size_t *out_pos`. Zeroing one
    /// of those is right for a single call and wrong on the second, which is
    /// the kind of wrong that shows up as corrupt output rather than an error.
    InoutScalar { c_type: String, name: Option<String> },
    /// `ret_len:<ctype>` — no Jade argument, and no result of its own. The shim
    /// declares a zeroed local, passes its address, and reads it as the length
    /// of the pointer the call *returned*.
    ///
    /// The mirror of `out_buffer`, which reads the return value as the count for
    /// a buffer it passed in. Here the bytes are the return value and the count
    /// comes back through a parameter: `const void *fdt_getprop(const void *fdt,
    /// int off, const char *name, int *lenp)`, which is the main read call in
    /// libfdt and has no other spelling.
    RetLen { c_type: String },
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
            ArgSpec::Scalar(_)
                | ArgSpec::Bytes
                | ArgSpec::BytesPtr
                | ArgSpec::InoutBytes { .. }
                | ArgSpec::Handle { .. }
                | ArgSpec::Callback { .. }
                | ArgSpec::InoutScalar { .. }
                | ArgSpec::InStruct { .. }
        )
    }

    /// Whether this contributes a value to the result.
    ///
    /// Not simply the negation of [`takes_jade_arg`]: an `inout_scalar` does
    /// both, since the caller seeds it and the library writes it back. Deriving
    /// the out-parameter list from "takes no argument" left it out of the
    /// result entirely.
    fn produces_result(&self) -> bool {
        matches!(
            self,
            ArgSpec::OutBuffer { .. }
                | ArgSpec::OutStruct { .. }
                | ArgSpec::OutHandle { .. }
                | ArgSpec::OutScalar { .. }
                | ArgSpec::InoutScalar { .. }
                | ArgSpec::InoutBytes { .. }
        )
    }

    /// The key this out-parameter comes back under, when it has one.
    fn out_name(&self) -> Option<&str> {
        match self {
            ArgSpec::OutBuffer { name, .. }
            | ArgSpec::OutStruct { name, .. }
            | ArgSpec::OutHandle { name, .. }
            | ArgSpec::OutScalar { name, .. }
            | ArgSpec::InoutScalar { name, .. }
            | ArgSpec::InoutBytes { name } => name.as_deref(),
            _ => None,
        }
    }

    /// How it is spelled in the `extern` prototype.
    fn c_decl(&self) -> String {
        match self {
            ArgSpec::Scalar(t) => t.decl.to_string(),
            ArgSpec::Bytes => "const void*, size_t".to_string(),
            ArgSpec::BytesPtr => "const void*".to_string(),
            ArgSpec::InoutBytes { .. } => "void*".to_string(),
            ArgSpec::OutBuffer { elem, .. } => format!("{elem}*"),
            ArgSpec::OutStruct { type_name, .. } => format!("{type_name}*"),
            ArgSpec::InStruct { type_name } => format!("const {type_name}*"),
            ArgSpec::Handle { name } => format!("{name}*"),
            ArgSpec::OutHandle { type_name, .. } => format!("{type_name}**"),
            ArgSpec::OutScalar { c_type, .. }
            | ArgSpec::InoutScalar { c_type, .. }
            | ArgSpec::RetLen { c_type } => format!("{c_type}*"),
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
    /// `bytes` — the call returns a pointer, and a `ret_len:` parameter says how
    /// far it runs. Copied out, because the memory is the library's or the
    /// caller's blob and neither is Jade's to hold.
    Bytes,
    /// `handle<T>` — the library's `T*`, wrapped for Jade to hold.
    Handle(String),
}

impl RetSpec {
    /// How it is spelled in the `extern` prototype.
    fn c_decl(&self) -> String {
        match self {
            RetSpec::Nil => "void".to_string(),
            RetSpec::Scalar(t) => t.decl.to_string(),
            RetSpec::Bytes => "const void*".to_string(),
            RetSpec::Handle { 0: name } => format!("{name}*"),
        }
    }
}

fn parse_ret(pkg: &str, sym: &str, spec: &str) -> Result<RetSpec, String> {
    if spec == "nil" {
        return Ok(RetSpec::Nil);
    }
    if spec == "bytes" {
        return Ok(RetSpec::Bytes);
    }
    if let Some(name) = handle_target(spec) {
        return check_c_ident(pkg, sym, name, "handle").map(|_| RetSpec::Handle(name.to_string()));
    }
    map_type(spec).map(RetSpec::Scalar).ok_or_else(|| bad_type_msg(pkg, sym, spec))
}

fn parse_arg(pkg: &str, sym: &str, full: &str) -> Result<ArgSpec, String> {
    // An out-parameter may carry `@name`, which is the key it comes back under
    // when a symbol has more than one. `@` cannot occur in a C type or
    // identifier, so splitting here leaves `check_c_ident` guarding exactly what
    // it did before.
    let (spec, out_name) = match full.split_once('@') {
        Some((s, n)) => {
            check_out_name(pkg, sym, n)?;
            (s, Some(n.to_string()))
        }
        None => (full, None),
    };

    if let Some(elem) = spec.strip_prefix("out_buffer:") {
        return check_c_ident(pkg, sym, elem, "out_buffer").map(|_| ArgSpec::OutBuffer {
            elem: elem.to_string(),
            name: out_name,
        });
    }
    if let Some(name) = spec.strip_prefix("out_struct:") {
        return check_c_ident(pkg, sym, name, "out_struct").map(|_| ArgSpec::OutStruct {
            type_name: name.to_string(),
            name: out_name,
        });
    }
    if let Some(name) = spec.strip_prefix("in_struct:") {
        // `@name` is the key a *result* comes back under, and an in_struct is an
        // argument. Accepting one and dropping it would read as a result that
        // never arrives.
        if out_name.is_some() {
            return Err(format!(
                "dependency '{pkg}': symbol '{sym}' names an `in_struct:{name}` with `@`, which \
                 is how an out-parameter says what key it comes back under. An `in_struct` is an \
                 argument and produces nothing."
            ));
        }
        return check_c_ident(pkg, sym, name, "in_struct").map(|_| ArgSpec::InStruct {
            type_name: name.to_string(),
        });
    }
    if let Some(name) = spec.strip_prefix("out_handle:") {
        return check_c_ident(pkg, sym, name, "out_handle").map(|_| ArgSpec::OutHandle {
            type_name: name.to_string(),
            name: out_name,
        });
    }
    for (prefix, inout) in [("out_scalar:", false), ("inout_scalar:", true), ("ret_len:", false)] {
        let Some(t) = spec.strip_prefix(prefix) else { continue };
        let what = prefix.trim_end_matches(':');
        check_c_ident(pkg, sym, t, what)?;
        // Must be a type the shim can declare a local of and read back. A
        // `char*` passes `c_scalar` as a string, and a string written through a
        // pointer is an ownership question the header does not answer — who
        // frees it — so it is refused by name rather than guessed at.
        match c_scalar(t) {
            Some(("JADE_FFI_STR", _)) | None => {
                return Err(format!(
                    "dependency '{pkg}': symbol '{sym}' has `{what}:{t}`, which is not a scalar                      the shim can declare and read back. Use a numeric or boolean C type; a                      string written through a pointer is not supported, because the header does                      not say who frees it."
                ));
            }
            Some(_) => {}
        }
        if prefix == "ret_len:" {
            // It sizes the return value rather than becoming a result, so there
            // is no key for a name to be.
            if out_name.is_some() {
                return Err(format!(
                    "dependency '{pkg}': symbol '{sym}' names a `ret_len:{t}` with `@`. It sizes \
                     the returned blob rather than coming back on its own, so there is no key \
                     for the name to be."
                ));
            }
            return Ok(ArgSpec::RetLen { c_type: t.to_string() });
        }
        return Ok(if inout {
            ArgSpec::InoutScalar { c_type: t.to_string(), name: out_name }
        } else {
            ArgSpec::OutScalar { c_type: t.to_string(), name: out_name }
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
    if spec == "bytes_ptr" {
        return Ok(ArgSpec::BytesPtr);
    }
    if spec == "inout_bytes" {
        return Ok(ArgSpec::InoutBytes { name: out_name });
    }
    map_type(spec).map(ArgSpec::Scalar).ok_or_else(|| bad_type_msg(pkg, sym, spec))
}

/// Refuse an out-parameter name that could not be a struct key.
///
/// It becomes a `strdup("…")` literal in the generated shim and a field name in
/// Jade, so it has to be an ordinary identifier. `ret` is reserved: a symbol
/// with a return value *and* named outs puts the return under that key.
fn check_out_name(pkg: &str, sym: &str, name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' names an out-parameter `@{name}`, which is not an              identifier. It becomes a field name on the result."
        ));
    }
    if name == "ret" {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' names an out-parameter `@ret`, which is reserved              for the C return value on a symbol that has both."
        ));
    }
    Ok(())
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

/// Emitted only when some symbol takes a struct as input.
const FIELD_HELPER: &str = r#"
/* One field of a Jade struct by name, or NULL when it has no such key.
 *
 * Linear because these are tiny — a bound struct carries the handful of fields
 * the field table names, not a whole record — and a scan of six keys costs less
 * than building an index would. */
static const JadeVal* jade_shim_field(const JadeStruct* s, const char* key) {
    if (!s || !s->keys) return NULL;
    for (size_t i = 0; i < s->len; i++)
        if (s->keys[i] && strcmp(s->keys[i], key) == 0) return &s->vals[i];
    return NULL;
}

/* Whether `key` is one of the `n` field names the C type has. */
static int jade_shim_known(const char* const* names, size_t n, const char* key) {
    if (!key) return 0;
    for (size_t i = 0; i < n; i++)
        if (strcmp(names[i], key) == 0) return 1;
    return 0;
}

/* Name the field a caller wrote that the C type does not have.
 *
 * _Thread_local for the reason the errno buffer is: Jade tasks are real OS
 * threads, and the ABI only requires an output string to outlive the call,
 * which both engines satisfy by copying before they return. */
static const char* jade_shim_nofield(const char* type_name, const char* key) {
    static _Thread_local char buf[192];
    snprintf(buf, sizeof buf, "%s has no field '%s'", type_name, key ? key : "");
    return buf;
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
    /// Positions in `args` of the out-parameters, in declaration order. More
    /// than one is allowed, and then each carries a name to come back under.
    outs: Vec<usize>,
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
        .filter(|(_, a)| a.produces_result())
        .map(|(i, _)| i)
        .collect();

    // Two results that both consume the C return value cannot coexist: an
    // out_buffer reads it as an element count, an out_handle folds it into the
    // failure convention, and there is only one of it.
    let consumes_ret = |a: &ArgSpec| matches!(a, ArgSpec::OutBuffer { .. } | ArgSpec::OutHandle { .. });
    if outs.iter().filter(|&&i| consumes_ret(&args[i])).count() > 1 {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' has two out-parameters that both read the C \
             return value — an `out_buffer` takes it as an element count and an `out_handle` \
             folds it into the failure convention. Only one of them can."
        ));
    }

    // With more than one out, each has to say what key it comes back under.
    // The generator takes those from the header's own parameter names; a
    // hand-written table has to supply them.
    if outs.len() > 1 {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for &i in &outs {
            let Some(n) = args[i].out_name() else {
                return Err(format!(
                    "dependency '{pkg}': symbol '{sym}' has {} out-parameters, so each one needs \
                     a name to come back under — write `out_scalar:size_t@written`. With one \
                     out-parameter the name is optional, because there is nothing to tell apart.",
                    outs.len()
                ));
            };
            if !seen.insert(n) {
                return Err(format!(
                    "dependency '{pkg}': symbol '{sym}' names two out-parameters `@{n}`. They \
                     become keys on one result, so the names have to differ."
                ));
            }
        }
    }

    // A returned blob and the parameter that sizes it only mean anything
    // together. One without the other is a manifest that says two different
    // things about what comes back, so both directions are refused by name.
    let ret_lens = args.iter().filter(|a| matches!(a, ArgSpec::RetLen { .. })).count();
    if ret_lens > 1 {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' has {ret_lens} `ret_len` parameters. There is one \
             returned pointer, so only one of them can say how far it runs."
        ));
    }
    if ret_lens == 1 && spec.ret != "bytes" {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' has a `ret_len` but returns '{}'. A `ret_len` \
             sizes a returned pointer, so the return type must be `bytes`.",
            spec.ret
        ));
    }
    if ret_lens == 0 && spec.ret == "bytes" {
        return Err(format!(
            "dependency '{pkg}': symbol '{sym}' returns `bytes` but nothing says how long it is. \
             Mark the parameter the length is written through as `ret_len:<ctype>`."
        ));
    }

    // An `in_struct` needs exactly what an `out_struct` needs, and for the same
    // reason: a field list saying what to carry, and the real header so the
    // compiler places those fields rather than a hand-written layout guessing.
    // Checked over every argument rather than over `outs`, because this one is
    // an argument.
    for a in &args {
        let ArgSpec::InStruct { type_name } = a else { continue };
        if !structs.contains_key(type_name) {
            return Err(format!(
                "dependency '{pkg}': symbol '{sym}' takes an `in_struct:{type_name}`, but there is \
                 no [dependencies.{pkg}.structs.{type_name}] table saying which fields to fill."
            ));
        }
        if headers.is_empty() {
            return Err(format!(
                "dependency '{pkg}': symbol '{sym}' takes an `in_struct:{type_name}`, so the \
                 dependency needs `headers = [\"<the library's header>\"]`. The shim has to \
                 declare a real {type_name}, and taking its layout from the field list instead \
                 would write at the wrong offsets whenever the two disagree."
            ));
        }
    }

    for &i in &outs {
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
            ArgSpec::OutStruct { type_name, .. } => {
                if !structs.contains_key(type_name) {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' fills an `out_struct:{type_name}`, but \
                         there is no [dependencies.{pkg}.structs.{type_name}] table saying which \
                         fields to read back."
                    ));
                }
                if headers.is_empty() {
                    return Err(format!(
                        "dependency '{pkg}': symbol '{sym}' fills an `out_struct:{type_name}`, so the \
                         dependency needs `headers = [\"<the library's header>\"]`. The shim has \
                         to declare a real {type_name}, and taking its layout from the field list \
                         instead would write at the wrong offsets whenever the two disagree."
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(Parsed { args, outs })
}

/// Whether the C return value becomes a key of its own.
///
/// Two out shapes consume it instead: an `out_buffer` reads it as the element
/// count that sizes the blob, and an `out_handle`'s return is a status that
/// feeds `fails_when`. Both already did this; naming the rule is what lets the
/// multi-out case reuse it rather than re-derive it.
fn ret_is_a_key(ret: &RetSpec, outs: &[&ArgSpec]) -> bool {
    !matches!(ret, RetSpec::Nil)
        && !outs
            .iter()
            .any(|a| matches!(a, ArgSpec::OutBuffer { .. } | ArgSpec::OutHandle { .. }))
}

/// Whether the result is a keyed struct rather than a single value.
///
/// Jade has one result slot, so anything with two things to hand back needs a
/// struct. Counting "the return value, if it is a key, plus the
/// out-parameters" reproduces every existing shape exactly — one out and a void
/// return is still the bare value, one out and a real return is still
/// `.ret`/`.out` — and generalizes to any number.
fn builds_result_struct(ret_key: bool, n_outs: usize) -> bool {
    usize::from(ret_key) + n_outs > 1
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
    let out_specs = || parsed.iter().flat_map(|p| p.outs.iter().map(|&i| &p.args[i]));
    let returns_bytes = names.iter().any(|s| symbols[*s].ret == "bytes");
    if returns_bytes
        || out_specs().any(|a| matches!(a, ArgSpec::OutBuffer { .. } | ArgSpec::InoutBytes { .. }))
    {
        out.push_str(BYTES_HELPER);
    }
    if parsed.iter().any(|p| p.args.iter().any(|a| matches!(a, ArgSpec::InStruct { .. }))) {
        out.push_str(FIELD_HELPER);
    }
    // A struct is built for an `out_struct`, and now also whenever a symbol has
    // more than one thing to hand back — which needs no out_struct at all.
    let mut needs_struct = out_specs().any(|a| matches!(a, ArgSpec::OutStruct { .. }));
    for (sym, p) in names.iter().zip(&parsed) {
        let ret_t = parse_ret(name, sym, &symbols[*sym].ret)?;
        let outs: Vec<&ArgSpec> = p.outs.iter().map(|&i| &p.args[i]).collect();
        needs_struct |= builds_result_struct(ret_is_a_key(&ret_t, &outs), outs.len());
    }
    if needs_struct {
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

/// Copy a Jade struct argument into a real C local of the library's own type.
///
/// A field the caller left out stays as the zero the `memset` put there, which
/// is what the C it stands in for does: declare, zero, set what matters. A
/// struct like `lzma_stream_flags` carries fifteen reserved fields the library
/// requires to be zero, and demanding all of them would make the shape
/// unusable.
///
/// A field the caller wrote that the type does not *have* is refused, by name.
/// That is the mistake worth catching — a misspelling would otherwise be
/// indistinguishable from an omission, and silently become a zero the caller
/// believed they had set.
fn emit_in_struct(
    pkg: &str,
    sym: &str,
    var: &str,
    type_name: &str,
    def: &CStruct,
    at: usize,
) -> Result<String, String> {
    let names: Vec<String> = def.fields.iter().map(|(f, _)| format!("\"{f}\"")).collect();
    let mut b = format!(
        "    {type_name} {var};\n\
         \x20   memset(&{var}, 0, sizeof {var});\n\
         \x20   const JadeStruct* {var}_s = argv[{at}].data.as_struct;\n\
         \x20   static const char* const {var}_names[] = {{ {} }};\n\
         \x20   for (size_t {var}_i = 0; {var}_s && {var}_i < {var}_s->len; {var}_i++) {{\n\
         \x20       if (jade_shim_known({var}_names, {}, {var}_s->keys[{var}_i])) continue;\n\
         \x20       out->tag = JADE_FFI_ERROR;\n\
         \x20       out->data.as_str = jade_shim_nofield(\"{type_name}\", {var}_s->keys[{var}_i]);\n\
         \x20       return 1;\n\
         \x20   }}\n",
        names.join(", "),
        def.fields.len()
    );
    for (n, (field, ty)) in def.fields.iter().enumerate() {
        let t = map_type(ty).ok_or_else(|| {
            format!(
                "dependency '{pkg}': symbol '{sym}' fills field '{field}' of {type_name} from \
                 '{ty}', which the Jade FFI cannot represent. Supported types are \
                 {SUPPORTED_TYPES}."
            )
        })?;
        b.push_str(&format!(
            "    const JadeVal* {var}_{n} = jade_shim_field({var}_s, \"{field}\");\n\
             \x20   if ({var}_{n}) {{\n\
             \x20       if ({var}_{n}->tag != {}) {{\n\
             \x20           out->tag = JADE_FFI_ERROR;\n\
             \x20           out->data.as_str = \"{sym}: field '{field}' of {type_name} must be a {ty}\";\n\
             \x20           return 1;\n\
             \x20       }}\n\
             \x20       {var}.{field} = ({}){var}_{n}->data.{};\n\
             \x20   }}\n",
            t.tag, t.decl, t.field
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
            ArgSpec::Bytes | ArgSpec::BytesPtr | ArgSpec::InoutBytes { .. } => {
                body.push_str(&format!("    if (argv[{j}].tag != JADE_FFI_BYTES) return 1;\n"));
            }
            ArgSpec::Callback { .. } => {
                body.push_str(&format!("    if (argv[{j}].tag != JADE_FFI_FN) return 1;\n"));
            }
            ArgSpec::InStruct { .. } => {
                body.push_str(&format!("    if (argv[{j}].tag != JADE_FFI_STRUCT) return 1;\n"));
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
            // The only out-parameter that also takes a value in, so it is
            // checked like an ordinary argument here and seeded from `argv`
            // below. Falling into the catch-all left it with no index at all.
            ArgSpec::InoutScalar { c_type, .. } => {
                let (tag, _) = c_scalar(c_type).expect("validated by parse_arg");
                body.push_str(&format!("    if (argv[{j}].tag != {tag}) return 1;\n"));
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
    // Each local is suffixed with the parameter's position, so a symbol with
    // two out-structs declares two distinct locals rather than the same name
    // twice.
    let mut cleanup = String::new();
    for (i, a) in p.args.iter().enumerate() {
        match a {
            ArgSpec::OutBuffer { elem, .. } => {
                let count_at = jade_idx[i + 1].expect("validated: the next arg is an int");
                body.push_str(&format!(
                    "    int64_t n_elem{i} = argv[{count_at}].data.as_int;\n\
                     \x20   if (n_elem{i} < 0) return 1;\n\
                     \x20   {elem}* obuf{i} = ({elem}*)malloc((size_t)(n_elem{i} ? n_elem{i} : 1) * sizeof({elem}));\n\
                     \x20   if (!obuf{i}) return 1;\n"
                ));
                // Accumulated, not replaced: a symbol with two pieces of scratch
                // must release both on the raise path, and assigning here leaked
                // whichever one was declared first.
                cleanup.push_str(&format!(" free(obuf{i});"));
            }
            ArgSpec::InoutBytes { .. } => {
                // The library edits in place, so it gets scratch of the caller's
                // own bytes rather than the caller's bytes. A Jade blob is
                // immutable; handing one over to be scribbled on would make that
                // untrue for the FFI's convenience.
                let k = jade_idx[i].expect("an inout_bytes consumes a Jade argument");
                body.push_str(&format!(
                    "    size_t iolen{i} = argv[{k}].data.as_bytes ? argv[{k}].data.as_bytes->len : (size_t)0;\n\
                     \x20   void* iobuf{i} = malloc(iolen{i} ? iolen{i} : 1);\n\
                     \x20   if (!iobuf{i}) {{{cleanup} return 1; }}\n\
                     \x20   if (iolen{i}) memcpy(iobuf{i}, argv[{k}].data.as_bytes->data, iolen{i});\n"
                ));
                cleanup.push_str(&format!(" free(iobuf{i});"));
            }
            ArgSpec::OutStruct { type_name, .. } => {
                // Zeroed, because a library is entitled to fill only the fields
                // it knows about and leave the rest as the caller set them.
                body.push_str(&format!(
                    "    {type_name} ostruct{i};\n    memset(&ostruct{i}, 0, sizeof ostruct{i});\n"
                ));
            }
            ArgSpec::OutHandle { type_name, .. } => {
                // Null to start, so a library that fails without writing leaves
                // something recognisable rather than a stack pointer.
                body.push_str(&format!("    {type_name}* ohandle{i} = NULL;\n"));
            }
            ArgSpec::InStruct { type_name } => {
                let k = jade_idx[i].expect("an in_struct consumes a Jade argument");
                let def = &structs[type_name];
                body.push_str(&emit_in_struct(pkg, sym, &format!("istruct{i}"), type_name, def, k)?);
            }
            ArgSpec::OutScalar { c_type, .. } => {
                body.push_str(&format!("    {c_type} oscalar{i} = ({c_type})0;\n"));
            }
            ArgSpec::RetLen { c_type } => {
                body.push_str(&format!("    {c_type} rlen{i} = ({c_type})0;\n"));
            }
            ArgSpec::InoutScalar { c_type, .. } => {
                // Seeded from what Jade passed, not zeroed: this is the shape
                // where the caller sets a position and the library advances it.
                let k = jade_idx[i].expect("an inout_scalar consumes a Jade argument");
                let (_, field) = c_scalar(c_type).expect("validated by parse_arg");
                body.push_str(&format!(
                    "    {c_type} oscalar{i} = ({c_type})argv[{k}].data.{field};\n"
                ));
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
            ArgSpec::BytesPtr => {
                let k = jade_idx[i].unwrap();
                call_args.push(format!(
                    "argv[{k}].data.as_bytes ? (const void*)argv[{k}].data.as_bytes->data : NULL"
                ));
            }
            ArgSpec::OutBuffer { .. } => call_args.push(format!("obuf{i}")),
            ArgSpec::OutStruct { .. } => call_args.push(format!("&ostruct{i}")),
            ArgSpec::InStruct { .. } => call_args.push(format!("&istruct{i}")),
            ArgSpec::OutScalar { .. } | ArgSpec::InoutScalar { .. } => {
                call_args.push(format!("&oscalar{i}"))
            }
            ArgSpec::RetLen { .. } => call_args.push(format!("&rlen{i}")),
            ArgSpec::InoutBytes { .. } => call_args.push(format!("iobuf{i}")),
            ArgSpec::Handle { name } => {
                call_args.push(format!("({name}*)h{}", jade_idx[i].unwrap()))
            }
            ArgSpec::OutHandle { .. } => call_args.push(format!("&ohandle{i}")),
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
            // The pointer belongs to the library, or to the caller's own blob,
            // and neither is Jade's to hold — so it is copied out. A NULL return
            // or a negative length is how these signal "nothing", which is nil.
            RetSpec::Bytes => {
                let at = p
                    .args
                    .iter()
                    .position(|a| matches!(a, ArgSpec::RetLen { .. }))
                    .expect("validated by parse_symbol");
                format!(
                    "    if (!r || (int64_t)rlen{at} < 0) {{\n\
                     \x20       {target}tag = JADE_FFI_NIL;\n\
                     \x20       {target}data.as_nil = 0;\n\
                     \x20   }} else {{\n\
                     \x20       JadeBytes* rb{at} = jade_shim_bytes(r, (size_t)rlen{at});\n\
                     \x20       if (!rb{at}) return 1;\n\
                     \x20       {target}tag = JADE_FFI_BYTES;\n\
                     \x20       {target}data.as_bytes = rb{at};\n\
                     \x20   }}\n"
                )
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
    //
    // One value goes straight into `out`; two or more become a keyed struct.
    // The counting rule is in `builds_result_struct`, and it reproduces every
    // shape that existed before: a bare return, a lone filled buffer, a lone
    // handle, and the `.ret`/`.out` pair.
    let outs: Vec<&ArgSpec> = p.outs.iter().map(|&i| &p.args[i]).collect();
    let ret_key = ret_is_a_key(&ret_t, &outs);

    // Each out-parameter's value, written into a slot named by `target`.
    let emit_out = |target: &str, i: usize, body: &mut String| -> Result<(), String> {
        match &p.args[i] {
            ArgSpec::OutBuffer { elem, .. } => {
                body.push_str(&format!(
                    "    /* Clamp: a library reporting more than it was given would\n\
                     \x20    * otherwise make this read past the scratch. */\n\
                     \x20   int64_t got{i} = r < 0 ? 0 : (r > n_elem{i} ? n_elem{i} : r);\n\
                     \x20   JadeBytes* b{i} = jade_shim_bytes(obuf{i}, (size_t)got{i} * sizeof({elem}));\n\
                     \x20   free(obuf{i});\n\
                     \x20   if (!b{i}) return 1;\n\
                     \x20   {target}tag = JADE_FFI_BYTES;\n\
                     \x20   {target}data.as_bytes = b{i};\n"
                ));
            }
            ArgSpec::OutStruct { type_name, .. } => {
                let def = &structs[type_name];
                body.push_str(&emit_out_struct(pkg, sym, &format!("ostruct{i}"), type_name, def, "")?);
                body.push_str(&format!(
                    "    {target}tag = JADE_FFI_STRUCT;\n\
                     \x20   {target}data.as_struct = ostruct{i}_j;\n"
                ));
            }
            ArgSpec::OutHandle { type_name, .. } => {
                // A library that failed without writing leaves the null it
                // started as, and nil is what that means to Jade.
                body.push_str(&format!(
                    "    if (!ohandle{i}) {{\n\
                     \x20       {target}tag = JADE_FFI_NIL;\n\
                     \x20       {target}data.as_nil = 0;\n\
                     \x20   }} else {{\n\
                     \x20       JadeHandle* oh{i} = jade_shim_handle((void*)ohandle{i}, \"{type_name}\");\n\
                     \x20       if (!oh{i}) return 1;\n\
                     \x20       {target}tag = JADE_FFI_HANDLE;\n\
                     \x20       {target}data.as_handle = oh{i};\n\
                     \x20   }}\n"
                ));
            }
            ArgSpec::OutScalar { c_type, .. } | ArgSpec::InoutScalar { c_type, .. } => {
                let (tag, field) = c_scalar(c_type).expect("validated by parse_arg");
                body.push_str(&format!(
                    "    {target}tag = {tag};\n    {target}data.{field} = oscalar{i};\n"
                ));
            }
            ArgSpec::InoutBytes { .. } => {
                // The whole buffer comes back, edited. Its length cannot change:
                // the library was given exactly what the caller had, so anything
                // that needs to grow reports no space rather than reallocating.
                body.push_str(&format!(
                    "    JadeBytes* io{i} = jade_shim_bytes(iobuf{i}, iolen{i});\n\
                     \x20   free(iobuf{i});\n\
                     \x20   if (!io{i}) return 1;\n\
                     \x20   {target}tag = JADE_FFI_BYTES;\n\
                     \x20   {target}data.as_bytes = io{i};\n"
                ));
            }
            _ => unreachable!("only out-parameters land here"),
        }
        Ok(())
    };

    // The C return value is dead when an out-parameter consumed it, and `-Wall
    // -Werror` is on, so say so.
    if !matches!(ret_t, RetSpec::Nil) && !ret_key && !outs.iter().any(|a| matches!(a, ArgSpec::OutBuffer { .. })) {
        body.push_str("    (void)r;\n");
    }

    if !builds_result_struct(ret_key, outs.len()) {
        match p.outs.first() {
            None => body.push_str(&emit_ret("out->")),
            Some(&i) => emit_out("out->", i, &mut body)?,
        }
    } else {
        let n = usize::from(ret_key) + p.outs.len();
        body.push_str(&format!(
            "    JadeStruct* res = jade_shim_struct(\"{sym}_result\", {n});\n\
             \x20   if (!res) return 1;\n"
        ));
        let mut k = 0usize;
        if ret_key {
            body.push_str(&emit_ret(&format!("res->vals[{k}].")));
            body.push_str(&format!("    res->keys[{k}] = strdup(\"ret\");\n"));
            k += 1;
        }
        for &i in &p.outs {
            // One out-parameter alongside a return value keeps the name `out`,
            // which is what it has always come back under. More than one, and
            // each carries its own name from the header.
            let key = match p.args[i].out_name() {
                Some(n) => n.to_string(),
                None => "out".to_string(),
            };
            emit_out(&format!("res->vals[{k}]."), i, &mut body)?;
            body.push_str(&format!("    res->keys[{k}] = strdup(\"{key}\");\n"));
            k += 1;
        }
        body.push_str("    out->tag = JADE_FFI_STRUCT;\n");
        body.push_str("    out->data.as_struct = res;\n");
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
