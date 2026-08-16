//! The VM's runtime value type ([`VmValue`]) and its display / type-name helpers.
//!
//! `VmValue` is the interpreter's representation of a Jade value. Value *semantics*
//! (arithmetic, formatting, coercion) live in the shared `jade_runtime` crate so
//! the VM and AOT backend cannot drift; what lives here is the enum the VM
//! dispatches on plus the two user-facing string projections.

use super::*;

/// Identifies a native (Rust-backed) callable stored inside a module dict.
/// Adding a new package method = adding a variant here + a match arm in `call_value`.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeFnId {
    Print,
    Route,
    /// `array.map(arr, fn)` / `array.filter(arr, fn)` — need VmState + async to
    /// call the user function per element, so they dispatch here rather than as
    /// pure BuiltinFns (which can't run Jade code).
    ArrayMap,
    ArrayFilter,
    /// `uhttp.stream(url, handler, headers?)` — stream an HTTP response over a
    /// Unix socket, invoking a Jade handler per line.
    UhttpStream,
}

/// A value at VM runtime, carrying `Arc<CompiledFn>` for functions so the VM
/// can execute them without re-running the emitter.
#[derive(Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A single Unicode scalar. Immediate, like `Int`/`Bool` — indexing or
    /// iterating a string yields these, so a scan allocates nothing. Carries a
    /// trust byte for the same reason `Str` does: a character of a tainted
    /// string is still tainted.
    Char(jade_runtime::trust::JChar),
    /// A binary blob. Distinct from `Str`: Jade strings are UTF-8 and
    /// NUL-terminated and arbitrary bytes are neither, so conversion is
    /// explicit in both directions. Shares `BytesObj` with the AOT heap.
    Bytes(Arc<jade_runtime::bytesf::BytesObj>),
    /// An opaque pointer handed over by a native package — a `sqlite3*`, a
    /// `SNDFILE*`. Jade holds it and passes it back; it never looks inside and
    /// never frees the pointee. Carries the C type it came from so a
    /// `sqlite3_stmt` cannot be passed where a `sqlite3` is expected. Shares
    /// `HandleObj` with the AOT heap.
    Handle(Arc<jade_runtime::handle::HandleObj>),
    /// A string plus where it came from. The trust byte is the same one
    /// compiled code keeps in the string header — the interpreter tracked no
    /// trust at all, so `sh.exec(sh.exec("..."))` ran under `jade run` and was
    /// refused under `jade build`. See `jade_runtime::trust`.
    Str(JStr),
    Fn(Arc<CompiledFn>),
    /// A closure: compiled function + snapshot of globals at creation time.
    Closure(Arc<CompiledFn>, Arc<HashMap<String, VmValue>>),
    Struct(Arc<Mutex<StructObj<VmValue>>>),
    BoundMethod(Arc<VmBoundMethod>),
    /// Reference-counted array — mutations are visible to all aliases.
    Array(Arc<Mutex<ArrayObj<VmValue>>>),
    Prompt(String),
    /// A user-defined GBNF pattern (RHS only, e.g. `"yes" | "no"`).
    /// Used with `?p |> grammar_var` to constrain LLM token sampling.
    ///
    /// This holds the *same* [`GrammarObj`] the AOT backend allocates on its
    /// heap — the first of the value types to be collapsed onto one shared
    /// representation. Read its GBNF via `GrammarObj::to_gbnf()`; do not
    /// re-derive it from `.pattern` at the use site, which is how the two
    /// engines drifted apart in the first place.
    Grammar(Arc<GrammarObj>),
    Dict(DictObj<VmValue>),
    /// A pure Rust-backed callable (no VM state mutation). Used for builtin
    /// core built-ins (print, len, write, input) and package functions.
    BuiltinFn(BuiltinFn),
    /// A BuiltinFn pre-loaded with its receiver for primitive method dispatch.
    NativeBoundMethod(Arc<NativeBoundMethod>),
    /// A Rust-backed callable returned by a built-in module (e.g. `array.map`).
    NativeFn(NativeFnId),
    /// A [`NativeFn`](VmValue::NativeFn) pre-loaded with its receiver, so
    /// `a.map(f)` reaches the same implementation as `array.map(a, f)`.
    ///
    /// [`NativeBoundMethod`] cannot do this job: it binds a *pure* `BuiltinFn`,
    /// and `map`/`filter` call a Jade function per element, which needs the VM's
    /// call context. That is the whole reason those two were the only array
    /// functions with no method spelling until v1.3.21.
    BoundNativeFn(Arc<(NativeFnId, VmValue)>),
    /// A function loaded from a native shared library registered as a `[lib]`
    /// module whose file is a `.dylib`/`.so`/`.dll`.
    NativeLibFn(Arc<NativeLibFn>),
    /// A handle to an in-flight async task.
    Future(Arc<JadeFuture>),
    /// A lazy token stream from an untyped prompt dereference.
    TokenStream(Arc<JadeTokenStream>),
    /// A buffered sequence produced by a `yield`ing function.
    ///
    /// A stream *is* a buffer: everything produced is retained, so reading it
    /// twice gives the same values twice. There is no one-shot rule to
    /// remember and no "already drained" error to hit.
    Stream(Arc<Mutex<Vec<VmValue>>>),
    /// A first-class type value. Callable with one argument for coercion/construction:
    /// `int("3")` → 3, `City(dict)` → City struct, etc.
    TypeRef(String),
    Nil,
}

pub struct VmBoundMethod {
    pub receiver: Arc<Mutex<StructObj<VmValue>>>,
    pub method: Arc<CompiledFn>,
}

impl std::fmt::Debug for VmValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmValue::Int(i) => write!(f, "Int({})", i),
            VmValue::Float(v) => write!(f, "Float({})", v),
            VmValue::Bool(b) => write!(f, "Bool({})", b),
            VmValue::Char(c) => write!(f, "Char({:?})", c.ch()),
            VmValue::Bytes(b) => write!(f, "Bytes[{} byte(s)]", b.len()),
            VmValue::Handle(h) => write!(f, "{:?}", h),
            VmValue::Str(s) => write!(f, "Str({:?})", s),
            VmValue::Fn(cf) => write!(f, "Fn({})", cf.params.join(", ")),
            VmValue::Closure(cf, _) => write!(f, "Closure({})", cf.params.join(", ")),
            VmValue::Struct(rc) => {
                let inst = rc.lock();
                write!(f, "{} {{...}}", inst.type_name())
            }
            VmValue::BoundMethod(_) => write!(f, "<bound method>"),
            VmValue::Array(arc) => write!(f, "Array[{} elem(s)]", arc.lock().len()),
            VmValue::Prompt(s) => write!(f, "Prompt({:?})", s),
            VmValue::Grammar(g) => match &g.anchor {
                None => write!(f, "Grammar({:?})", g.pattern),
                Some(a) => write!(f, "Grammar({:?}, anchor={:?})", g.pattern, a),
            },
            VmValue::Dict(m) => write!(f, "Dict({} key(s))", m.len()),
            VmValue::BuiltinFn(bf) => write!(f, "BuiltinFn({})", bf.name),
            VmValue::NativeBoundMethod(nbm) => write!(f, "NativeBoundMethod({})", nbm.method.name),
            VmValue::NativeFn(nf) => write!(f, "NativeFn({:?})", nf),
            VmValue::BoundNativeFn(b) => write!(f, "BoundNativeFn({:?})", b.0),
            VmValue::NativeLibFn(nfn) => write!(f, "NativeLibFn({})", nfn.name),
            VmValue::Future(_) => write!(f, "Future"),
            VmValue::TokenStream(_) => write!(f, "TokenStream"),
            VmValue::Stream(b) => write!(f, "Stream[{} item(s)]", b.lock().len()),
            VmValue::TypeRef(t) => write!(f, "TypeRef({})", t),
            VmValue::Nil => write!(f, "Nil"),
        }
    }
}

/// Convert a `VmValue` to its user-visible string representation.
pub fn value_to_display(v: &VmValue) -> String {
    // Scalar/collection formatting rules live once in the shared runtime crate
    // (jade_runtime::render) so the VM and the AOT renderer (render_word) cannot
    // drift — same float `.0` rule, same `[a, b]` / sorted-quoted `{"k": v}`
    // framing. Only the per-engine iteration differs (VmValue vs tagged words).
    match v {
        VmValue::Int(i) => i.to_string(),
        VmValue::Float(f) => jade_runtime::render::format_float(*f),
        VmValue::Bool(b) => b.to_string(),
        VmValue::Str(s) => s.to_string(),
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let parts: Vec<String> = guard.iter().map(value_to_display).collect();
            jade_runtime::render::render_array(&parts)
        }
        VmValue::Dict(m) => {
            let mut entries: Vec<(String, String)> =
                m.iter().map(|(k, v)| (k.clone(), value_to_display(v))).collect();
            jade_runtime::render::render_dict(&mut entries)
        }
        VmValue::Fn(_) => "<fn>".to_string(),
        VmValue::Closure(_, _) => "<fn>".to_string(),
        VmValue::Struct(_) => "<struct>".to_string(),
        VmValue::BoundMethod(_) => "<bound method>".to_string(),
        VmValue::BuiltinFn(bf) => format!("<builtin {}>", bf.name),
        VmValue::NativeBoundMethod(nm) => format!("<builtin {}>", nm.method.name),
        VmValue::Prompt(_) => "<prompt>".to_string(),
        VmValue::Grammar(_) => "<grammar>".to_string(),
        VmValue::NativeFn(_) => "<native fn>".to_string(),
        VmValue::BoundNativeFn(_) => "<builtin method>".to_string(),
        VmValue::NativeLibFn(nfn) => format!("<native lib fn {}>", nfn.name),
        VmValue::Future(_) => "<future>".to_string(),
        VmValue::TokenStream(_) => "<token stream>".to_string(),
        VmValue::Stream(b) => {
            let parts: Vec<String> = b.lock().iter().map(value_to_display).collect();
            jade_runtime::render::render_array(&parts)
        }
        VmValue::Char(c) => c.ch().to_string(),
        VmValue::Bytes(b) => jade_runtime::render::render_bytes(b.as_slice()),
        VmValue::Handle(h) => jade_runtime::handle::render(h),
        VmValue::TypeRef(t) => format!("<type {}>", t),
        VmValue::Nil => "nil".to_string(),
    }
}

/// Return the runtime type name of a `VmValue` as a static string.
pub fn value_type_name(v: &VmValue) -> &'static str {
    match v {
        VmValue::Int(_) => "int",
        VmValue::Float(_) => "float",
        VmValue::Bool(_) => "bool",
        VmValue::Char(_) => "char",
        VmValue::Bytes(_) => "bytes",
        // The C type is in the value, not the name — same as `struct`, which
        // reports "struct" rather than "Point". A message that needs to name the
        // specific type reads it off the handle.
        VmValue::Handle(_) => "handle",
        VmValue::Str(_) => "str",
        VmValue::Array(_) => "array",
        VmValue::Dict(_) => "dict",
        VmValue::Struct(_) => "struct",
        VmValue::Fn(_) | VmValue::Closure(_, _) => "fn",
        VmValue::BoundMethod(_) | VmValue::NativeBoundMethod(_) => "method",
        VmValue::BuiltinFn(_) => "builtin",
        VmValue::NativeFn(_) => "native fn",
        VmValue::BoundNativeFn(_) => "method",
        VmValue::NativeLibFn(_) => "native fn",
        VmValue::Future(_) => "future",
        VmValue::TokenStream(_) => "token stream",
        VmValue::Stream(_) => "stream",
        VmValue::TypeRef(_) => "type",
        VmValue::Prompt(_) => "prompt",
        VmValue::Grammar(_) => "grammar",
        VmValue::Nil => "nil",
    }
}

/// Test-only writer that forwards to a shared buffer without holding the lock
/// across `.await` points (each `write` call locks and immediately releases).
/// `Send`-safe because `Arc<Mutex<Vec<u8>>>` is `Send`.
#[cfg(test)]
pub(crate) struct TestWriter(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl std::io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
