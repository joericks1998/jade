use std::{sync::Arc, collections::{HashMap, HashSet}, path::PathBuf};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::{
    compiler::{
        bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
        emit::CompiledProgram,
        stdlib::{self, BuiltinFn, NativeBoundMethod, PrimType},
    },
    frontend::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{JadeError, Result, Span},
    },
    llm,
    native::NativeLibFn,
};

// ── Token budgets (mirror eval.rs) ────────────────────────────────────────────

const DEFAULT_MAX_TOKENS: u32 = 4096;
const RETRY_MAX_TOKENS: u32 = 64;
const RETRY_MAX_TOKENS_COMPLEX: u32 = 512;

// ── Runtime value ─────────────────────────────────────────────────────────────

/// A value at VM runtime.
///
/// Identifies a native (Rust-backed) callable stored inside a module dict.
/// Adding a new package method = adding a variant here + a match arm in `call_value`.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeFnId {
    LlmSetMaxTokens,
    Print,
    Stream,
    Route,
}

/// A value at VM runtime, carrying `Arc<CompiledFn>` for functions so the VM
/// can execute them without re-running the emitter.
#[derive(Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Fn(Arc<CompiledFn>),
    /// A closure: compiled function + snapshot of globals at creation time.
    Closure(Arc<CompiledFn>, Arc<HashMap<String, VmValue>>),
    Struct(Arc<Mutex<VmStruct>>),
    BoundMethod(Arc<VmBoundMethod>),
    /// Reference-counted array — mutations are visible to all aliases.
    Array(Arc<Mutex<Vec<VmValue>>>),
    Prompt(String),
    /// A user-defined GBNF pattern (RHS only, e.g. `"yes" | "no"`).
    /// Used with `?p |> grammar_var` to constrain LLM token sampling.
    /// `anchor`: if set, grammar enforcement begins only after the model emits this string.
    Grammar { pattern: String, anchor: Option<String> },
    Dict(HashMap<String, VmValue>),
    /// A pure Rust-backed callable (no VM state mutation). Used for stdlib
    /// core built-ins (print, len, write, input) and package functions.
    BuiltinFn(BuiltinFn),
    /// A BuiltinFn pre-loaded with its receiver for primitive method dispatch.
    NativeBoundMethod(Arc<NativeBoundMethod>),
    /// A Rust-backed callable returned by a built-in module (e.g. `llm.set_max_tokens`).
    NativeFn(NativeFnId),
    /// A function loaded from a native shared library via `use "native/<pkg>"`.
    NativeLibFn(Arc<NativeLibFn>),
    /// A handle to an in-flight async task.
    Future(Arc<JadeFuture>),
    /// A lazy token stream from an untyped prompt dereference.
    TokenStream(Arc<JadeTokenStream>),
    /// A first-class type value. Callable with one argument for coercion/construction:
    /// `int("3")` → 3, `City(dict)` → City struct, etc.
    TypeRef(String),
    Nil,
}

/// Task result type: (value, token_delta, raised_exception).
/// The third element carries the raised exception value so that parent tasks can
/// re-raise it with the correct type (struct/string) rather than losing it.
type TaskOutput = std::result::Result<(VmValue, i64), JadeError>;
type TaskBundle = (TaskOutput, Option<VmValue>);

/// A handle to a spawned async task.  `Arc<JadeFuture>` is `Send + Sync` because
/// `Mutex` makes the inner `Option<JoinHandle>` safe to share across threads.
pub struct JadeFuture {
    pub handle: Mutex<Option<JoinHandle<TaskBundle>>>,
}

/// A lazy, in-flight token stream from an inference call.
/// Wrapping in `Arc` makes it cloneable as a `VmValue`; the interior `Option`
/// enforces single-drain semantics — taking `None` on a second drain is an error.
pub struct JadeTokenStream {
    pub rx: Mutex<Option<tokio::sync::mpsc::Receiver<String>>>,
    pub tokens_handle: Mutex<Option<JoinHandle<Result<i64>>>>,
    pub prompt_key: (String, Option<String>),
}

impl Drop for JadeFuture {
    fn drop(&mut self) {
        // Abort any un-awaited task so it does not run forever as a detached thread.
        let guard = self.handle.get_mut();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }
}

pub struct VmStruct {
    pub type_name: String,
    pub fields: HashMap<String, VmValue>,
}

pub struct VmBoundMethod {
    pub receiver: Arc<Mutex<VmStruct>>,
    pub method: Arc<CompiledFn>,
}

impl std::fmt::Debug for VmValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmValue::Int(i)   => write!(f, "Int({})", i),
            VmValue::Float(v) => write!(f, "Float({})", v),
            VmValue::Bool(b)  => write!(f, "Bool({})", b),
            VmValue::Str(s)   => write!(f, "Str({:?})", s),
            VmValue::Fn(cf)   => write!(f, "Fn({})", cf.params.join(", ")),
            VmValue::Closure(cf, _) => write!(f, "Closure({})", cf.params.join(", ")),
            VmValue::Struct(rc) => {
                let inst = rc.lock();
                write!(f, "{} {{...}}", inst.type_name)
            }
            VmValue::BoundMethod(_) => write!(f, "<bound method>"),
            VmValue::Array(arc) => write!(f, "Array[{} elem(s)]", arc.lock().len()),
            VmValue::Prompt(s)   => write!(f, "Prompt({:?})", s),
            VmValue::Grammar { pattern, anchor: None }    => write!(f, "Grammar({:?})", pattern),
            VmValue::Grammar { pattern, anchor: Some(a) } => write!(f, "Grammar({:?}, anchor={:?})", pattern, a),
            VmValue::Dict(m)     => write!(f, "Dict({} key(s))", m.len()),
            VmValue::BuiltinFn(bf) => write!(f, "BuiltinFn({})", bf.name),
            VmValue::NativeBoundMethod(nbm) => write!(f, "NativeBoundMethod({})", nbm.method.name),
            VmValue::NativeFn(nf) => write!(f, "NativeFn({:?})", nf),
            VmValue::NativeLibFn(nfn) => write!(f, "NativeLibFn({})", nfn.name),
            VmValue::Future(_)       => write!(f, "Future"),
            VmValue::TokenStream(_)  => write!(f, "TokenStream"),
            VmValue::TypeRef(t)      => write!(f, "TypeRef({})", t),
            VmValue::Nil             => write!(f, "Nil"),
        }
    }
}

// ── Public display helper ─────────────────────────────────────────────────────

/// Convert a `VmValue` to its user-visible string representation.
pub fn value_to_display(v: &VmValue) -> String {
    match v {
        VmValue::Int(i) => i.to_string(),
        VmValue::Float(f) => {
            let s = format!("{}", f);
            if s.chars().all(|c| c.is_ascii_digit() || c == '-') {
                format!("{}.0", s)
            } else {
                s
            }
        }
        VmValue::Bool(b)   => b.to_string(),
        VmValue::Str(s)    => s.clone(),
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let parts: Vec<String> = guard.iter().map(value_to_display).collect();
            format!("[{}]", parts.join(", "))
        }
        VmValue::Dict(m) => {
            let mut pairs: Vec<_> = m.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            let parts: Vec<String> = pairs.iter()
                .map(|(k, v)| format!("{:?}: {}", k, value_to_display(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        VmValue::Fn(_)                 => "<fn>".to_string(),
        VmValue::Closure(_, _)         => "<fn>".to_string(),
        VmValue::Struct(_)             => "<struct>".to_string(),
        VmValue::BoundMethod(_)        => "<bound method>".to_string(),
        VmValue::BuiltinFn(bf)         => format!("<builtin {}>", bf.name),
        VmValue::NativeBoundMethod(nm) => format!("<builtin {}>", nm.method.name),
        VmValue::Prompt(_)             => "<prompt>".to_string(),
        VmValue::Grammar { .. }        => "<grammar>".to_string(),
        VmValue::NativeFn(_)           => "<native fn>".to_string(),
        VmValue::NativeLibFn(nfn)      => format!("<native lib fn {}>", nfn.name),
        VmValue::Future(_)             => "<future>".to_string(),
        VmValue::TokenStream(_)        => "<token stream>".to_string(),
        VmValue::TypeRef(t)            => format!("<type {}>", t),
        VmValue::Nil                   => "nil".to_string(),
    }
}

/// Return the runtime type name of a `VmValue` as a static string.
pub fn value_type_name(v: &VmValue) -> &'static str {
    match v {
        VmValue::Int(_) => "int",
        VmValue::Float(_) => "float",
        VmValue::Bool(_) => "bool",
        VmValue::Str(_) => "str",
        VmValue::Array(_) => "array",
        VmValue::Dict(_) => "dict",
        VmValue::Struct(_) => "struct",
        VmValue::Fn(_) | VmValue::Closure(_, _) => "fn",
        VmValue::BoundMethod(_) | VmValue::NativeBoundMethod(_) => "method",
        VmValue::BuiltinFn(_) => "builtin",
        VmValue::NativeFn(_) => "native fn",
        VmValue::NativeLibFn(_) => "native fn",
        VmValue::Future(_) => "future",
        VmValue::TokenStream(_) => "token stream",
        VmValue::TypeRef(_) => "type",
        VmValue::Prompt(_) => "prompt",
        VmValue::Grammar { .. } => "grammar",
        VmValue::Nil => "nil",
    }
}

// ── VM state ──────────────────────────────────────────────────────────────────

/// The global execution state, including LLM integration.
pub struct VmState {
    /// The value most recently raised by `Instr::Raise` that propagated past its
    /// frame's handler stack. Consumed by the nearest enclosing `SetupHandler`.
    pub raised_exception: Option<VmValue>,
    /// All top-level (global) bindings after execution.
    pub globals: HashMap<String, VmValue>,
    /// Extend-block method tables: `type_name → method_name → fn`.
    pub extend_methods: HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    /// Struct field definitions (needed for struct instantiation validation).
    pub struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Decorator function names registered on each struct type.
    pub struct_decorators: HashMap<String, Vec<(String, Vec<VmValue>)>>,
    /// `@route("field")` configs: type_name → field_name to read for routing.
    pub route_configs: HashMap<String, String>,
    /// Optional LLM inference backend.
    pub inference_backend: Option<std::sync::Arc<dyn llm::InferenceBackend>>,
    pub token_count: i64,
    pub max_retries: usize,
    pub max_tokens: u32,
    pub default_model: String,
    /// Memoisation cache: maps `(prompt_text, output_type)` → the raw response
    /// text that produced a successful result. Mirrors the same cache in `Env`.
    pub prompt_cache: HashMap<(String, Option<String>), String>,
    /// Directory of the currently-executing file — used to resolve relative `use` paths.
    pub source_dir: PathBuf,
    /// Set of canonical paths currently being imported (cycle detection).
    pub import_stack: HashSet<PathBuf>,
    /// Native package map from `[native]` in jade.toml: name → absolute path to .dylib/.so.
    pub native_packages: HashMap<String, PathBuf>,
}

impl VmState {
    fn new() -> Self {
        let mut globals = HashMap::new();
        globals.insert("__tokens__".to_string(), VmValue::Int(0));
        globals.insert("__model__".to_string(), VmValue::Str(String::new()));
        globals.insert("__max_retries__".to_string(), VmValue::Int(15));
        globals.insert("__retry_log__".to_string(), VmValue::Array(Arc::new(Mutex::new(vec![]))));
        stdlib::seed_globals(&mut globals);
        VmState {
            raised_exception: None,
            globals,
            extend_methods: HashMap::new(),
            struct_defs: HashMap::new(),
            struct_decorators: HashMap::new(),
            route_configs: HashMap::new(),
            inference_backend: None,
            token_count: 0,
            max_retries: 15,
            max_tokens: DEFAULT_MAX_TOKENS,
            default_model: String::new(),
            prompt_cache: HashMap::new(),
            source_dir: PathBuf::new(),
            import_stack: HashSet::new(),
            native_packages: HashMap::new(),
        }
    }

    fn set_session(&mut self, name: &str, value: VmValue) {
        self.globals.insert(name.to_string(), value);
    }

    /// Apply `VmOpts` fields onto this state and seed the session globals.
    ///
    /// Extracted to avoid duplicating the same 6-line block in `run` and
    /// `new_for_repl`.
    fn apply_opts(&mut self, opts: VmOpts) {
        self.inference_backend = opts.backend;
        self.max_retries = opts.max_retries;
        self.default_model = opts.default_model.clone();
        self.source_dir = opts.source_dir;
        self.native_packages = opts.native_packages;
        self.set_session("__model__", VmValue::Str(opts.default_model));
        self.set_session("__max_retries__", VmValue::Int(opts.max_retries as i64));
    }

    /// Iterate over all global bindings — used by `-v` verbose output.
    pub fn global_entries(&self) -> impl Iterator<Item = (&String, &VmValue)> {
        self.globals.iter()
    }

    /// Create a live `VmState` seeded from `VmOpts` for the REPL.
    ///
    /// Unlike `run()`, this does **not** execute any program — it returns an
    /// empty state that the REPL can feed snippets into via `run_incremental`.
    pub fn new_for_repl(opts: VmOpts) -> Self {
        let mut state = VmState::new();
        state.apply_opts(opts);
        state
    }

    /// Create a snapshot of this state suitable for passing to a spawned async task.
    ///
    /// Globals, struct_defs, and extend_methods are cloned (value snapshot).
    /// The inference backend is Arc-cloned so both tasks share the same connection pool.
    /// Mutations inside the spawned task do NOT propagate back, except token counts:
    /// the child starts at 0 and returns its delta, which the parent accumulates on Await/Join.
    pub fn new_for_spawn(&self) -> Self {
        VmState {
            raised_exception: None,
            globals: self.globals.clone(),
            extend_methods: self.extend_methods.clone(),
            struct_defs: self.struct_defs.clone(),
            struct_decorators: self.struct_decorators.clone(),
            route_configs: self.route_configs.clone(),
            inference_backend: self.inference_backend.clone(),
            token_count: 0,
            max_retries: self.max_retries,
            max_tokens: self.max_tokens,
            default_model: self.default_model.clone(),
            prompt_cache: self.prompt_cache.clone(),
            source_dir: self.source_dir.clone(),
            import_stack: HashSet::new(),
            native_packages: self.native_packages.clone(),
        }
    }
}

/// Options for an `vm::run` invocation.
pub struct VmOpts {
    pub backend: Option<std::sync::Arc<dyn llm::InferenceBackend>>,
    pub default_model: String,
    pub max_retries: usize,
    /// Directory of the source file being run — used to resolve relative `use` paths.
    /// Defaults to the current working directory when running in-memory (tests, REPL).
    pub source_dir: PathBuf,
    /// Native package map from `[native]` in jade.toml: name → absolute path to .dylib/.so.
    pub native_packages: HashMap<String, PathBuf>,
}

impl Default for VmOpts {
    fn default() -> Self {
        VmOpts {
            backend: None,
            default_model: String::new(),
            max_retries: 15,
            source_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            native_packages: HashMap::new(),
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Execute a compiled program and return the populated global state.
pub async fn run(program: CompiledProgram, opts: VmOpts) -> Result<VmState> {
    let mut state = VmState::new();
    state.apply_opts(opts);
    run_with_state(program, &mut state).await?;
    Ok(state)
}

/// Execute a compiled program against an existing `VmState`.
///
/// This is the public entry point for the REPL — it lets each snippet share
/// globals, struct definitions, and extend-block methods with prior snippets.
pub async fn run_incremental(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    run_with_state(program, state).await
}


/// Execute a compiled program against an existing `VmState`.
/// Used internally for imports so they share globals/struct_defs/extend_methods.
async fn run_with_state(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    // Merge compile-time metadata into the shared state.
    for (k, v) in program.struct_defs {
        state.globals.entry(k.clone()).or_insert_with(|| VmValue::TypeRef(k.clone()));
        state.struct_defs.insert(k, v);
    }
    for (k, v) in program.struct_decorators {
        state.struct_decorators.insert(k, v);
    }
    for (type_name, methods) in program.extend_methods {
        state.extend_methods.entry(type_name).or_default().extend(methods);
    }
    for (k, v) in program.route_configs {
        state.route_configs.insert(k, v);
    }

    let mut slots: Vec<VmValue> = vec![VmValue::Nil; program.top_n_slots as usize];
    execute_chunk(&program.top, &mut slots, state).await?;
    Ok(())
}

// ── Execution engine ──────────────────────────────────────────────────────────

/// Build a `RuntimeError { message }` struct value for wrapping built-in errors
/// when they are caught by a `try/catch` block.
fn make_vm_runtime_error(message: String) -> VmValue {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), VmValue::Str(message));
    VmValue::Struct(Arc::new(Mutex::new(VmStruct {
        type_name: "RuntimeError".to_string(),
        fields,
    })))
}

/// Execute `chunk` with the provided register frame.  Returns `Some(value)` if
/// a `Return` instruction was executed, `None` if execution ended normally.
async fn execute_chunk(
    chunk: &Chunk,
    slots: &mut Vec<VmValue>,
    state: &mut VmState,
) -> Result<Option<VmValue>> {
    // Ensure the slots vector is large enough for this chunk's registers.
    // (Top-level slots are pre-allocated by `run`; function frames are sized
    // by `call_fn`; this is a safety net for edge cases.)
    let needed = chunk.code.iter().fold(0u32, |acc, instr| acc.max(instr_max_reg(instr)));
    if slots.len() <= needed as usize {
        slots.resize(needed as usize + 1, VmValue::Nil);
    }

    // Instruction pointer — must be declared before the macros that assign to it.
    let mut ip: usize = 0;

    // Active exception handler frames: (caught_reg, handler_ip).
    // SetupHandler pushes; PopHandler pops; Raise/errors dispatch to the top frame.
    let mut handlers: Vec<(Reg, usize)> = Vec::new();

    // Dispatch `err` to the top handler frame, or propagate it up the call stack.
    // Used inline — written as a named closure so every error site stays readable.
    // Returns the error to propagate (None means handler was invoked; continue the loop).
    macro_rules! vm_err {
        ($err:expr) => {{
            let __err: JadeError = $err;
            if let Some((__caught, __handler_ip)) = handlers.pop() {
                let __raised = match __err {
                    JadeError::Exception { .. } => state.raised_exception.take()
                        .unwrap_or_else(|| VmValue::Str("unknown exception".to_string())),
                    ref __e => make_vm_runtime_error(__e.to_string()),
                };
                set(slots, __caught, __raised);
                ip = __handler_ip;
                continue;
            } else {
                return Err(__err);
            }
        }};
    }

    // Like `expr?` but dispatches to an exception handler when one is active.
    macro_rules! vm_try {
        ($expr:expr) => {
            match $expr {
                Ok(__v) => __v,
                Err(__e) => { vm_err!(__e); }
            }
        };
    }

    loop {
        if ip >= chunk.code.len() {
            break;
        }
        let instr = &chunk.code[ip];
        let span  = chunk.spans[ip];
        ip += 1;

        match instr {
            Instr::Halt => break,

            // ── Imports ───────────────────────────────────────────────────────
            Instr::ImportFile(path, namespace) => {
                // ── Native packages ─────────────────────────────────────────
                // `use "native/<name>"` loads a .dylib/.so registered in jade.toml [native].
                if let Some(pkg_name) = path.strip_prefix("native/") {
                    let lib_path = state.native_packages.get(pkg_name)
                        .cloned()
                        .ok_or_else(|| JadeError::IoError {
                            message: format!(
                                "native package '{}' not declared in jade.toml [native]",
                                pkg_name
                            ),
                            span,
                        })?;
                    let fns = crate::native::load_native_package(&lib_path, span)?;
                    state.globals.insert(pkg_name.to_string(), VmValue::Dict(fns));
                    continue;
                }

                // ── Built-in packages ───────────────────────────────────────
                // stdlib packages always bind under their own global_name; namespace param ignored.
                if let Some(pkg) = stdlib::find_package(path) {
                    let val = if pkg.import_name == "llm" {
                        stdlib::llm_pkg::llm_vm_dict_value()
                    } else {
                        pkg.vm_dict_value()
                    };
                    state.globals.insert(pkg.global_name.to_string(), val);
                    continue;
                }

                // ── User .jde files — namespaced ────────────────────────────
                let abs_path = state.source_dir.join(path);
                let canon = abs_path.canonicalize().map_err(|_| JadeError::ImportNotFound {
                    path: path.clone(),
                    span,
                })?;

                if state.import_stack.contains(&canon) {
                    return Err(JadeError::CircularImport {
                        path: path.clone(),
                        span,
                    });
                }

                state.import_stack.insert(canon.clone());

                let sub_source_dir = canon.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf();

                let compile_result: Result<crate::compiler::emit::CompiledProgram> = (|| {
                    let source = std::fs::read_to_string(&canon).map_err(|_| {
                        JadeError::ImportNotFound { path: path.clone(), span }
                    })?;

                    let canon_str = canon.to_string_lossy().into_owned();
                    let hash = crate::cache::file_hash(&canon);

                    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
                    let program = match cached_ast {
                        Some(p) => p,
                        None => {
                            let tokens = crate::frontend::lexer::tokenize(&source)?;
                            let p = crate::frontend::parser::parse(tokens)?;
                            if let Some(ref h) = hash {
                                crate::cache::write_ast_cache(h, &canon_str, &p);
                            }
                            p
                        }
                    };

                    let tprogram = if let Some(ref h) = hash {
                        match crate::cache::read_tir_cache(h) {
                            Some(tp) => tp,
                            None => {
                                let tp = crate::compiler::type_infer::infer(program)?;
                                crate::cache::write_tir_cache(h, &canon_str, &tp);
                                tp
                            }
                        }
                    } else {
                        crate::compiler::type_infer::infer(program)?
                    };

                    crate::compiler::emit::emit(tprogram)
                })();

                let result: Result<()> = match compile_result {
                    Ok(compiled) => {
                        // Run the imported file in an isolated sub-state so its
                        // top-level bindings don't bleed into the parent namespace.
                        let mut sub_state = VmState::new();
                        // Capture keys already present so we can filter them out later.
                        let initial_keys: std::collections::HashSet<String> =
                            sub_state.globals.keys().cloned().collect();
                        // Propagate runtime config from parent.
                        sub_state.source_dir = sub_source_dir;
                        sub_state.import_stack = state.import_stack.clone();
                        sub_state.native_packages = state.native_packages.clone();
                        sub_state.inference_backend = state.inference_backend.clone();
                        sub_state.max_retries = state.max_retries;
                        sub_state.max_tokens = state.max_tokens;
                        sub_state.default_model = state.default_model.clone();

                        let r = Box::pin(run_with_state(compiled, &mut sub_state)).await;
                        if r.is_ok() {
                            // Collect user-defined globals (exclude stdlib and internal keys).
                            let mut module_globals: HashMap<String, VmValue> = sub_state
                                .globals
                                .drain()
                                .filter(|(k, _)| !initial_keys.contains(k))
                                .collect();
                            // Qualify any TypeRef values so coercion calls resolve correctly.
                            for v in module_globals.values_mut() {
                                if let VmValue::TypeRef(t) = v {
                                    *t = format!("{}.{}", namespace, t);
                                }
                            }
                            state.globals.insert(namespace.clone(), VmValue::Dict(module_globals));

                            // Merge struct_defs prefixed with the namespace.
                            for (k, v) in sub_state.struct_defs.drain() {
                                state.struct_defs.insert(format!("{}.{}", namespace, k), v);
                            }
                            // Merge extend_methods prefixed with the namespace.
                            for (type_name, methods) in sub_state.extend_methods.drain() {
                                state.extend_methods
                                    .entry(format!("{}.{}", namespace, type_name))
                                    .or_default()
                                    .extend(methods);
                            }
                            // Merge struct_decorators prefixed with the namespace.
                            for (type_name, decs) in sub_state.struct_decorators.drain() {
                                state.struct_decorators
                                    .entry(format!("{}.{}", namespace, type_name))
                                    .or_default()
                                    .extend(decs);
                            }
                            // Propagate LLM token usage back to parent.
                            state.token_count += sub_state.token_count;
                        }
                        r.map_err(|e| JadeError::InFile {
                            file: path.clone(),
                            cause: Box::new(e),
                        })
                    }
                    Err(e) => Err(JadeError::InFile {
                        file: path.clone(),
                        cause: Box::new(e),
                    }),
                };

                state.import_stack.remove(&canon);
                result?;
            }

            Instr::ImportFrom(path, names) => {
                // ── Native packages ─────────────────────────────────────────
                if let Some(pkg_name) = path.strip_prefix("native/") {
                    let lib_path = state.native_packages.get(pkg_name)
                        .cloned()
                        .ok_or_else(|| JadeError::IoError {
                            message: format!(
                                "native package '{}' not declared in jade.toml [native]",
                                pkg_name
                            ),
                            span,
                        })?;
                    let fns = crate::native::load_native_package(&lib_path, span)?;
                    for name in names {
                        if let Some(val) = fns.get(name) {
                            state.globals.insert(name.clone(), val.clone());
                        }
                    }
                    continue;
                }

                if let Some(pkg) = stdlib::find_package(path) {
                    // Build the package dict, then extract only the requested names.
                    let dict = if pkg.import_name == "llm" {
                        stdlib::llm_pkg::llm_vm_dict_value()
                    } else {
                        pkg.vm_dict_value()
                    };
                    if let VmValue::Dict(map) = dict {
                        for name in names {
                            if let Some(val) = map.get(name) {
                                state.globals.insert(name.clone(), val.clone());
                            }
                        }
                    }
                    continue;
                }
                // File import: run in an isolated sub-state, then bind only the
                // requested names directly into the parent namespace.
                let abs_path = state.source_dir.join(path);
                let canon = abs_path.canonicalize().map_err(|_| JadeError::ImportNotFound {
                    path: path.clone(),
                    span,
                })?;
                if state.import_stack.contains(&canon) {
                    return Err(JadeError::CircularImport { path: path.clone(), span });
                }
                state.import_stack.insert(canon.clone());
                let sub_source_dir = canon.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf();
                let compile_result: Result<crate::compiler::emit::CompiledProgram> = (|| {
                    let source = std::fs::read_to_string(&canon).map_err(|_| {
                        JadeError::ImportNotFound { path: path.clone(), span }
                    })?;
                    let tokens = crate::frontend::lexer::tokenize(&source)?;
                    let p = crate::frontend::parser::parse(tokens)?;
                    let tp = crate::compiler::type_infer::infer(p)?;
                    crate::compiler::emit::emit(tp)
                })();
                let result: Result<()> = match compile_result {
                    Ok(compiled) => {
                        let mut sub_state = VmState::new();
                        sub_state.source_dir = sub_source_dir;
                        sub_state.import_stack = state.import_stack.clone();
                        sub_state.native_packages = state.native_packages.clone();
                        sub_state.inference_backend = state.inference_backend.clone();
                        sub_state.max_retries = state.max_retries;
                        sub_state.max_tokens = state.max_tokens;
                        sub_state.default_model = state.default_model.clone();
                        let r = Box::pin(run_with_state(compiled, &mut sub_state)).await;
                        if r.is_ok() {
                            for name in names {
                                if let Some(val) = sub_state.globals.remove(name) {
                                    state.globals.insert(name.clone(), val);
                                }
                                // If the requested name is a struct type, also import its def.
                                if let Some(def) = sub_state.struct_defs.remove(name) {
                                    state.struct_defs.insert(name.clone(), def);
                                }
                                if let Some(methods) = sub_state.extend_methods.remove(name) {
                                    state.extend_methods
                                        .entry(name.clone())
                                        .or_default()
                                        .extend(methods);
                                }
                            }
                            state.token_count += sub_state.token_count;
                        }
                        r.map_err(|e| JadeError::InFile {
                            file: path.clone(),
                            cause: Box::new(e),
                        })
                    }
                    Err(e) => Err(JadeError::InFile {
                        file: path.clone(),
                        cause: Box::new(e),
                    }),
                };
                state.import_stack.remove(&canon);
                result?;
            }

            // ── Loads ─────────────────────────────────────────────────────────
            Instr::LoadInt(d, v)   => set(slots, *d, VmValue::Int(*v)),
            Instr::LoadFloat(d, v) => set(slots, *d, VmValue::Float(*v)),
            Instr::LoadBool(d, v)  => set(slots, *d, VmValue::Bool(*v)),
            Instr::LoadStr(d, s)   => set(slots, *d, VmValue::Str(s.clone())),
            Instr::LoadNil(d)      => set(slots, *d, VmValue::Nil),
            Instr::LoadFn(d, idx)  => {
                let cf = Arc::clone(&chunk.fn_defs[*idx]);
                set(slots, *d, VmValue::Fn(cf));
            }
            Instr::MakeClosure(d, idx) => {
                let cf = Arc::clone(&chunk.fn_defs[*idx]);
                let captured: HashMap<String, VmValue> = state.globals.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                set(slots, *d, VmValue::Closure(cf, Arc::new(captured)));
            }
            Instr::Move(d, s) => {
                let v = get(slots, *s).clone();
                set(slots, *d, v);
            }

            // ── Variables ─────────────────────────────────────────────────────
            Instr::GetGlobal(d, name) => {
                let v = state.globals.get(name)
                    .ok_or_else(|| JadeError::UndefinedVariable { name: name.clone(), span })?
                    .clone();
                set(slots, *d, v);
            }
            Instr::SetGlobal(name, s) => {
                let v = vm_try!(vm_maybe_drain(get(slots, *s).clone(), state, span).await);
                state.globals.insert(name.clone(), v);
            }
            Instr::GetLocal(d, slot) => {
                let v = slots.get(*slot as usize)
                    .cloned()
                    .unwrap_or(VmValue::Nil);
                set(slots, *d, v);
            }
            Instr::SetLocal(slot, s) => {
                let v = vm_try!(vm_maybe_drain(get(slots, *s).clone(), state, span).await);
                ensure_slot(slots, *slot);
                slots[*slot as usize] = v;
            }

            // ── Integer arithmetic ────────────────────────────────────────────
            Instr::AddInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(
                    vm_try!(a.checked_add(b).ok_or(JadeError::IntegerOverflow { span }))
                ));
            }
            Instr::SubInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(
                    vm_try!(a.checked_sub(b).ok_or(JadeError::IntegerOverflow { span }))
                ));
            }
            Instr::MulInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(
                    vm_try!(a.checked_mul(b).ok_or(JadeError::IntegerOverflow { span }))
                ));
            }
            Instr::DivInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b == 0 { vm_err!(JadeError::DivisionByZero { span }); }
                set(slots, *d, VmValue::Int(a / b));
            }
            Instr::ModInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b == 0 { vm_err!(JadeError::RemainderByZero { span }); }
                set(slots, *d, VmValue::Int(a % b));
            }
            Instr::NegInt(d, s) => {
                let a = vm_try!(get_int(slots, *s, span));
                set(slots, *d, VmValue::Int(-a));
            }

            // ── Float arithmetic ──────────────────────────────────────────────
            Instr::AddFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Float(a + b));
            }
            Instr::SubFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Float(a - b));
            }
            Instr::MulFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Float(a * b));
            }
            Instr::DivFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                if b == 0.0 { vm_err!(JadeError::DivisionByZero { span }); }
                set(slots, *d, VmValue::Float(a / b));
            }
            Instr::NegFloat(d, s) => {
                let a = vm_try!(get_flt(slots, *s, span));
                set(slots, *d, VmValue::Float(-a));
            }
            Instr::IntToFloat(d, s) => {
                let a = vm_try!(get_int(slots, *s, span));
                set(slots, *d, VmValue::Float(a as f64));
            }
            Instr::ConcatStr(d, l, r) => {
                let a = vm_try!(get_str(slots, *l, span));
                let b = vm_try!(get_str(slots, *r, span));
                set(slots, *d, VmValue::Str(a + &b));
            }

            // ── Bitwise ───────────────────────────────────────────────────────
            Instr::BitAnd(d, l, r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Int(a&b)); }
            Instr::BitOr(d, l, r)  => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Int(a|b)); }
            Instr::BitXor(d, l, r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Int(a^b)); }
            Instr::BitNot(d, s)    => { let a=vm_try!(get_int(slots,*s,span)); set(slots,*d,VmValue::Int(!a)); }
            Instr::Shl(d, l, r)    => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b < 0 || b >= 64 { vm_err!(JadeError::InvalidShift { amount: b, span }); }
                set(slots, *d, VmValue::Int(a << b as u32));
            }
            Instr::Shr(d, l, r)    => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b < 0 || b >= 64 { vm_err!(JadeError::InvalidShift { amount: b, span }); }
                set(slots, *d, VmValue::Int(a >> b as u32));
            }

            // ── Logical ───────────────────────────────────────────────────────
            Instr::Not(d, s) => {
                let b = vm_try!(get_bool(slots, *s, span));
                set(slots, *d, VmValue::Bool(!b));
            }

            // ── Dynamic fallbacks ─────────────────────────────────────────────
            Instr::BinOp(d, op, l, r) => {
                let lv = get(slots, *l).clone();
                let rv = get(slots, *r).clone();
                let result = vm_try!(eval_binop_dynamic(op, lv, rv, span));
                set(slots, *d, result);
            }
            Instr::UnaryOp(d, op, s) => {
                let v = get(slots, *s).clone();
                let result = vm_try!(eval_unaryop_dynamic(op, v, span));
                set(slots, *d, result);
            }

            // ── Typed comparisons — int ───────────────────────────────────────
            Instr::CmpEqInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b)); }
            Instr::CmpNeInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a!=b)); }
            Instr::CmpLtInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<b));  }
            Instr::CmpGtInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>b));  }
            Instr::CmpLeInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<=b)); }
            Instr::CmpGeInt(d,l,r) => { let (a,b)=vm_try!(int2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>=b)); }

            // ── Typed comparisons — float ─────────────────────────────────────
            Instr::CmpEqFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b)); }
            Instr::CmpNeFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a!=b)); }
            Instr::CmpLtFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<b));  }
            Instr::CmpGtFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>b));  }
            Instr::CmpLeFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<=b)); }
            Instr::CmpGeFloat(d,l,r) => { let (a,b)=vm_try!(flt2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>=b)); }

            // ── Typed comparisons — mixed ─────────────────────────────────────
            Instr::CmpLtIntFloat(d,l,r) => { let a=vm_try!(get_int(slots,*l,span)) as f64; let b=vm_try!(get_flt(slots,*r,span)); set(slots,*d,VmValue::Bool(a<b));  }
            Instr::CmpGtIntFloat(d,l,r) => { let a=vm_try!(get_int(slots,*l,span)) as f64; let b=vm_try!(get_flt(slots,*r,span)); set(slots,*d,VmValue::Bool(a>b));  }
            Instr::CmpLeIntFloat(d,l,r) => { let a=vm_try!(get_int(slots,*l,span)) as f64; let b=vm_try!(get_flt(slots,*r,span)); set(slots,*d,VmValue::Bool(a<=b)); }
            Instr::CmpGeIntFloat(d,l,r) => { let a=vm_try!(get_int(slots,*l,span)) as f64; let b=vm_try!(get_flt(slots,*r,span)); set(slots,*d,VmValue::Bool(a>=b)); }
            Instr::CmpLtFloatInt(d,l,r) => { let a=vm_try!(get_flt(slots,*l,span)); let b=vm_try!(get_int(slots,*r,span)) as f64; set(slots,*d,VmValue::Bool(a<b));  }
            Instr::CmpGtFloatInt(d,l,r) => { let a=vm_try!(get_flt(slots,*l,span)); let b=vm_try!(get_int(slots,*r,span)) as f64; set(slots,*d,VmValue::Bool(a>b));  }
            Instr::CmpLeFloatInt(d,l,r) => { let a=vm_try!(get_flt(slots,*l,span)); let b=vm_try!(get_int(slots,*r,span)) as f64; set(slots,*d,VmValue::Bool(a<=b)); }
            Instr::CmpGeFloatInt(d,l,r) => { let a=vm_try!(get_flt(slots,*l,span)); let b=vm_try!(get_int(slots,*r,span)) as f64; set(slots,*d,VmValue::Bool(a>=b)); }

            // ── Typed comparisons — bool ──────────────────────────────────────
            Instr::CmpEqBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b)); }
            Instr::CmpNeBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a!=b)); }
            Instr::CmpLtBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(!a&&b)); }
            Instr::CmpGtBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a&&!b)); }
            Instr::CmpLeBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b||(!a&&b))); }
            Instr::CmpGeBool(d,l,r) => { let (a,b)=vm_try!(bool2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b||(a&&!b))); }

            // ── Typed comparisons — str ───────────────────────────────────────
            Instr::CmpEqStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a==b)); }
            Instr::CmpNeStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a!=b)); }
            Instr::CmpLtStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<b));  }
            Instr::CmpGtStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>b));  }
            Instr::CmpLeStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a<=b)); }
            Instr::CmpGeStr(d,l,r) => { let (a,b)=vm_try!(str2(slots,*l,*r,span)); set(slots,*d,VmValue::Bool(a>=b)); }

            // ── Dynamic comparisons ───────────────────────────────────────────
            Instr::CmpEq(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,"==",span)); set(slots,*d,v); }
            Instr::CmpNe(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,"!=",span)); set(slots,*d,v); }
            Instr::CmpLt(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,"<", span)); set(slots,*d,v); }
            Instr::CmpGt(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,">", span)); set(slots,*d,v); }
            Instr::CmpLe(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,"<=",span)); set(slots,*d,v); }
            Instr::CmpGe(d,l,r) => { let v=vm_try!(cmp_dynamic(slots,*l,*r,">=",span)); set(slots,*d,v); }

            // ── Control flow ──────────────────────────────────────────────────
            Instr::Jump(offset) => {
                ip = (ip as i64 + *offset as i64) as usize;
            }
            Instr::JumpIfFalse(cond, offset) => {
                if let VmValue::Bool(false) = get(slots, *cond) {
                    ip = (ip as i64 + *offset as i64) as usize;
                }
            }
            Instr::JumpIfTrue(cond, offset) => {
                if let VmValue::Bool(true) = get(slots, *cond) {
                    ip = (ip as i64 + *offset as i64) as usize;
                }
            }

            // ── Calls ─────────────────────────────────────────────────────────
            Instr::Call(dest, callee_reg, arg_regs) => {
                let callee = get(slots, *callee_reg).clone();
                let args: Vec<VmValue> = arg_regs.iter().map(|&r| get(slots, r).clone()).collect();
                let result = vm_try!(call_value(callee, args, state, span).await);
                set(slots, *dest, result);
            }
            Instr::CallNamed(dest, callee_reg, arg_pairs) => {
                let callee = get(slots, *callee_reg).clone();
                let mut positional: Vec<VmValue> = Vec::new();
                let mut named: Vec<(String, VmValue)> = Vec::new();
                for (name_opt, reg) in arg_pairs {
                    let val = get(slots, *reg).clone();
                    match name_opt {
                        None    => positional.push(val),
                        Some(n) => named.push((n.clone(), val)),
                    }
                }
                let args = vm_try!(resolve_named_args(&callee, positional, named, span));
                let result = vm_try!(call_value(callee, args, state, span).await);
                set(slots, *dest, result);
            }
            Instr::Return(opt_reg) => {
                let v = match opt_reg {
                    Some(r) => get(slots, *r).clone(),
                    None    => VmValue::Nil,
                };
                return Ok(Some(v));
            }

            // ── Collections ───────────────────────────────────────────────────
            Instr::MakeArray(dest, elem_regs) => {
                let elems: Vec<VmValue> = elem_regs.iter().map(|&r| get(slots, r).clone()).collect();
                set(slots, *dest, VmValue::Array(Arc::new(Mutex::new(elems))));
            }
            Instr::MakeDict(dest, pairs) => {
                let mut map = HashMap::with_capacity(pairs.len());
                for &(kr, vr) in pairs {
                    let key_val = get(slots, kr).clone();
                    let key = match key_val {
                        VmValue::Str(s) => s,
                        ref other => { vm_err!(JadeError::TypeError { message: format!("dict key must be str, got {}", value_type_name(other)), span }); }
                    };
                    let val = get(slots, vr).clone();
                    map.insert(key, val);
                }
                set(slots, *dest, VmValue::Dict(map));
            }
            Instr::GetIndex(dest, obj_reg, idx_reg) => {
                let obj = get(slots, *obj_reg).clone();
                let idx = get(slots, *idx_reg).clone();
                let result = vm_try!(vm_index(obj, idx, span));
                set(slots, *dest, result);
            }
            Instr::SetIndex(obj_reg, idx_reg, val_reg) => {
                let idx = get(slots, *idx_reg).clone();
                let val = get(slots, *val_reg).clone();
                // Clone the object first to avoid holding a mutable borrow on slots
                // (needed because vm_err! may re-borrow slots via `set`).
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Array(arc) => {
                        let i = match idx { VmValue::Int(n) => n, ref other => { vm_err!(JadeError::TypeError { message: format!("array index must be int, got {}", value_type_name(other)), span }); } };
                        let len = arc.lock().len();
                        if i < 0 || i as usize >= len { vm_err!(JadeError::IndexOutOfBounds { index: i, len, span }); }
                        arc.lock()[i as usize] = val;
                    }
                    VmValue::Dict(mut m) => {
                        let k = match idx { VmValue::Str(s) => s, ref other => { vm_err!(JadeError::TypeError { message: format!("dict index must be str, got {}", value_type_name(other)), span }); } };
                        m.insert(k, val);
                        slots[*obj_reg as usize] = VmValue::Dict(m);
                    }
                    ref other => { vm_err!(JadeError::TypeError { message: format!("value of type {} is not indexable", value_type_name(other)), span }); }
                }
            }

            // ── Struct ────────────────────────────────────────────────────────
            Instr::MakeStruct(dest, type_name, field_specs) => {
                let mut fields = HashMap::with_capacity(field_specs.len());
                for (fname, freg, is_prompt) in field_specs {
                    let mut val = get(slots, *freg).clone();
                    if *is_prompt {
                        val = match val {
                            VmValue::Str(text) => VmValue::Prompt(text),
                            other => other, // already Prompt, or wrong type caught at type-check
                        };
                    }
                    fields.insert(fname.clone(), val);
                }
                // Fill in defaults for any fields omitted from the literal.
                // Needed when the struct type was unknown at compile time (imported type).
                if let Some(def_fields) = state.struct_defs.get(type_name.as_str()).cloned() {
                    for def_field in &def_fields {
                        match def_field {
                            StructFieldDef::Let { name, default } => {
                                if !fields.contains_key(name.as_str()) {
                                    if let Some(v) = eval_literal_default(default) {
                                        fields.insert(name.clone(), v);
                                    }
                                }
                            }
                            StructFieldDef::Prompt { name, default } => {
                                if !fields.contains_key(name.as_str()) {
                                    if let Some(v) = eval_literal_default(default) {
                                        let v = match v {
                                            VmValue::Str(s) => VmValue::Prompt(s),
                                            other => other,
                                        };
                                        fields.insert(name.clone(), v);
                                    }
                                }
                            }
                            StructFieldDef::Required(_) => {}
                        }
                    }
                }
                let mut result = VmValue::Struct(Arc::new(Mutex::new(VmStruct {
                    type_name: type_name.clone(),
                    fields,
                })));
                // Call struct decorators: dec(instance, arg1, ...) for each @dec.
                let decs = state.struct_decorators.get(type_name).cloned().unwrap_or_default();
                for (dec_name, dec_args) in decs {
                    if let Some(dec_fn) = resolve_decorator_fn(&dec_name, state) {
                        let mut call_args = vec![result];
                        call_args.extend(dec_args);
                        result = call_value(dec_fn, call_args, state, span).await?;
                    }
                }
                set(slots, *dest, result);
            }
            Instr::GetField(dest, obj_reg, field) => {
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Struct(rc) => {
                        let (type_name, field_val) = {
                            let guard = rc.lock();
                            (guard.type_name.clone(), guard.fields.get(field.as_str()).cloned())
                        };
                        if let Some(v) = field_val {
                            set(slots, *dest, v);
                        } else if let Some(methods) = state.extend_methods.get(&type_name) {
                            if let Some(mfn) = methods.get(field.as_str()) {
                                set(slots, *dest, VmValue::BoundMethod(Arc::new(VmBoundMethod {
                                    receiver: rc,
                                    method: Arc::clone(mfn),
                                })));
                            } else {
                                vm_err!(JadeError::UndefinedField { type_name, field: field.clone(), span });
                            }
                        } else {
                            vm_err!(JadeError::UndefinedField { type_name, field: field.clone(), span });
                        }
                    }
                    // Dict: check HashMap entries first (package namespaces), then primitive methods.
                    VmValue::Dict(ref map) => {
                        if let Some(v) = map.get(field.as_str()) {
                            set(slots, *dest, v.clone());
                        } else if let Some(method) = stdlib::find_primitive_method(PrimType::Dict, field) {
                            set(slots, *dest, VmValue::NativeBoundMethod(Arc::new(NativeBoundMethod {
                                receiver: obj.clone(),
                                method,
                            })));
                        } else {
                            vm_err!(JadeError::UndefinedField {
                                type_name: "dict".to_string(),
                                field: field.clone(),
                                span,
                            });
                        }
                    }
                    // Primitive method dispatch for str/array/int/float.
                    ref prim @ (VmValue::Str(_) | VmValue::Array(_)
                               | VmValue::Int(_) | VmValue::Float(_)) => {
                        if let Some(ty) = PrimType::from_value(prim) {
                            if let Some(method) = stdlib::find_primitive_method(ty, field) {
                                set(slots, *dest, VmValue::NativeBoundMethod(Arc::new(NativeBoundMethod {
                                    receiver: prim.clone(),
                                    method,
                                })));
                            } else {
                                vm_err!(JadeError::UndefinedField {
                                    type_name: ty.type_name().to_string(),
                                    field: field.clone(),
                                    span,
                                });
                            }
                        } else {
                            vm_err!(JadeError::NotAStruct { span });
                        }
                    }
                    // Dunder attributes on function values: fn.__name__, fn.__params__
                    VmValue::Fn(ref cf) => {
                        let v = match field.as_str() {
                            "__name__" => VmValue::Str(cf.chunk.name.clone()),
                            "__params__" => {
                                let arr: Vec<VmValue> = cf.params.iter()
                                    .map(|p| VmValue::Str(p.clone()))
                                    .collect();
                                VmValue::Array(Arc::new(Mutex::new(arr)))
                            }
                            _ => vm_err!(JadeError::UndefinedField {
                                type_name: "fn".to_string(),
                                field: field.clone(),
                                span,
                            }),
                        };
                        set(slots, *dest, v);
                    }
                    _ => { vm_err!(JadeError::NotAStruct { span }); }
                }
            }
            Instr::SetField(obj_reg, field, val_reg) => {
                let val = get(slots, *val_reg).clone();
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Struct(rc) => {
                        let error_type_name = {
                            let guard = rc.lock();
                            if guard.fields.contains_key(field.as_str()) {
                                None
                            } else {
                                Some(guard.type_name.clone())
                            }
                        };
                        if let Some(type_name) = error_type_name {
                            vm_err!(JadeError::UndefinedField {
                                type_name,
                                field: field.clone(),
                                span,
                            });
                        }
                        rc.lock().fields.insert(field.clone(), val);
                    }
                    _ => { vm_err!(JadeError::NotAStruct { span }); }
                }
            }

            // ── FStr ──────────────────────────────────────────────────────────
            Instr::BuildFStr(dest, parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        FStrPart::Literal(s) => result.push_str(s),
                        FStrPart::Reg(r) => result.push_str(&value_to_display(get(slots, *r))),
                    }
                }
                set(slots, *dest, VmValue::Str(result));
            }

            // ── Prompt ────────────────────────────────────────────────────────
            Instr::MakePrompt(dest, text_reg) => {
                let text = match get(slots, *text_reg).clone() {
                    VmValue::Str(s) => s,
                    _ => { vm_err!(JadeError::TypeError {
                        message: "prompt declaration requires a string body".to_string(),
                        span,
                    }); }
                };
                set(slots, *dest, VmValue::Prompt(text));
            }
            Instr::PromptDeref(dest, prompt_reg, output_type, grammar_reg) => {
                let text = match get(slots, *prompt_reg).clone() {
                    VmValue::Prompt(t) => t,
                    _ => { vm_err!(JadeError::NotAPrompt { name: "<expr>".to_string(), span }); }
                };
                let (grammar_override, grammar_anchor) = match grammar_reg {
                    None => (None, None),
                    Some(r) => match get(slots, *r).clone() {
                        VmValue::Grammar { pattern, anchor } => {
                            (Some(crate::compiler::gbnf::grammar_from_pattern(&pattern)), anchor)
                        }
                        _ => panic!("grammar register did not hold a Grammar value"),
                    },
                };
                let result = if output_type.is_none() && grammar_override.is_none() {
                    vm_try!(vm_prompt_deref_stream(text, state, span).await)
                } else {
                    vm_try!(vm_prompt_deref(text, output_type.as_deref(), grammar_override, grammar_anchor, state, span).await)
                };
                set(slots, *dest, result);
            }

            // ── Exception handling ────────────────────────────────────────────
            Instr::Raise(val_reg) => {
                let raised = get(slots, *val_reg).clone();
                if let Some((caught_reg, handler_ip)) = handlers.pop() {
                    set(slots, caught_reg, raised);
                    ip = handler_ip;
                } else {
                    let message = value_to_display(&raised);
                    state.raised_exception = Some(raised);
                    return Err(JadeError::Exception { message, span });
                }
            }

            Instr::SetupHandler(caught_reg, offset) => {
                // ip has already been incremented past this instruction.
                let handler_ip = (ip as i64 + *offset as i64) as usize;
                handlers.push((*caught_reg, handler_ip));
            }

            Instr::PopHandler => {
                handlers.pop();
            }

            Instr::GetTypeName(dest, src) => {
                let name = match get(slots, *src) {
                    VmValue::Struct(rc) => rc.lock().type_name.clone(),
                    _ => String::new(),
                };
                set(slots, *dest, VmValue::Str(name));
            }

            // ── Async ─────────────────────────────────────────────────────────
            Instr::Spawn(dest, callee_reg, arg_regs) => {
                let callee = get(slots, *callee_reg).clone();
                let args: Vec<VmValue> = arg_regs.iter().map(|&r| get(slots, r).clone()).collect();
                let child_state = state.new_for_spawn();
                let handle = tokio::spawn(call_value_standalone(callee, args, child_state, span));
                set(slots, *dest, VmValue::Future(Arc::new(JadeFuture {
                    handle: Mutex::new(Some(handle)),
                })));
            }
            Instr::Await(dest, future_reg) => {
                let fut_val = get(slots, *future_reg).clone();
                match fut_val {
                    VmValue::Future(jade_fut) => {
                        // SAFETY: .take() consumes the JoinHandle as an owned value before
                        // reaching .await, so the MutexGuard is dropped synchronously here —
                        // std::sync::MutexGuard is never held across an await point.
                        let handle = vm_try!(jade_fut.handle.lock().take()
                            .ok_or(JadeError::DoubleAwait { span }));
                        let join_result = handle.await;
                        let (task_result, child_raised) = vm_try!(join_result.map_err(|e| JadeError::AsyncPanic {
                            message: e.to_string(),
                            span,
                        }));
                        if let Some(v) = child_raised {
                            state.raised_exception = Some(v);
                        }
                        let (value, child_tokens) = vm_try!(task_result);
                        state.token_count += child_tokens;
                        state.globals.insert("__tokens__".to_string(), VmValue::Int(state.token_count));
                        set(slots, *dest, value);
                    }
                    _ => { vm_err!(JadeError::NotAFuture { span }); }
                }
            }
            Instr::Join(dest, future_regs) => {
                let mut handles = Vec::with_capacity(future_regs.len());
                for &r in future_regs {
                    match get(slots, r).clone() {
                        VmValue::Future(jade_fut) => {
                            // SAFETY: same as Instr::Await — .take() is synchronous.
                            let handle = vm_try!(jade_fut.handle.lock().take()
                                .ok_or(JadeError::DoubleAwait { span }));
                            handles.push(handle);
                        }
                        _ => { vm_err!(JadeError::NotAFuture { span }); }
                    }
                }
                let mut results = Vec::with_capacity(handles.len());
                for handle in handles {
                    let join_result = handle.await;
                    let (task_result, child_raised) = vm_try!(join_result.map_err(|e| JadeError::AsyncPanic {
                        message: e.to_string(),
                        span,
                    }));
                    if let Some(v) = child_raised {
                        state.raised_exception = Some(v);
                    }
                    let (value, child_tokens) = vm_try!(task_result);
                    state.token_count += child_tokens;
                    results.push(value);
                }
                state.globals.insert("__tokens__".to_string(), VmValue::Int(state.token_count));
                set(slots, *dest, VmValue::Array(Arc::new(Mutex::new(results))));
            }
        }
    }
    Ok(None)
}

// ── Call dispatch ─────────────────────────────────────────────────────────────

/// Replace zero-span placeholders from built-in error paths with the actual call-site span.
fn patch_builtin_span(mut e: JadeError, call_span: Span) -> JadeError {
    match &mut e {
        JadeError::ArityMismatch { span, .. }
        | JadeError::TypeError { span, .. }
        | JadeError::IoError { span, .. } => {
            if span.line == 0 { *span = call_span; }
        }
        _ => {}
    }
    e
}

/// Resolve a mix of positional and named arguments into a positional Vec by
/// matching named args against the callee's parameter list.
fn resolve_named_args(
    callee: &VmValue,
    positional: Vec<VmValue>,
    named: Vec<(String, VmValue)>,
    span: Span,
) -> Result<Vec<VmValue>> {
    if named.is_empty() {
        return Ok(positional);
    }
    match callee {
        VmValue::Fn(cf) => {
            let params = &cf.params;
            let mut result = vec![VmValue::Nil; params.len()];
            for (i, v) in positional.into_iter().enumerate() {
                if i < result.len() { result[i] = v; }
            }
            for (name, v) in named {
                let pos = params.iter().position(|p| p == &name)
                    .ok_or_else(|| JadeError::TypeError {
                        message: format!("unknown parameter '{}'", name),
                        span,
                    })?;
                result[pos] = v;
            }
            Ok(result)
        }
        _ => {
            // For native/builtin/closure callees, append named values positionally.
            let mut args = positional;
            for (_, v) in named { args.push(v); }
            Ok(args)
        }
    }
}

#[async_recursion::async_recursion]
async fn call_value(
    callee: VmValue,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    match callee {
        VmValue::Fn(cf) => call_fn(&cf, args, state, span).await,
        VmValue::Closure(cf, captured) => {
            // Temporarily inject captured variables into globals so the closure body
            // sees them via GetGlobal. Save any displaced values and restore after.
            let mut saved: Vec<(String, Option<VmValue>)> = Vec::new();
            for (k, v) in captured.iter() {
                let old = state.globals.insert(k.clone(), v.clone());
                saved.push((k.clone(), old));
            }
            let result = call_fn(&cf, args, state, span).await;
            for (k, old) in saved {
                match old {
                    Some(v) => { state.globals.insert(k, v); }
                    None    => { state.globals.remove(&k); }
                }
            }
            result
        }
        VmValue::BoundMethod(bm) => {
            let method = Arc::clone(&bm.method);
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(VmValue::Struct(Arc::clone(&bm.receiver)));
            full_args.extend(args);
            call_fn(&method, full_args, state, span).await
        }
        VmValue::BuiltinFn(bf) => (bf.vm_impl)(&args).map_err(|e| patch_builtin_span(e, span)),
        VmValue::NativeLibFn(nfn) => nfn.call(&args, span),
        VmValue::NativeBoundMethod(nbm) => {
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(nbm.receiver.clone());
            full_args.extend(args);
            (nbm.method.vm_impl)(&full_args).map_err(|e| patch_builtin_span(e, span))
        }
        VmValue::NativeFn(nf) => match nf {
            NativeFnId::LlmSetMaxTokens => {
                if args.len() != 1 {
                    return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
                }
                match &args[0] {
                    VmValue::Int(n) if *n > 0 => {
                        state.max_tokens = *n as u32;
                        Ok(VmValue::Nil)
                    }
                    ref other => Err(JadeError::TypeError { message: format!("llm.set_max_tokens() requires a positive int, got {}", value_type_name(other)), span }),
                }
            }
            NativeFnId::Print => {
                if args.len() != 1 {
                    return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
                }
                match args.into_iter().next().unwrap() {
                    VmValue::TokenStream(ts) => {
                        vm_drain_token_stream_printing(ts, state, span, true).await?;
                        Ok(VmValue::Nil)
                    }
                    other => {
                        println!("{}", value_to_display(&other));
                        Ok(VmValue::Nil)
                    }
                }
            }
            NativeFnId::Stream => {
                if args.len() != 1 {
                    return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
                }
                match args.into_iter().next().unwrap() {
                    VmValue::TokenStream(ts) => {
                        let text = vm_drain_token_stream_printing(ts, state, span, true).await?;
                        Ok(VmValue::Str(text))
                    }
                    other => {
                        let s = value_to_display(&other);
                        println!("{}", s);
                        Ok(VmValue::Str(s))
                    }
                }
            }
            NativeFnId::Route => {
                if args.is_empty() || args.len() > 2 {
                    return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span });
                }
                let mut iter = args.into_iter();
                let obj = iter.next().unwrap();
                // If `on` is omitted, try route_configs for this struct's type.
                let on = iter.next().unwrap_or_else(|| {
                    if let VmValue::Struct(ref s) = obj {
                        let type_name = s.lock().type_name.clone();
                        if let Some(field_name) = state.route_configs.get(&type_name) {
                            let fields = s.lock();
                            return fields.fields.get(field_name)
                                .cloned()
                                .unwrap_or(VmValue::Nil);
                        }
                    }
                    VmValue::Nil
                });
                match on {
                    VmValue::Nil => Ok(obj),
                    VmValue::Str(method_name) => {
                        // Prefer the struct's own extend methods; fall back to globals.
                        let fn_val = if let VmValue::Struct(ref s) = obj {
                            let type_name = s.lock().type_name.clone();
                            state.extend_methods
                                .get(&type_name)
                                .and_then(|m| m.get(&method_name))
                                .map(|cf| VmValue::Fn(Arc::clone(cf)))
                                .or_else(|| state.globals.get(&method_name).cloned())
                        } else {
                            state.globals.get(&method_name).cloned()
                        };
                        match fn_val {
                            Some(f) => call_value(f, vec![obj], state, span).await,
                            None => Err(JadeError::Exception {
                                message: format!("route(): no method or function named {:?}", method_name),
                                span,
                            }),
                        }
                    }
                    other => Err(JadeError::TypeError {
                        message: format!("route(): expected string method name, got {}", value_to_display(&other)),
                        span,
                    }),
                }
            }
        },
        VmValue::TypeRef(type_name) => {
            if args.len() != 1 {
                return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
            }
            vm_type_call(type_name, args.into_iter().next().unwrap(), state, span)
        }
        _ => Err(JadeError::NotCallable { span }),
    }
}

/// Standalone version of `call_value` that owns its `VmState`, suitable for
/// passing to `tokio::spawn` where borrowed state cannot cross thread boundaries.
/// Always returns `(result, raised_exception)` so the parent can propagate the
/// exception value (struct/string) through try/catch rather than losing it.
#[async_recursion::async_recursion]
async fn call_value_standalone(
    callee: VmValue,
    args: Vec<VmValue>,
    mut state: VmState,
    span: Span,
) -> TaskBundle {
    let result = call_value(callee, args, &mut state, span).await;
    let raised = state.raised_exception.take();
    (result.map(|v| (v, state.token_count)), raised)
}

#[async_recursion::async_recursion]
async fn call_fn(
    cf: &CompiledFn,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // For bound methods `self` has already been prepended to `args`.
    // Fill trailing defaults for any omitted optional parameters.
    let mut args = args;
    if args.len() < cf.params.len() {
        let missing_start = args.len();
        for i in missing_start..cf.params.len() {
            match cf.defaults.get(i).and_then(|d| d.as_ref()) {
                Some(default) => args.push(default.clone()),
                None => return Err(JadeError::ArityMismatch {
                    expected: cf.params.len(),
                    got: missing_start,
                    span,
                }),
            }
        }
    } else if args.len() > cf.params.len() {
        return Err(JadeError::ArityMismatch {
            expected: cf.params.len(),
            got: args.len(),
            span,
        });
    }
    // Build the frame: params occupy slots 0..params.len(); rest start as Nil.
    let n = (cf.n_slots as usize).max(cf.params.len());
    let mut frame = vec![VmValue::Nil; n];
    for (i, v) in args.into_iter().enumerate() {
        frame[i] = vm_maybe_drain(v, state, span).await?;
    }
    let result = execute_chunk(&cf.chunk, &mut frame, state).await?;
    Ok(result.unwrap_or(VmValue::Nil))
}


// ── Prompt deref ──────────────────────────────────────────────────────────────

async fn vm_prompt_deref(
    prompt_text: String,
    output_type: Option<&str>,
    grammar_override: Option<String>,
    grammar_anchor: Option<String>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // Cache check — skip inference entirely on a repeated (prompt, type) pair.
    let cache_key = (prompt_text.clone(), output_type.map(str::to_owned));
    if let Some(cached) = state.prompt_cache.get(&cache_key).cloned() {
        return match output_type {
            None => Ok(VmValue::Str(cached)),
            Some(type_name) => {
                let struct_defs = state.struct_defs.clone();
                let v = coerce(cached.trim(), type_name, &struct_defs).map_err(|_| {
                    JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: 1, span }
                })?;
                apply_struct_decorators(v, type_name, state, span).await
            }
        };
    }

    // Clone the Arc so we don't hold a borrow of state across .await points.
    let backend = state.inference_backend.as_ref()
        .ok_or(JadeError::MissingApiKey { span })?
        .clone();

    // Grammar priority: user-supplied override > auto-generated from output type.
    let grammar = grammar_override.or_else(|| {
        output_type.and_then(|tn| crate::compiler::gbnf::grammar_for(tn, &state.struct_defs))
    });

    // Stateless call — no conversation history is sent or recorded.
    // Conversational memory is the JadeLang program's responsibility.
    let initial_resp = backend.infer(llm::InferenceRequest {
        prompt: prompt_text.clone(),
        model: state.default_model.clone(),
        max_tokens: state.max_tokens,
        grammar: grammar.clone(),
        anchor: grammar_anchor.clone(),
    }, span).await?;

    state.token_count += initial_resp.tokens_used;
    let tc = state.token_count;
    state.globals.insert("__tokens__".to_string(), VmValue::Int(tc));

    let Some(type_name) = output_type else {
        state.prompt_cache.insert(cache_key, initial_resp.text.clone());
        return Ok(VmValue::Str(initial_resp.text));
    };

    // Typed deref: retry loop — send the raw error string directly to the model.
    let max_retries = state.max_retries;
    let mut current = initial_resp.text;
    let struct_defs = state.struct_defs.clone();

    let retry_max_tokens = if matches!(type_name, "int" | "float" | "bool" | "str") {
        RETRY_MAX_TOKENS
    } else {
        RETRY_MAX_TOKENS_COMPLEX
    };

    for attempt in 0..max_retries {
        match coerce(current.trim(), type_name, &struct_defs) {
            Ok(v) => {
                state.prompt_cache.insert(cache_key, current);
                return apply_struct_decorators(v, type_name, state, span).await;
            }
            Err(correction) => {
                let entry = VmValue::Str(format!(
                    "attempt {}: response={:?} hint={:?}",
                    attempt + 1, current.trim(), correction
                ));
                if let Some(VmValue::Array(arc)) = state.globals.get("__retry_log__") {
                    arc.lock().push(entry);
                }

                let retry = backend.infer(llm::InferenceRequest {
                    prompt: correction.clone(),
                    model: state.default_model.clone(),
                    max_tokens: retry_max_tokens,
                    grammar: grammar.clone(),
                    anchor: grammar_anchor.clone(),
                }, span).await?;
                current = retry.text;
            }
        }
    }

    match coerce(current.trim(), type_name, &struct_defs) {
        Ok(v) => {
            state.prompt_cache.insert(cache_key, current);
            apply_struct_decorators(v, type_name, state, span).await
        }
        Err(_) => Err(JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: max_retries + 1, span }),
    }
}

/// Resolve a possibly-dotted decorator name to a callable VmValue.
/// "tools.on_fail" → GetGlobal("tools") → GetMethod("on_fail") as BoundMethod.
/// Mirrors what the function-decorator emitter does with bytecode at compile time.
fn resolve_decorator_fn(dec_name: &str, state: &VmState) -> Option<VmValue> {
    if let Some(dot) = dec_name.find('.') {
        let base_name = &dec_name[..dot];
        let field_name = &dec_name[dot + 1..];
        match state.globals.get(base_name)?.clone() {
            VmValue::Struct(arc) => {
                let (type_name, field_val) = {
                    let guard = arc.lock();
                    (guard.type_name.clone(), guard.fields.get(field_name).cloned())
                };
                if let Some(v) = field_val {
                    return Some(v);
                }
                let mfn = state.extend_methods.get(&type_name)?.get(field_name)?.clone();
                Some(VmValue::BoundMethod(Arc::new(VmBoundMethod {
                    receiver: arc,
                    method: mfn,
                })))
            }
            VmValue::Dict(map) => map.get(field_name).cloned(),
            _ => None,
        }
    } else {
        state.globals.get(dec_name).cloned()
    }
}

/// Apply any struct decorators registered for `type_name` to a coerced value.
/// Called after every successful `coerce()` so decorator behaviour is identical
/// whether the struct came from a literal or from `?p |> Type`.
async fn apply_struct_decorators(
    mut v: VmValue,
    type_name: &str,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    let decs = state.struct_decorators.get(type_name).cloned().unwrap_or_default();
    for (dec_name, dec_args) in decs {
        if let Some(dec_fn) = resolve_decorator_fn(&dec_name, state) {
            let mut call_args = vec![v];
            call_args.extend(dec_args);
            v = call_value(dec_fn, call_args, state, span).await?;
        }
    }
    Ok(v)
}

/// Strip markdown code fences that LLMs often wrap JSON in (``` or ```json).
fn vm_extract_json(text: &str) -> String {
    let t = text.trim();
    let inner = t
        .strip_prefix("```json").or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    let t = inner.unwrap_or(t);
    // Scan forward through every `{` or `[` start position and return the first
    // candidate that is parseable JSON (after optional normalization).
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' || bytes[i] == b'[' {
            if let Some(end) = json_find_end(&t[i..]) {
                let candidate = &t[i..i + end];
                if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                    return candidate.to_owned();
                }
                // Try normalizing: quote unquoted keys, remove commas inside numbers.
                let normalized = json_normalize(candidate);
                if serde_json::from_str::<serde_json::Value>(&normalized).is_ok() {
                    return normalized;
                }
            }
        }
        i += 1;
    }
    t.to_owned()
}

/// Quote unquoted object keys and strip thousands-separator commas from numbers.
/// Handles the two most common model formatting mistakes: `{key: val}` and `1,000`.
fn json_normalize(s: &str) -> String {
    let s = json_quote_keys(s);
    json_strip_number_commas(&s)
}

fn json_quote_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape_next = false;
    while i < bytes.len() {
        if escape_next {
            escape_next = false;
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if bytes[i] == b'\\' && in_string {
            escape_next = true;
            out.push('\\');
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = !in_string;
            out.push('"');
            i += 1;
            continue;
        }
        // Outside strings: detect unquoted key (word chars followed by ':').
        if !in_string && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &s[start..i];
            // Peek past whitespace to see if ':' follows (and it's not '::').
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
            let is_key = j < bytes.len() && bytes[j] == b':'
                && (j + 1 >= bytes.len() || bytes[j + 1] != b':');
            if is_key {
                out.push('"');
                out.push_str(word);
                out.push('"');
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn json_strip_number_commas(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
        }
        // Skip comma that sits between two digits outside a string.
        if !in_string
            && bytes[i] == b','
            && i > 0 && bytes[i - 1].is_ascii_digit()
            && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()
        {
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Scan `s` for the end of the first top-level JSON object or array, respecting
/// string escapes and nesting. Returns the exclusive byte index after the closing
/// bracket/brace, or `None` if `s` contains no top-level `{` or `[`.
fn json_find_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let (open, close) = if bytes[start] == b'{' { (b'{', b'}') } else { (b'[', b']') };
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => { i += 2; }
            b'"' => { in_string = !in_string; i += 1; }
            b if !in_string && b == open  => { depth += 1; i += 1; }
            b if !in_string && b == close => {
                depth -= 1;
                i += 1;
                if depth == 0 { return Some(i); }
            }
            _ => { i += 1; }
        }
    }
    None
}

/// Recursively convert a `serde_json::Value` to a `VmValue`.
fn json_to_vm_value(json: &serde_json::Value) -> std::result::Result<VmValue, String> {
    match json {
        serde_json::Value::Null => Err("null is not a valid Jade value".to_string()),
        serde_json::Value::Bool(b) => Ok(VmValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { Ok(VmValue::Int(i)) }
            else if let Some(f) = n.as_f64() { Ok(VmValue::Float(f)) }
            else { Err(format!("number {} cannot be represented as int or float", n)) }
        }
        serde_json::Value::String(s) => Ok(VmValue::Str(s.clone())),
        serde_json::Value::Array(arr) => arr.iter().enumerate()
            .map(|(i, v)| json_to_vm_value(v).map_err(|e| format!("element {}: {}", i, e)))
            .collect::<std::result::Result<Vec<VmValue>, String>>()
            .map(|v| VmValue::Array(Arc::new(Mutex::new(v)))),
        serde_json::Value::Object(obj) => obj.iter()
            .map(|(k, v)| json_to_vm_value(v)
                .map(|val| (k.clone(), val))
                .map_err(|e| format!("field '{}': {}", k, e)))
            .collect::<std::result::Result<HashMap<String, VmValue>, String>>()
            .map(VmValue::Dict),
    }
}

/// Summarise struct field names and optionality for LLM error messages.
fn vm_field_summary(def: &[StructFieldDef]) -> String {
    def.iter().map(|f| match f {
        StructFieldDef::Required(n)      => format!("{} (required)", n),
        StructFieldDef::Let { name, .. } => format!("{} (optional)", name),
        StructFieldDef::Prompt { name, .. } => format!("{} (prompt, optional)", name),
    }).collect::<Vec<_>>().join(", ")
}

/// Parse an LLM JSON response into a struct `VmValue`.
fn vm_coerce_struct(
    text: &str,
    type_name: &str,
    def: &[StructFieldDef],
) -> std::result::Result<VmValue, String> {
    let raw = vm_extract_json(text);
    let json: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!(
        "Your response could not be parsed as a {} struct: {}. \
         Respond with a JSON object with fields: {}.",
        type_name, e, vm_field_summary(def)
    ))?;
    let obj = json.as_object().ok_or_else(|| format!(
        "Your response is not a JSON object. \
         Respond with a JSON object for struct '{}' with fields: {}.",
        type_name, vm_field_summary(def)
    ))?;

    let mut fields: HashMap<String, VmValue> = HashMap::new();
    for field_def in def {
        match field_def {
            StructFieldDef::Required(name) => {
                let raw_val = obj.get(name.as_str()).ok_or_else(|| format!(
                    "Missing required field '{}' for struct '{}'. \
                     Respond with a JSON object containing all required fields: {}.",
                    name, type_name, vm_field_summary(def)
                ))?;
                let val = json_to_vm_value(raw_val).map_err(|e| format!(
                    "Field '{}' is invalid: {}. \
                     Respond with a corrected JSON object for struct '{}'.",
                    name, e, type_name
                ))?;
                fields.insert(name.clone(), val);
            }
            StructFieldDef::Let { name, .. } => {
                if let Some(raw_val) = obj.get(name.as_str()) {
                    let val = json_to_vm_value(raw_val).map_err(|e| format!(
                        "Field '{}' is invalid: {}. \
                         Respond with a corrected JSON object for struct '{}'.",
                        name, e, type_name
                    ))?;
                    fields.insert(name.clone(), val);
                }
            }
            StructFieldDef::Prompt { name, .. } => {
                if let Some(raw_val) = obj.get(name.as_str()) {
                    let s = raw_val.as_str().ok_or_else(|| format!(
                        "Prompt field '{}' must be a string value.", name
                    ))?;
                    fields.insert(name.clone(), VmValue::Prompt(s.to_string()));
                }
            }
        }
    }

    Ok(VmValue::Struct(Arc::new(Mutex::new(VmStruct {
        type_name: type_name.to_string(),
        fields,
    }))))
}

/// Start a streaming inference call and return a lazy `VmValue::TokenStream`.
/// Cache hits short-circuit to `VmValue::Str` — drain logic handles both transparently.
async fn vm_prompt_deref_stream(
    prompt_text: String,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    let cache_key = (prompt_text.clone(), None::<String>);
    if let Some(cached) = state.prompt_cache.get(&cache_key).cloned() {
        return Ok(VmValue::Str(cached));
    }
    let backend = state.inference_backend.as_ref()
        .ok_or(JadeError::MissingApiKey { span })?
        .clone();
    let (rx, handle) = backend.infer_stream(llm::InferenceRequest {
        prompt: prompt_text.clone(),
        model: state.default_model.clone(),
        max_tokens: state.max_tokens,
        grammar: None,
        anchor: None,
    }, span).await?;
    Ok(VmValue::TokenStream(Arc::new(JadeTokenStream {
        rx: Mutex::new(Some(rx)),
        tokens_handle: Mutex::new(Some(handle)),
        prompt_key: (prompt_text, None),
    })))
}

/// Drain a `TokenStream` silently into a `VmValue::Str`, updating token count and cache.
async fn vm_drain_token_stream(
    ts: Arc<JadeTokenStream>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    let rx_opt = ts.rx.lock().take();
    let mut rx = rx_opt.ok_or(JadeError::DoubleStreamDrain { span })?;
    let mut text = String::new();
    while let Some(token) = rx.recv().await {
        text.push_str(&token);
    }
    let h_opt = ts.tokens_handle.lock().take();
    if let Some(h) = h_opt {
        match h.await {
            Ok(Ok(tokens)) => {
                state.token_count += tokens;
                let tc = state.token_count;
                state.globals.insert("__tokens__".to_string(), VmValue::Int(tc));
            }
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(JadeError::AsyncPanic {
                message: format!("token stream task panicked: {e}"),
                span,
            }),
        }
    }
    state.prompt_cache.insert(ts.prompt_key.clone(), text.clone());
    Ok(VmValue::Str(text))
}

/// Drain a `TokenStream`, printing each token to stdout as it arrives.
/// Returns the accumulated text so `stream()` can return it as a `Str`.
async fn vm_drain_token_stream_printing(
    ts: Arc<JadeTokenStream>,
    state: &mut VmState,
    span: Span,
    newline: bool,
) -> Result<String> {
    let rx_opt = ts.rx.lock().take();
    let mut rx = rx_opt.ok_or(JadeError::DoubleStreamDrain { span })?;
    let mut text = String::new();
    while let Some(token) = rx.recv().await {
        use std::io::Write as _;
        print!("{}", token);
        let _ = std::io::stdout().flush();
        text.push_str(&token);
    }
    if newline { println!(); }
    let h_opt = ts.tokens_handle.lock().take();
    if let Some(h) = h_opt {
        match h.await {
            Ok(Ok(tokens)) => {
                state.token_count += tokens;
                let tc = state.token_count;
                state.globals.insert("__tokens__".to_string(), VmValue::Int(tc));
            }
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(JadeError::AsyncPanic {
                message: format!("token stream task panicked: {e}"),
                span,
            }),
        }
    }
    state.prompt_cache.insert(ts.prompt_key.clone(), text.clone());
    Ok(text)
}

/// Drain a `TokenStream` to `Str` if the value is one; pass everything else through.
async fn vm_maybe_drain(v: VmValue, state: &mut VmState, span: Span) -> Result<VmValue> {
    match v {
        VmValue::TokenStream(ts) => vm_drain_token_stream(ts, state, span).await,
        other => Ok(other),
    }
}

/// Runtime implementation of callable type constructors: `int(x)`, `str(x)`, `City(dict)`, etc.
fn vm_type_call(
    type_name: String,
    arg: VmValue,
    state: &VmState,
    span: Span,
) -> Result<VmValue> {
    let err = |msg: String| Err(JadeError::Exception { message: msg, span });
    match type_name.as_str() {
        "int" => match arg {
            VmValue::Int(i)   => Ok(VmValue::Int(i)),
            VmValue::Float(f) => Ok(VmValue::Int(f as i64)),
            VmValue::Bool(b)  => Ok(VmValue::Int(if b { 1 } else { 0 })),
            VmValue::Str(s)   => s.trim().parse::<i64>()
                .map(VmValue::Int)
                .map_err(|_| JadeError::Exception {
                    message: format!("int(): cannot convert {:?} to int", s),
                    span,
                }),
            other => err(format!("int(): cannot convert {} to int", value_to_display(&other))),
        },
        "float" => match arg {
            VmValue::Float(f) => Ok(VmValue::Float(f)),
            VmValue::Int(i)   => Ok(VmValue::Float(i as f64)),
            VmValue::Bool(b)  => Ok(VmValue::Float(if b { 1.0 } else { 0.0 })),
            VmValue::Str(s)   => s.trim().parse::<f64>()
                .map(VmValue::Float)
                .map_err(|_| JadeError::Exception {
                    message: format!("float(): cannot convert {:?} to float", s),
                    span,
                }),
            other => err(format!("float(): cannot convert {} to float", value_to_display(&other))),
        },
        "bool" => match arg {
            VmValue::Bool(b)  => Ok(VmValue::Bool(b)),
            VmValue::Int(i)   => Ok(VmValue::Bool(i != 0)),
            VmValue::Float(f) => Ok(VmValue::Bool(f != 0.0)),
            VmValue::Nil      => Ok(VmValue::Bool(false)),
            VmValue::Str(s)   => match s.to_lowercase().as_str() {
                "true"  => Ok(VmValue::Bool(true)),
                "false" => Ok(VmValue::Bool(false)),
                ""      => Ok(VmValue::Bool(false)),
                _       => Ok(VmValue::Bool(true)),
            },
            other => Ok(VmValue::Bool(!matches!(other, VmValue::Nil))),
        },
        "str" => Ok(VmValue::Str(value_to_display(&arg))),
        "func" => match arg {
            VmValue::Str(name) => state.globals.get(&name).cloned().ok_or_else(|| {
                JadeError::Exception {
                    message: format!("func(): no function named {:?}", name),
                    span,
                }
            }),
            other if matches!(
                other,
                VmValue::Fn(_) | VmValue::Closure(_, _) | VmValue::BoundMethod(_) | VmValue::BuiltinFn(_)
            ) => Ok(other),
            other => err(format!("func(): expected a string or function, got {}", value_to_display(&other))),
        },
        name => {
            if let Some(def) = state.struct_defs.get(name) {
                match arg {
                    VmValue::Dict(map) => {
                        let mut fields = HashMap::new();
                        for field_def in def {
                            let fname = field_def.name();
                            if let Some(v) = map.get(fname) {
                                fields.insert(fname.to_string(), v.clone());
                            } else {
                                fields.insert(fname.to_string(), VmValue::Nil);
                            }
                        }
                        Ok(VmValue::Struct(Arc::new(Mutex::new(VmStruct {
                            type_name: name.to_string(),
                            fields,
                        }))))
                    }
                    VmValue::Struct(s) if s.lock().type_name == name => Ok(VmValue::Struct(s)),
                    other => err(format!(
                        "{}(): cannot construct from {}", name, value_to_display(&other)
                    )),
                }
            } else {
                err(format!("{}(): unknown type", name))
            }
        }
    }
}

/// Try to coerce a raw LLM response to a `VmValue`.
/// Returns `Ok(value)` on success or `Err(correction_prompt)` on failure —
/// the correction is fed back to the LLM, never surfaced to the user directly.
fn coerce(
    text: &str,
    type_name: &str,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> std::result::Result<VmValue, String> {
    match type_name {
        "int" => text.parse::<i64>().map(VmValue::Int).map_err(|_| format!(
            "Your response {:?} could not be parsed as an integer. \
             Respond with only a plain integer, e.g. 42.",
            text
        )),
        "float" => text.parse::<f64>().map(VmValue::Float).map_err(|_| format!(
            "Your response {:?} could not be parsed as a float. \
             Respond with only a plain float, e.g. 3.14.",
            text
        )),
        "str" => Ok(VmValue::Str(text.to_string())),
        "bool" => match text.to_lowercase().as_str() {
            "true"  => Ok(VmValue::Bool(true)),
            "false" => Ok(VmValue::Bool(false)),
            _ => Err(format!(
                "Your response {:?} could not be parsed as a boolean. \
                 Respond with only 'true' or 'false'.",
                text
            )),
        },
        "Array" | "array" => {
            let raw = vm_extract_json(text);
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON array: {}. \
                     Respond with only a JSON array, e.g. [1, \"two\", true].",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Array(arr) => arr.iter().enumerate()
                        .map(|(i, elem)| json_to_vm_value(elem)
                            .map_err(|e| format!("element {}: {}", i, e)))
                        .collect::<std::result::Result<Vec<VmValue>, String>>()
                        .map(|v| VmValue::Array(Arc::new(Mutex::new(v))))
                        .map_err(|e| format!(
                            "Your response array could not be fully converted: {}. \
                             Respond with only a JSON array of int, float, bool, or string values.",
                            e
                        )),
                    _ => Err("Your response is not a JSON array. \
                              Respond with only a JSON array, e.g. [1, \"two\", true].".to_string()),
                })
        }
        "Dict" | "dict" => {
            let raw = vm_extract_json(text);
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!(
                    "Your response could not be parsed as a JSON object: {}. \
                     Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                    e
                ))
                .and_then(|v| match v {
                    serde_json::Value::Object(obj) => obj.iter()
                        .map(|(k, val)| json_to_vm_value(val)
                            .map(|v| (k.clone(), v))
                            .map_err(|e| format!("field '{}': {}", k, e)))
                        .collect::<std::result::Result<HashMap<String, VmValue>, String>>()
                        .map(VmValue::Dict)
                        .map_err(|e| format!(
                            "Your response dict could not be fully converted: {}. \
                             Respond with only a JSON object, e.g. {{\"key\": \"value\"}}.",
                            e
                        )),
                    _ => Err("Your response is not a JSON object. \
                              Respond with only a JSON object, e.g. {\"key\": \"value\"}.".to_string()),
                })
        }
        name => {
            if let Some(def) = struct_defs.get(name) {
                vm_coerce_struct(text, name, def)
            } else {
                Err(format!(
                    "Unknown type '{}'. Cannot coerce LLM response to this type.", name
                ))
            }
        }
    }
}

// ── Dynamic dispatch helpers ──────────────────────────────────────────────────

fn eval_binop_dynamic(op: &BinOpKind, l: VmValue, r: VmValue, span: Span) -> Result<VmValue> {
    use BinOpKind::*;
    match op {
        Add => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b))     => a.checked_add(b).ok_or(JadeError::IntegerOverflow{span}).map(VmValue::Int),
            (VmValue::Str(a), VmValue::Str(b))     => Ok(VmValue::Str(a + &b)),
            (a, b) => { let (af, bf) = to_floats(a, b, op, span)?; Ok(VmValue::Float(af + bf)) }
        },
        Sub => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b))     => a.checked_sub(b).ok_or(JadeError::IntegerOverflow{span}).map(VmValue::Int),
            (a, b) => { let (af, bf) = to_floats(a, b, op, span)?; Ok(VmValue::Float(af - bf)) }
        },
        Mul => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b))     => a.checked_mul(b).ok_or(JadeError::IntegerOverflow{span}).map(VmValue::Int),
            (a, b) => { let (af, bf) = to_floats(a, b, op, span)?; Ok(VmValue::Float(af * bf)) }
        },
        Div => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => {
                if b == 0 { Err(JadeError::DivisionByZero{span}) } else { Ok(VmValue::Int(a/b)) }
            }
            (a, b) => {
                let (af, bf) = to_floats(a, b, op, span)?;
                if bf == 0.0 { Err(JadeError::DivisionByZero{span}) } else { Ok(VmValue::Float(af/bf)) }
            }
        },
        Mod => match (l, r) {
            (VmValue::Int(a), VmValue::Int(b)) => {
                if b == 0 { Err(JadeError::RemainderByZero{span}) } else { Ok(VmValue::Int(a%b)) }
            }
            (a, b) => {
                let (af, bf) = to_floats(a, b, op, span)?;
                if bf == 0.0 { Err(JadeError::RemainderByZero{span}) } else { Ok(VmValue::Float(af%bf)) }
            }
        },
        BitAnd => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a&b)), (l,r) => Err(JadeError::TypeError{message:format!("'&' requires int operands, got {} and {}", value_type_name(&l), value_type_name(&r)),span}) },
        BitOr  => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a|b)), (l,r) => Err(JadeError::TypeError{message:format!("'|' requires int operands, got {} and {}", value_type_name(&l), value_type_name(&r)),span}) },
        BitXor => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a^b)), (l,r) => Err(JadeError::TypeError{message:format!("'^' requires int operands, got {} and {}", value_type_name(&l), value_type_name(&r)),span}) },
        Shl => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b)) => {
                if b<0||b>=64 { Err(JadeError::InvalidShift{amount:b,span}) } else { Ok(VmValue::Int(a<<b as u32)) }
            }
            _ => Err(JadeError::TypeError{message:"'<<' requires int operands".to_string(),span})
        },
        Shr => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b)) => {
                if b<0||b>=64 { Err(JadeError::InvalidShift{amount:b,span}) } else { Ok(VmValue::Int(a>>b as u32)) }
            }
            _ => Err(JadeError::TypeError{message:"'>>' requires int operands".to_string(),span})
        },
        Eq => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b))       => Ok(VmValue::Bool(a==b)),
            (VmValue::Float(a),VmValue::Float(b))   => Ok(VmValue::Bool(a==b)),
            (VmValue::Bool(a),VmValue::Bool(b))     => Ok(VmValue::Bool(a==b)),
            (VmValue::Str(a),VmValue::Str(b))       => Ok(VmValue::Bool(a==b)),
            (VmValue::Nil, VmValue::Nil)            => Ok(VmValue::Bool(true)),
            (VmValue::Nil, _) | (_, VmValue::Nil)  => Ok(VmValue::Bool(false)),
            (l,r) => Err(JadeError::TypeError{message:format!("'==' cannot compare {} and {}", value_type_name(&l), value_type_name(&r)),span})
        },
        Ne => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b))       => Ok(VmValue::Bool(a!=b)),
            (VmValue::Float(a),VmValue::Float(b))   => Ok(VmValue::Bool(a!=b)),
            (VmValue::Bool(a),VmValue::Bool(b))     => Ok(VmValue::Bool(a!=b)),
            (VmValue::Str(a),VmValue::Str(b))       => Ok(VmValue::Bool(a!=b)),
            (VmValue::Nil, VmValue::Nil)            => Ok(VmValue::Bool(false)),
            (VmValue::Nil, _) | (_, VmValue::Nil)  => Ok(VmValue::Bool(true)),
            (l,r) => Err(JadeError::TypeError{message:format!("'!=' cannot compare {} and {}", value_type_name(&l), value_type_name(&r)),span})
        },
        Lt => cmp_order(l,r,"<",span,|a:f64,b:f64| a<b, |a:i64,b:i64| a<b, |a:&str,b:&str| a<b, |a:bool,b:bool| !a&&b),
        Gt => cmp_order(l,r,">",span,|a:f64,b:f64| a>b, |a:i64,b:i64| a>b, |a:&str,b:&str| a>b, |a:bool,b:bool| a&&!b),
        Le => cmp_order(l,r,"<=",span,|a:f64,b:f64| a<=b,|a:i64,b:i64| a<=b,|a:&str,b:&str| a<=b,|a:bool,b:bool| a==b||(!a&&b)),
        Ge => cmp_order(l,r,">=",span,|a:f64,b:f64| a>=b,|a:i64,b:i64| a>=b,|a:&str,b:&str| a>=b,|a:bool,b:bool| a==b||(a&&!b)),
        In => vm_contains(l, r, span).map(VmValue::Bool),
        NotIn => vm_contains(l, r, span).map(|b| VmValue::Bool(!b)),
        And | Or => unreachable!("short-circuit ops must not reach BinOp dynamic dispatch"),
    }
}

fn vm_scalar_eq(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x),   VmValue::Int(y))   => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        (VmValue::Bool(x),  VmValue::Bool(y))  => x == y,
        (VmValue::Str(x),   VmValue::Str(y))   => x == y,
        (VmValue::Nil,      VmValue::Nil)      => true,
        _ => false,
    }
}

fn vm_contains(needle: VmValue, haystack: VmValue, span: Span) -> Result<bool> {
    match haystack {
        VmValue::Array(arc) => {
            let arr = arc.lock();
            Ok(arr.iter().any(|v| vm_scalar_eq(v, &needle)))
        }
        VmValue::Dict(map) => {
            let key = match needle {
                VmValue::Str(s) => s,
                ref other => return Err(JadeError::TypeError { message: format!("'in' dict key must be str, got {}", value_type_name(other)), span }),
            };
            Ok(map.contains_key(&key))
        }
        VmValue::Str(s) => {
            let sub = match needle {
                VmValue::Str(sub) => sub,
                ref other => return Err(JadeError::TypeError { message: format!("'in' substring must be str, got {}", value_type_name(other)), span }),
            };
            Ok(s.contains(sub.as_str()))
        }
        ref other => Err(JadeError::TypeError { message: format!("'in' requires array, dict, or str, got {}", value_type_name(other)), span }),
    }
}

fn cmp_order(
    l: VmValue, r: VmValue, op: &str, span: Span,
    ff: impl Fn(f64,f64)->bool,
    ii: impl Fn(i64,i64)->bool,
    ss: impl Fn(&str,&str)->bool,
    bb: impl Fn(bool,bool)->bool,
) -> Result<VmValue> {
    match (l, r) {
        (VmValue::Int(a),   VmValue::Int(b))   => Ok(VmValue::Bool(ii(a,b))),
        (VmValue::Float(a), VmValue::Float(b)) => Ok(VmValue::Bool(ff(a,b))),
        (VmValue::Int(a),   VmValue::Float(b)) => Ok(VmValue::Bool(ff(a as f64,b))),
        (VmValue::Float(a), VmValue::Int(b))   => Ok(VmValue::Bool(ff(a,b as f64))),
        (VmValue::Bool(a),  VmValue::Bool(b))  => Ok(VmValue::Bool(bb(a,b))),
        (VmValue::Str(a),   VmValue::Str(b))   => Ok(VmValue::Bool(ss(&a,&b))),
        (l, r) => Err(JadeError::TypeError { message: format!("'{}' cannot compare {} and {}", op, value_type_name(&l), value_type_name(&r)), span }),
    }
}

fn eval_unaryop_dynamic(op: &UnaryOpKind, v: VmValue, span: Span) -> Result<VmValue> {
    match op {
        UnaryOpKind::BitNot => match v { VmValue::Int(i) => Ok(VmValue::Int(!i)), ref v => Err(JadeError::TypeError{message:format!("'~' requires int, got {}", value_type_name(v)),span}) },
        UnaryOpKind::Not    => match v { VmValue::Bool(b)=> Ok(VmValue::Bool(!b)), ref v => Err(JadeError::TypeError{message:format!("'!' requires bool, got {}", value_type_name(v)),span}) },
        UnaryOpKind::Neg    => match v {
            VmValue::Int(i)   => Ok(VmValue::Int(-i)),
            VmValue::Float(f) => Ok(VmValue::Float(-f)),
            ref v => Err(JadeError::TypeError{message:format!("unary '-' requires int or float, got {}", value_type_name(v)),span})
        },
    }
}

fn to_floats(l: VmValue, r: VmValue, op: &BinOpKind, span: Span) -> Result<(f64, f64)> {
    let lf = match l { VmValue::Int(i) => i as f64, VmValue::Float(f) => f, _ => return Err(JadeError::TypeError { message: format!("{:?} requires numeric operands", op), span }) };
    let rf = match r { VmValue::Int(i) => i as f64, VmValue::Float(f) => f, _ => return Err(JadeError::TypeError { message: format!("{:?} requires numeric operands", op), span }) };
    Ok((lf, rf))
}

fn cmp_dynamic(slots: &[VmValue], l: Reg, r: Reg, op: &str, span: Span) -> Result<VmValue> {
    let lv = get(slots, l).clone();
    let rv = get(slots, r).clone();
    let result = match op {
        "==" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a==b,
            (VmValue::Float(a),VmValue::Float(b)) => a==b,
            (VmValue::Bool(a),VmValue::Bool(b))   => a==b,
            (VmValue::Str(a),VmValue::Str(b))     => a==b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        "!=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a!=b,
            (VmValue::Float(a),VmValue::Float(b)) => a!=b,
            (VmValue::Bool(a),VmValue::Bool(b))   => a!=b,
            (VmValue::Str(a),VmValue::Str(b))     => a!=b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        "<"  => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a<b,
            (VmValue::Float(a),VmValue::Float(b)) => a<b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)<b,
            (VmValue::Float(a),VmValue::Int(b))   => a<(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => !a&&b,
            (VmValue::Str(a),VmValue::Str(b))     => a<b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        ">"  => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a>b,
            (VmValue::Float(a),VmValue::Float(b)) => a>b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)>b,
            (VmValue::Float(a),VmValue::Int(b))   => a>(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a&&!b,
            (VmValue::Str(a),VmValue::Str(b))     => a>b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        "<=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a<=b,
            (VmValue::Float(a),VmValue::Float(b)) => a<=b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)<=b,
            (VmValue::Float(a),VmValue::Int(b))   => a<=(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a==b||(!a&&b),
            (VmValue::Str(a),VmValue::Str(b))     => a<=b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        ">=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a>=b,
            (VmValue::Float(a),VmValue::Float(b)) => a>=b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)>=b,
            (VmValue::Float(a),VmValue::Int(b))   => a>=(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a==b||(a&&!b),
            (VmValue::Str(a),VmValue::Str(b))     => a>=b,
            (lv,rv) => return Err(JadeError::TypeError{message:format!("'{}' cannot compare {} and {}", op, value_type_name(&lv), value_type_name(&rv)),span}),
        },
        _ => unreachable!(),
    };
    Ok(VmValue::Bool(result))
}

fn vm_index(obj: VmValue, idx: VmValue, span: Span) -> Result<VmValue> {
    match (obj, idx) {
        (VmValue::Str(s), VmValue::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(VmValue::Str(chars[i as usize].to_string()))
            }
        }
        (VmValue::Array(arc), VmValue::Int(i)) => {
            let guard = arc.lock();
            let len = guard.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(guard[i as usize].clone())
            }
        }
        (VmValue::Dict(m), VmValue::Str(k)) => {
            m.get(&k).cloned().ok_or_else(|| JadeError::KeyNotFound { key: k, span })
        }
        (VmValue::Dict(_), idx) => Err(JadeError::TypeError { message: format!("dict index must be str, got {}", value_type_name(&idx)), span }),
        (obj, idx) => Err(JadeError::TypeError { message: format!("value of type {} is not indexable with {}", value_type_name(&obj), value_type_name(&idx)), span }),
    }
}

// ── Slot helpers ──────────────────────────────────────────────────────────────

#[inline]
fn get(slots: &[VmValue], r: Reg) -> &VmValue {
    // Registers outside the allocated range are treated as Nil; safe
    // because we size frames conservatively in execute_chunk.
    slots.get(r as usize).unwrap_or(&VmValue::Nil)
}

#[inline]
fn set(slots: &mut Vec<VmValue>, r: Reg, v: VmValue) {
    ensure_slot(slots, r);
    slots[r as usize] = v;
}

#[inline]
fn ensure_slot(slots: &mut Vec<VmValue>, r: Reg) {
    if r as usize >= slots.len() {
        slots.resize(r as usize + 1, VmValue::Nil);
    }
}

fn get_int(slots: &[VmValue], r: Reg, span: Span) -> Result<i64> {
    match get(slots, r) {
        VmValue::Int(i) => Ok(*i),
        _ => Err(JadeError::TypeError { message: "expected int".to_string(), span }),
    }
}

fn get_flt(slots: &[VmValue], r: Reg, span: Span) -> Result<f64> {
    match get(slots, r) {
        VmValue::Float(f) => Ok(*f),
        _ => Err(JadeError::TypeError { message: "expected float".to_string(), span }),
    }
}

fn get_bool(slots: &[VmValue], r: Reg, span: Span) -> Result<bool> {
    match get(slots, r) {
        VmValue::Bool(b) => Ok(*b),
        _ => Err(JadeError::TypeError { message: "expected bool".to_string(), span }),
    }
}

fn get_str(slots: &[VmValue], r: Reg, span: Span) -> Result<String> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.clone()),
        _ => Err(JadeError::TypeError { message: "expected str".to_string(), span }),
    }
}

/// Borrow a string slot by reference.  Use this instead of `get_str` when the
/// caller only needs to read the string (e.g. for comparisons) and does not
/// need an owned `String`.  Avoids a heap allocation per comparison.
fn get_str_ref<'a>(slots: &'a [VmValue], r: Reg, span: Span) -> Result<&'a str> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.as_str()),
        _ => Err(JadeError::TypeError { message: "expected str".to_string(), span }),
    }
}

fn int2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(i64, i64)> {
    Ok((get_int(slots, l, span)?, get_int(slots, r, span)?))
}

fn flt2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(f64, f64)> {
    Ok((get_flt(slots, l, span)?, get_flt(slots, r, span)?))
}

fn bool2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(bool, bool)> {
    Ok((get_bool(slots, l, span)?, get_bool(slots, r, span)?))
}

/// Borrow both string slots for comparison.  Returns `(&str, &str)` to avoid
/// cloning both `String`s when only an equality or ordering check is needed.
fn str2<'a>(slots: &'a [VmValue], l: Reg, r: Reg, span: Span) -> Result<(&'a str, &'a str)> {
    Ok((get_str_ref(slots, l, span)?, get_str_ref(slots, r, span)?))
}

/// Walk an instruction and return the highest register index it references.
/// Used to size the slots vec defensively in `execute_chunk`.
/// Evaluate a struct field default expression if it is a simple literal.
/// Returns None for non-literal defaults (they stay unset and will cause a
/// runtime error if accessed — the same behaviour as before this fix).
fn eval_literal_default(expr: &crate::frontend::ast::Expr) -> Option<VmValue> {
    use crate::frontend::ast::Expr;
    match expr {
        Expr::Str { value, .. }     => Some(VmValue::Str(value.clone())),
        Expr::Integer { value, .. } => Some(VmValue::Int(*value)),
        Expr::Float { value, .. }   => Some(VmValue::Float(*value)),
        Expr::Bool { value, .. }    => Some(VmValue::Bool(*value)),
        Expr::Identifier { name, .. } if name == "nil" => Some(VmValue::Nil),
        Expr::Array { elements, .. } if elements.is_empty() =>
            Some(VmValue::Array(Arc::new(Mutex::new(vec![])))),
        Expr::Dict { entries, .. } if entries.is_empty() =>
            Some(VmValue::Dict(HashMap::new())),
        _ => None,
    }
}

fn instr_max_reg(instr: &Instr) -> u32 {
    match instr {
        Instr::LoadInt(d,_)|Instr::LoadFloat(d,_)|Instr::LoadBool(d,_)
        |Instr::LoadStr(d,_)|Instr::LoadNil(d)|Instr::LoadFn(d,_)
        |Instr::MakeClosure(d,_) => *d,
        Instr::GetLocal(d,_)|Instr::GetGlobal(d,_) => *d,
        Instr::Move(d,s)|Instr::NegInt(d,s)|Instr::NegFloat(d,s)
        |Instr::IntToFloat(d,s)|Instr::BitNot(d,s)|Instr::Not(d,s)
        |Instr::MakePrompt(d,s)
        |Instr::UnaryOp(d,_,s)
        |Instr::PromptDeref(d,s,_,None) => (*d).max(*s),
        Instr::PromptDeref(d,s,_,Some(g)) => (*d).max(*s).max(*g),
        Instr::SetGlobal(_,s)|Instr::SetLocal(_,s) => *s,
        Instr::AddInt(d,l,r)|Instr::SubInt(d,l,r)|Instr::MulInt(d,l,r)
        |Instr::DivInt(d,l,r)|Instr::ModInt(d,l,r)
        |Instr::AddFloat(d,l,r)|Instr::SubFloat(d,l,r)
        |Instr::MulFloat(d,l,r)|Instr::DivFloat(d,l,r)
        |Instr::ConcatStr(d,l,r)
        |Instr::BitAnd(d,l,r)|Instr::BitOr(d,l,r)|Instr::BitXor(d,l,r)
        |Instr::Shl(d,l,r)|Instr::Shr(d,l,r)
        |Instr::CmpEqInt(d,l,r)|Instr::CmpNeInt(d,l,r)|Instr::CmpLtInt(d,l,r)
        |Instr::CmpGtInt(d,l,r)|Instr::CmpLeInt(d,l,r)|Instr::CmpGeInt(d,l,r)
        |Instr::CmpEqFloat(d,l,r)|Instr::CmpNeFloat(d,l,r)|Instr::CmpLtFloat(d,l,r)
        |Instr::CmpGtFloat(d,l,r)|Instr::CmpLeFloat(d,l,r)|Instr::CmpGeFloat(d,l,r)
        |Instr::CmpLtIntFloat(d,l,r)|Instr::CmpGtIntFloat(d,l,r)
        |Instr::CmpLeIntFloat(d,l,r)|Instr::CmpGeIntFloat(d,l,r)
        |Instr::CmpLtFloatInt(d,l,r)|Instr::CmpGtFloatInt(d,l,r)
        |Instr::CmpLeFloatInt(d,l,r)|Instr::CmpGeFloatInt(d,l,r)
        |Instr::CmpEqBool(d,l,r)|Instr::CmpNeBool(d,l,r)|Instr::CmpLtBool(d,l,r)
        |Instr::CmpGtBool(d,l,r)|Instr::CmpLeBool(d,l,r)|Instr::CmpGeBool(d,l,r)
        |Instr::CmpEqStr(d,l,r)|Instr::CmpNeStr(d,l,r)|Instr::CmpLtStr(d,l,r)
        |Instr::CmpGtStr(d,l,r)|Instr::CmpLeStr(d,l,r)|Instr::CmpGeStr(d,l,r)
        |Instr::CmpEq(d,l,r)|Instr::CmpNe(d,l,r)|Instr::CmpLt(d,l,r)
        |Instr::CmpGt(d,l,r)|Instr::CmpLe(d,l,r)|Instr::CmpGe(d,l,r)
        |Instr::BinOp(d,_,l,r)
        |Instr::GetIndex(d,l,r) => (*d).max(*l).max(*r),
        Instr::GetField(d,o,_) => (*d).max(*o),
        Instr::SetIndex(o,i,v) => (*o).max(*i).max(*v),
        Instr::SetField(o,_,v) => (*o).max(*v),
        Instr::JumpIfFalse(c,_)|Instr::JumpIfTrue(c,_) => *c,
        Instr::Jump(_)|Instr::Halt|Instr::Return(None)
        |Instr::ImportFile(_,_)|Instr::ImportFrom(_,_) => 0,
        Instr::Return(Some(r)) => *r,
        Instr::Call(d,c,args) => {
            let mut m = (*d).max(*c);
            for &a in args { m = m.max(a); }
            m
        }
        Instr::MakeArray(d, regs) => {
            let mut m = *d;
            for &r in regs { m = m.max(r); }
            m
        }
        Instr::MakeDict(d, pairs) => {
            let mut m = *d;
            for &(k,v) in pairs { m = m.max(k).max(v); }
            m
        }
        Instr::MakeStruct(d,_,fields) => {
            let mut m = *d;
            for (_,r,_) in fields { m = m.max(*r); }
            m
        }
        Instr::BuildFStr(d, parts) => {
            let mut m = *d;
            for p in parts { if let FStrPart::Reg(r) = p { m = m.max(*r); } }
            m
        }
        Instr::Raise(r)            => *r,
        Instr::SetupHandler(r, _)  => *r,
        Instr::PopHandler          => 0,
        Instr::GetTypeName(d, s)   => (*d).max(*s),
        Instr::Spawn(d, c, args) => {
            let mut m = (*d).max(*c);
            for &a in args { m = m.max(a); }
            m
        }
        Instr::Await(d, s) => (*d).max(*s),
        Instr::Join(d, regs) => {
            let mut m = *d;
            for &r in regs { m = m.max(r); }
            m
        }
        Instr::CallNamed(d, c, pairs) => {
            let mut m = (*d).max(*c);
            for (_, r) in pairs { m = m.max(*r); }
            m
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
