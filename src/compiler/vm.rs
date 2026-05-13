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
    Dict(HashMap<String, VmValue>),
    /// A pure Rust-backed callable (no VM state mutation). Used for stdlib
    /// core built-ins (print, len, write, input) and package functions.
    BuiltinFn(BuiltinFn),
    /// A BuiltinFn pre-loaded with its receiver for primitive method dispatch.
    NativeBoundMethod(Arc<NativeBoundMethod>),
    /// A Rust-backed callable returned by a built-in module (e.g. `llm.set_max_tokens`).
    NativeFn(NativeFnId),
    /// A handle to an in-flight async task.
    Future(Arc<JadeFuture>),
    /// A lazy token stream from an untyped prompt dereference.
    TokenStream(Arc<JadeTokenStream>),
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
            VmValue::Prompt(s)  => write!(f, "Prompt({:?})", s),
            VmValue::Dict(m)    => write!(f, "Dict({} key(s))", m.len()),
            VmValue::BuiltinFn(bf) => write!(f, "BuiltinFn({})", bf.name),
            VmValue::NativeBoundMethod(nbm) => write!(f, "NativeBoundMethod({})", nbm.method.name),
            VmValue::NativeFn(nf) => write!(f, "NativeFn({:?})", nf),
            VmValue::Future(_)      => write!(f, "Future"),
            VmValue::TokenStream(_) => write!(f, "TokenStream"),
            VmValue::Nil            => write!(f, "Nil"),
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
        VmValue::NativeFn(_)           => "<native fn>".to_string(),
        VmValue::Future(_)             => "<future>".to_string(),
        VmValue::TokenStream(_)        => "<token stream>".to_string(),
        VmValue::Nil                   => "nil".to_string(),
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
}

impl VmState {
    fn new() -> Self {
        let mut globals = HashMap::new();
        globals.insert("__tokens__".to_string(), VmValue::Int(0));
        globals.insert("__model__".to_string(), VmValue::Str(String::new()));
        globals.insert("__max_retries__".to_string(), VmValue::Int(3));
        globals.insert("__retry_log__".to_string(), VmValue::Array(Arc::new(Mutex::new(vec![]))));
        stdlib::seed_globals(&mut globals);
        VmState {
            raised_exception: None,
            globals,
            extend_methods: HashMap::new(),
            struct_defs: HashMap::new(),
            inference_backend: None,
            token_count: 0,
            max_retries: 3,
            max_tokens: DEFAULT_MAX_TOKENS,
            default_model: String::new(),
            prompt_cache: HashMap::new(),
            source_dir: PathBuf::new(),
            import_stack: HashSet::new(),
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
            inference_backend: self.inference_backend.clone(),
            token_count: 0,
            max_retries: self.max_retries,
            max_tokens: self.max_tokens,
            default_model: self.default_model.clone(),
            prompt_cache: self.prompt_cache.clone(),
            source_dir: self.source_dir.clone(),
            import_stack: HashSet::new(),
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
}

impl Default for VmOpts {
    fn default() -> Self {
        VmOpts {
            backend: None,
            default_model: String::new(),
            max_retries: 3,
            source_dir: std::env::current_dir().unwrap_or_default(),
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
        state.struct_defs.insert(k, v);
    }
    for (type_name, methods) in program.extend_methods {
        state.extend_methods.entry(type_name).or_default().extend(methods);
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
            Instr::ImportFile(path) => {
                // ── Built-in packages ───────────────────────────────────────
                // Intercept stdlib package names before touching the filesystem.
                if let Some(pkg) = stdlib::find_package(path) {
                    let val = if pkg.import_name == "llm" {
                        // llm has a state-mutating function — use the special dict builder
                        stdlib::llm_pkg::llm_vm_dict_value()
                    } else {
                        pkg.vm_dict_value()
                    };
                    state.globals.insert(pkg.global_name.to_string(), val);
                    continue;
                }

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

                // Save and update source_dir for the imported file's own imports.
                let prev_dir = state.source_dir.clone();
                state.source_dir = canon.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf();

                // Compile in a sync closure (lex/parse/emit are all sync).
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

                let result = match compile_result {
                    Ok(compiled) => Box::pin(run_with_state(compiled, state)).await,
                    Err(e) => Err(e),
                };

                // Always restore source_dir and release the import_stack entry.
                state.source_dir = prev_dir;
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
                    let key = match get(slots, kr).clone() {
                        VmValue::Str(s) => s,
                        _ => { vm_err!(JadeError::TypeError { op: "dict key".to_string(), span }); }
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
                        let i = match idx { VmValue::Int(n) => n, _ => { vm_err!(JadeError::TypeError { op: "array index".to_string(), span }); } };
                        let len = arc.lock().len();
                        if i < 0 || i as usize >= len { vm_err!(JadeError::IndexOutOfBounds { index: i, len, span }); }
                        arc.lock()[i as usize] = val;
                    }
                    VmValue::Dict(mut m) => {
                        let k = match idx { VmValue::Str(s) => s, _ => { vm_err!(JadeError::TypeError { op: "dict index".to_string(), span }); } };
                        m.insert(k, val);
                        slots[*obj_reg as usize] = VmValue::Dict(m);
                    }
                    _ => { vm_err!(JadeError::TypeError { op: "index assign".to_string(), span }); }
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
                set(slots, *dest, VmValue::Struct(Arc::new(Mutex::new(VmStruct {
                    type_name: type_name.clone(),
                    fields,
                }))));
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
                        op: "prompt declaration requires a string body".to_string(),
                        span,
                    }); }
                };
                set(slots, *dest, VmValue::Prompt(text));
            }
            Instr::PromptDeref(dest, prompt_reg, output_type) => {
                let text = match get(slots, *prompt_reg).clone() {
                    VmValue::Prompt(t) => t,
                    _ => { vm_err!(JadeError::NotAPrompt { name: "<expr>".to_string(), span }); }
                };
                let result = if output_type.is_none() {
                    vm_try!(vm_prompt_deref_stream(text, state, span).await)
                } else {
                    vm_try!(vm_prompt_deref(text, output_type.as_deref(), state, span).await)
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
                    _ => Err(JadeError::TypeError { op: "llm.set_max_tokens".to_string(), span }),
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
        },
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
    if args.len() != cf.params.len() {
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
                coerce(cached.trim(), type_name, &struct_defs).map_err(|_| {
                    JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: 1, span }
                })
            }
        };
    }

    // Clone the Arc so we don't hold a borrow of state across .await points.
    let backend = state.inference_backend.as_ref()
        .ok_or(JadeError::MissingApiKey { span })?
        .clone();
    // Stateless call — no conversation history is sent or recorded.
    // Conversational memory is the JadeLang program's responsibility.
    let initial_resp = backend.infer(llm::InferenceRequest {
        prompt: prompt_text.clone(),
        model: state.default_model.clone(),
        max_tokens: state.max_tokens,
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
                return Ok(v);
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
                }, span).await?;
                current = retry.text;
            }
        }
    }

    match coerce(current.trim(), type_name, &struct_defs) {
        Ok(v) => {
            state.prompt_cache.insert(cache_key, current);
            Ok(v)
        }
        Err(_) => Err(JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: max_retries + 1, span }),
    }
}

/// Strip markdown code fences that LLMs often wrap JSON in (``` or ```json).
fn vm_extract_json(text: &str) -> &str {
    let t = text.trim();
    let inner = t
        .strip_prefix("```json").or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    inner.unwrap_or(t)
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
    let json: serde_json::Value = serde_json::from_str(raw).map_err(|e| format!(
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
            serde_json::from_str::<serde_json::Value>(raw)
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
            serde_json::from_str::<serde_json::Value>(raw)
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
        BitAnd => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a&b)), _ => Err(JadeError::TypeError{op:"&".to_string(),span}) },
        BitOr  => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a|b)), _ => Err(JadeError::TypeError{op:"|".to_string(),span}) },
        BitXor => match (l,r) { (VmValue::Int(a),VmValue::Int(b)) => Ok(VmValue::Int(a^b)), _ => Err(JadeError::TypeError{op:"^".to_string(),span}) },
        Shl => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b)) => {
                if b<0||b>=64 { Err(JadeError::InvalidShift{amount:b,span}) } else { Ok(VmValue::Int(a<<b as u32)) }
            }
            _ => Err(JadeError::TypeError{op:"<<".to_string(),span})
        },
        Shr => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b)) => {
                if b<0||b>=64 { Err(JadeError::InvalidShift{amount:b,span}) } else { Ok(VmValue::Int(a>>b as u32)) }
            }
            _ => Err(JadeError::TypeError{op:">>".to_string(),span})
        },
        Eq => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b))     => Ok(VmValue::Bool(a==b)),
            (VmValue::Float(a),VmValue::Float(b)) => Ok(VmValue::Bool(a==b)),
            (VmValue::Bool(a),VmValue::Bool(b))   => Ok(VmValue::Bool(a==b)),
            (VmValue::Str(a),VmValue::Str(b))     => Ok(VmValue::Bool(a==b)),
            _ => Err(JadeError::TypeError{op:"==".to_string(),span})
        },
        Ne => match (l,r) {
            (VmValue::Int(a),VmValue::Int(b))     => Ok(VmValue::Bool(a!=b)),
            (VmValue::Float(a),VmValue::Float(b)) => Ok(VmValue::Bool(a!=b)),
            (VmValue::Bool(a),VmValue::Bool(b))   => Ok(VmValue::Bool(a!=b)),
            (VmValue::Str(a),VmValue::Str(b))     => Ok(VmValue::Bool(a!=b)),
            _ => Err(JadeError::TypeError{op:"!=".to_string(),span})
        },
        Lt => cmp_order(l,r,"<",span,|a:f64,b:f64| a<b, |a:i64,b:i64| a<b, |a:&str,b:&str| a<b, |a:bool,b:bool| !a&&b),
        Gt => cmp_order(l,r,">",span,|a:f64,b:f64| a>b, |a:i64,b:i64| a>b, |a:&str,b:&str| a>b, |a:bool,b:bool| a&&!b),
        Le => cmp_order(l,r,"<=",span,|a:f64,b:f64| a<=b,|a:i64,b:i64| a<=b,|a:&str,b:&str| a<=b,|a:bool,b:bool| a==b||(!a&&b)),
        Ge => cmp_order(l,r,">=",span,|a:f64,b:f64| a>=b,|a:i64,b:i64| a>=b,|a:&str,b:&str| a>=b,|a:bool,b:bool| a==b||(a&&!b)),
        And | Or => unreachable!("short-circuit ops must not reach BinOp dynamic dispatch"),
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
        _ => Err(JadeError::TypeError { op: op.to_string(), span }),
    }
}

fn eval_unaryop_dynamic(op: &UnaryOpKind, v: VmValue, span: Span) -> Result<VmValue> {
    match op {
        UnaryOpKind::BitNot => match v { VmValue::Int(i) => Ok(VmValue::Int(!i)), _ => Err(JadeError::TypeError{op:"~".to_string(),span}) },
        UnaryOpKind::Not    => match v { VmValue::Bool(b)=> Ok(VmValue::Bool(!b)),_ => Err(JadeError::TypeError{op:"!".to_string(),span}) },
        UnaryOpKind::Neg    => match v {
            VmValue::Int(i)   => Ok(VmValue::Int(-i)),
            VmValue::Float(f) => Ok(VmValue::Float(-f)),
            _ => Err(JadeError::TypeError{op:"-".to_string(),span})
        },
    }
}

fn to_floats(l: VmValue, r: VmValue, op: &BinOpKind, span: Span) -> Result<(f64, f64)> {
    let lf = match l { VmValue::Int(i) => i as f64, VmValue::Float(f) => f, _ => return Err(JadeError::TypeError { op: format!("{:?}", op), span }) };
    let rf = match r { VmValue::Int(i) => i as f64, VmValue::Float(f) => f, _ => return Err(JadeError::TypeError { op: format!("{:?}", op), span }) };
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
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
        },
        "!=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a!=b,
            (VmValue::Float(a),VmValue::Float(b)) => a!=b,
            (VmValue::Bool(a),VmValue::Bool(b))   => a!=b,
            (VmValue::Str(a),VmValue::Str(b))     => a!=b,
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
        },
        "<"  => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a<b,
            (VmValue::Float(a),VmValue::Float(b)) => a<b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)<b,
            (VmValue::Float(a),VmValue::Int(b))   => a<(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => !a&&b,
            (VmValue::Str(a),VmValue::Str(b))     => a<b,
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
        },
        ">"  => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a>b,
            (VmValue::Float(a),VmValue::Float(b)) => a>b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)>b,
            (VmValue::Float(a),VmValue::Int(b))   => a>(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a&&!b,
            (VmValue::Str(a),VmValue::Str(b))     => a>b,
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
        },
        "<=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a<=b,
            (VmValue::Float(a),VmValue::Float(b)) => a<=b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)<=b,
            (VmValue::Float(a),VmValue::Int(b))   => a<=(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a==b||(!a&&b),
            (VmValue::Str(a),VmValue::Str(b))     => a<=b,
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
        },
        ">=" => match (lv,rv) {
            (VmValue::Int(a),VmValue::Int(b))     => a>=b,
            (VmValue::Float(a),VmValue::Float(b)) => a>=b,
            (VmValue::Int(a),VmValue::Float(b))   => (a as f64)>=b,
            (VmValue::Float(a),VmValue::Int(b))   => a>=(b as f64),
            (VmValue::Bool(a),VmValue::Bool(b))   => a==b||(a&&!b),
            (VmValue::Str(a),VmValue::Str(b))     => a>=b,
            _ => return Err(JadeError::TypeError{op:op.to_string(),span}),
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
        (VmValue::Dict(_), _) => Err(JadeError::TypeError { op: "dict index".to_string(), span }),
        _ => Err(JadeError::TypeError { op: "[]".to_string(), span }),
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
        _ => Err(JadeError::TypeError { op: "expected int".to_string(), span }),
    }
}

fn get_flt(slots: &[VmValue], r: Reg, span: Span) -> Result<f64> {
    match get(slots, r) {
        VmValue::Float(f) => Ok(*f),
        _ => Err(JadeError::TypeError { op: "expected float".to_string(), span }),
    }
}

fn get_bool(slots: &[VmValue], r: Reg, span: Span) -> Result<bool> {
    match get(slots, r) {
        VmValue::Bool(b) => Ok(*b),
        _ => Err(JadeError::TypeError { op: "expected bool".to_string(), span }),
    }
}

fn get_str(slots: &[VmValue], r: Reg, span: Span) -> Result<String> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.clone()),
        _ => Err(JadeError::TypeError { op: "expected str".to_string(), span }),
    }
}

/// Borrow a string slot by reference.  Use this instead of `get_str` when the
/// caller only needs to read the string (e.g. for comparisons) and does not
/// need an owned `String`.  Avoids a heap allocation per comparison.
fn get_str_ref<'a>(slots: &'a [VmValue], r: Reg, span: Span) -> Result<&'a str> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.as_str()),
        _ => Err(JadeError::TypeError { op: "expected str".to_string(), span }),
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
fn instr_max_reg(instr: &Instr) -> u32 {
    match instr {
        Instr::LoadInt(d,_)|Instr::LoadFloat(d,_)|Instr::LoadBool(d,_)
        |Instr::LoadStr(d,_)|Instr::LoadNil(d)|Instr::LoadFn(d,_)
        |Instr::MakeClosure(d,_) => *d,
        Instr::GetLocal(d,_)|Instr::GetGlobal(d,_) => *d,
        Instr::Move(d,s)|Instr::NegInt(d,s)|Instr::NegFloat(d,s)
        |Instr::IntToFloat(d,s)|Instr::BitNot(d,s)|Instr::Not(d,s)
        |Instr::MakePrompt(d,s)
        |Instr::UnaryOp(d,_,s)|Instr::PromptDeref(d,s,_) => (*d).max(*s),
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
        Instr::Jump(_)|Instr::Halt|Instr::Return(None)|Instr::ImportFile(_) => 0,
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
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler::{emit, type_infer},
        frontend::{lexer, parser},
    };

    fn run_src(src: &str) -> Result<VmState> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        let tprogram = type_infer::infer(program).expect("type inference failed");
        let compiled = emit::emit(tprogram).expect("emit failed");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run(compiled, VmOpts::default()))
    }

    fn get_int(state: &VmState, name: &str) -> i64 {
        match state.globals.get(name).expect("var not found") {
            VmValue::Int(i) => *i,
            v => panic!("expected Int, got {:?}", v),
        }
    }

    fn get_float(state: &VmState, name: &str) -> f64 {
        match state.globals.get(name).expect("var not found") {
            VmValue::Float(f) => *f,
            v => panic!("expected Float, got {:?}", v),
        }
    }

    fn get_bool(state: &VmState, name: &str) -> bool {
        match state.globals.get(name).expect("var not found") {
            VmValue::Bool(b) => *b,
            v => panic!("expected Bool, got {:?}", v),
        }
    }

    fn get_str<'a>(state: &'a VmState, name: &str) -> &'a str {
        match state.globals.get(name).expect("var not found") {
            VmValue::Str(s) => s,
            v => panic!("expected Str, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_int_literal() {
        let s = run_src("let x = 42").unwrap();
        assert_eq!(get_int(&s, "x"), 42);
    }

    #[test]
    fn test_vm_float_literal() {
        let s = run_src("let x = 3.14").unwrap();
        assert!((get_float(&s, "x") - 3.14).abs() < 1e-10);
    }

    #[test]
    fn test_vm_bool_literal() {
        let s = run_src("let x = true").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_str_literal() {
        let s = run_src("let x = \"hello\"").unwrap();
        assert_eq!(get_str(&s, "x"), "hello");
    }

    #[test]
    fn test_vm_add_int() {
        let s = run_src("let x = 3 + 4").unwrap();
        assert_eq!(get_int(&s, "x"), 7);
    }

    #[test]
    fn test_vm_add_float() {
        let s = run_src("let x = 1.5 + 2.5").unwrap();
        assert!((get_float(&s, "x") - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_vm_add_int_float() {
        let s = run_src("let x = 1 + 2.5").unwrap();
        assert!((get_float(&s, "x") - 3.5).abs() < 1e-10);
    }

    #[test]
    fn test_vm_sub_mul_div() {
        let s = run_src("let x = 10 - 3\nlet y = 4 * 5\nlet z = 10 / 2").unwrap();
        assert_eq!(get_int(&s, "x"), 7);
        assert_eq!(get_int(&s, "y"), 20);
        assert_eq!(get_int(&s, "z"), 5);
    }

    #[test]
    fn test_vm_mod() {
        let s = run_src("let x = 10 % 3").unwrap();
        assert_eq!(get_int(&s, "x"), 1);
    }

    #[test]
    fn test_vm_comparison() {
        let s = run_src("let a = 3 < 5\nlet b = 5 > 3\nlet c = 3 == 3\nlet d = 3 != 4").unwrap();
        assert!(get_bool(&s, "a"));
        assert!(get_bool(&s, "b"));
        assert!(get_bool(&s, "c"));
        assert!(get_bool(&s, "d"));
    }

    #[test]
    fn test_vm_logical_and_or() {
        let s = run_src("let a = true && false\nlet b = false || true").unwrap();
        assert!(!get_bool(&s, "a"));
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_short_circuit_and() {
        let s = run_src("let a = false && true").unwrap();
        assert!(!get_bool(&s, "a"));
    }

    #[test]
    fn test_vm_short_circuit_or() {
        let s = run_src("let a = true || false").unwrap();
        assert!(get_bool(&s, "a"));
    }

    #[test]
    fn test_vm_if_true() {
        let s = run_src("let x = 0\nif true {\n  x = 1\n}").unwrap();
        assert_eq!(get_int(&s, "x"), 1);
    }

    #[test]
    fn test_vm_if_false() {
        let s = run_src("let x = 0\nif false {\n  x = 1\n}").unwrap();
        assert_eq!(get_int(&s, "x"), 0);
    }

    #[test]
    fn test_vm_if_else() {
        let s = run_src("let x = 0\nif false {\n  x = 1\n} else {\n  x = 2\n}").unwrap();
        assert_eq!(get_int(&s, "x"), 2);
    }

    #[test]
    fn test_vm_while_loop() {
        let s = run_src("let i = 0\nlet sum = 0\nwhile i < 5 {\n  sum = sum + i\n  i = i + 1\n}").unwrap();
        assert_eq!(get_int(&s, "sum"), 10);
    }

    #[test]
    fn test_vm_function_call() {
        let s = run_src("fn add(a, b) {\n  return a + b\n}\nlet x = add(3, 4)").unwrap();
        assert_eq!(get_int(&s, "x"), 7);
    }

    #[test]
    fn test_vm_recursive_fn() {
        let s = run_src("fn fact(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * fact(n - 1)\n}\nlet x = fact(5)").unwrap();
        assert_eq!(get_int(&s, "x"), 120);
    }

    #[test]
    fn test_vm_array_literal() {
        let s = run_src("let a = [1, 2, 3]\nlet x = a[1]").unwrap();
        assert_eq!(get_int(&s, "x"), 2);
    }

    #[test]
    fn test_vm_array_assign() {
        let s = run_src("let a = [1, 2, 3]\na[0] = 10\nlet x = a[0]").unwrap();
        assert_eq!(get_int(&s, "x"), 10);
    }

    #[test]
    fn test_vm_str_concat() {
        let s = run_src("let a = \"hello\"\nlet b = \" world\"\nlet c = a + b").unwrap();
        assert_eq!(get_str(&s, "c"), "hello world");
    }

    #[test]
    fn test_vm_fstr() {
        let s = run_src("let name = \"jade\"\nlet x = f\"hello, {name}!\"").unwrap();
        assert_eq!(get_str(&s, "x"), "hello, jade!");
    }

    #[test]
    fn test_vm_bitwise() {
        let s = run_src("let a = 5 & 3\nlet b = 5 | 3\nlet c = 5 ^ 3").unwrap();
        assert_eq!(get_int(&s, "a"), 1);
        assert_eq!(get_int(&s, "b"), 7);
        assert_eq!(get_int(&s, "c"), 6);
    }

    #[test]
    fn test_vm_unary_neg() {
        let s = run_src("let x = -5\nlet y = -3.14").unwrap();
        assert_eq!(get_int(&s, "x"), -5);
        assert!((get_float(&s, "y") - (-3.14)).abs() < 1e-10);
    }

    #[test]
    fn test_vm_struct() {
        let s = run_src("struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 10, y: 20 }\nlet px = p.x").unwrap();
        assert_eq!(get_int(&s, "px"), 10);
    }

    #[test]
    fn test_vm_struct_field_assign() {
        let s = run_src("struct Point {\n  x,\n  y,\n}\nlet p = Point { x: 1, y: 2 }\np.x = 99\nlet px = p.x").unwrap();
        assert_eq!(get_int(&s, "px"), 99);
    }

    #[test]
    fn test_vm_dict() {
        let s = run_src("let d = {\"key\": 42}\nlet x = d[\"key\"]").unwrap();
        assert_eq!(get_int(&s, "x"), 42);
    }

    #[test]
    fn test_vm_len_array() {
        let s = run_src("let a = [1, 2, 3]\nlet n = len(a)").unwrap();
        assert_eq!(get_int(&s, "n"), 3);
    }

    #[test]
    fn test_vm_len_str() {
        let s = run_src("let s = \"hello\"\nlet n = len(s)").unwrap();
        assert_eq!(get_int(&s, "n"), 5);
    }

    #[test]
    fn test_vm_extend_method() {
        let src = "struct Counter {\n  val,\n}\nextend Counter {\n  fn inc(self) {\n    self.val = self.val + 1\n  }\n}\nlet c = Counter { val: 0 }\nc.inc()\nlet v = c.val";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "v"), 1);
    }

    #[test]
    fn test_vm_prompt_decl() {
        let s = run_src("prompt p = \"hello\"").unwrap();
        match s.globals.get("p").unwrap() {
            VmValue::Prompt(t) => assert_eq!(t, "hello"),
            v => panic!("expected Prompt, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_div_by_zero() {
        let res = run_src("let x = 1 / 0");
        assert!(res.is_err());
    }

    // ── Implicit return tests ─────────────────────────────────────────────────

    /// A function whose body is a single bare expression returns that value.
    #[test]
    fn test_vm_implicit_return_bare_expr() {
        let s = run_src("fn answer() {\n  42\n}\nlet x = answer()").unwrap();
        assert_eq!(get_int(&s, "x"), 42);
    }

    /// A function with let bindings followed by a bare expression returns the
    /// expression value, not nil.
    #[test]
    fn test_vm_implicit_return_after_let() {
        let s = run_src("fn double(n) {\n  let result = n * 2\n  result\n}\nlet x = double(5)").unwrap();
        assert_eq!(get_int(&s, "x"), 10);
    }

    /// A function ending with an explicit `return` still works correctly; the
    /// emitter must not append a second `Return(None)` instruction after it.
    #[test]
    fn test_vm_explicit_return_no_dead_instruction() {
        let s = run_src("fn add(a, b) {\n  return a + b\n}\nlet x = add(3, 4)").unwrap();
        assert_eq!(get_int(&s, "x"), 7);
    }

    /// A function with an empty body falls off the end and returns nil.
    #[test]
    fn test_vm_empty_body_returns_nil() {
        let s = run_src("fn noop() {}\nlet x = noop()").unwrap();
        match s.globals.get("x").unwrap() {
            VmValue::Nil => {}
            v => panic!("expected Nil, got {:?}", v),
        }
    }

    // ── helpers for ported eval.rs tests ─────────────────────────────────────

    /// Like `run_src` but propagates errors from every stage (lex, parse,
    /// type_infer, emit, vm) so error-path tests return `Err` rather than
    /// panicking at the `expect` call.
    fn try_run_src(src: &str) -> Result<VmState> {
        let tokens = lexer::tokenize(src)?;
        let program = parser::parse(tokens)?;
        let tprogram = type_infer::infer(program)?;
        let compiled = emit::emit(tprogram)?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run(compiled, VmOpts::default()))
    }

    fn run_src_with_mock(src: &str, responses: Vec<&str>) -> Result<VmState> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        let tprogram = type_infer::infer(program).expect("type inference failed");
        let compiled = emit::emit(tprogram).expect("emit failed");
        let opts = VmOpts {
            backend: Some(std::sync::Arc::new(crate::llm::MockBackend::new(responses))),
            ..VmOpts::default()
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run(compiled, opts))
    }

    // ── REPL incremental state ────────────────────────────────────────────────

    #[test]
    fn test_vm_repl_state_persists() {
        // Tests that run_incremental preserves globals across two separate runs.
        // Each snippet is compiled independently (no cross-snippet references)
        // because the type inferrer is stateless — cross-snippet variable
        // references require a stateful type inferrer (future work).
        use crate::compiler::{emit, type_infer};
        fn repl_run(src: &str, state: &mut VmState) {
            let tokens = lexer::tokenize(src).expect("lex");
            let program = parser::parse(tokens).expect("parse");
            let tprogram = type_infer::infer(program).expect("infer");
            let compiled = emit::emit(tprogram).expect("emit");
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio")
                .block_on(run_incremental(compiled, state))
                .expect("run_incremental");
        }
        let mut state = VmState::new_for_repl(VmOpts::default());
        repl_run("let x = 42", &mut state);
        repl_run("let y = 100", &mut state);
        // Both globals must be present after two independent incremental runs
        match state.globals.get("x").unwrap() {
            VmValue::Int(42) => {}
            v => panic!("expected Int(42), got {:?}", v),
        }
        match state.globals.get("y").unwrap() {
            VmValue::Int(100) => {}
            v => panic!("expected Int(100), got {:?}", v),
        }
    }

    #[test]
    fn test_vm_repl_result_capture_and_remove() {
        use crate::{
            compiler::{emit, type_infer},
            frontend::ast::Stmt,
            frontend::error::Span,
        };
        let src = "1 + 1";
        let tokens = lexer::tokenize(src).expect("lex");
        let mut program = parser::parse(tokens).expect("parse");
        // Wrap the bare expression in a let binding named __repl_result__
        if let Some(Stmt::Expr(expr)) = program.stmts.pop() {
            program.stmts.push(Stmt::Let {
                name: "__repl_result__".to_string(),
                value: expr,
                span: Span { line: 0, col: 0 },
            });
        }
        let tprogram = type_infer::infer(program).expect("infer");
        let compiled = emit::emit(tprogram).expect("emit");
        let mut state = VmState::new_for_repl(VmOpts::default());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio")
            .block_on(run_incremental(compiled, &mut state))
            .expect("run_incremental");
        // read it
        assert!(matches!(state.globals.get("__repl_result__"), Some(VmValue::Int(2))));
        // remove it
        state.globals.remove("__repl_result__");
        assert!(state.globals.get("__repl_result__").is_none());
    }

    // ── arithmetic (ported from eval.rs) ─────────────────────────────────────

    #[test]
    fn test_vm_div_float() {
        let s = run_src("let x = 5.0 / 2.0").unwrap();
        assert!((get_float(&s, "x") - 2.5).abs() < 1e-10);
    }

    #[test]
    fn test_vm_mod_float() {
        let s = run_src("let x = 5.0 % 2.0").unwrap();
        assert!((get_float(&s, "x") - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_vm_mul_promotes_to_float() {
        let s = run_src("let x = 2 * 1.5").unwrap();
        assert!((get_float(&s, "x") - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_vm_shl() {
        let s = run_src("let x = 1 << 3").unwrap();
        assert_eq!(get_int(&s, "x"), 8);
    }

    #[test]
    fn test_vm_shr() {
        let s = run_src("let x = 16 >> 2").unwrap();
        assert_eq!(get_int(&s, "x"), 4);
    }

    #[test]
    fn test_vm_bitnot_zero() {
        let s = run_src("let x = ~0").unwrap();
        assert_eq!(get_int(&s, "x"), -1);
    }

    #[test]
    fn test_vm_neg_paren_ok() {
        let s = run_src("let x = -(3 + 4)").unwrap();
        assert_eq!(get_int(&s, "x"), -7);
    }

    // ── error conditions (ported from eval.rs) ────────────────────────────────

    #[test]
    fn test_vm_div_by_zero_float() {
        assert!(try_run_src("let x = 5.0 / 0.0").is_err());
    }

    #[test]
    fn test_vm_remainder_by_zero_int() {
        let err = try_run_src("let x = 5 % 0").err().expect("expected error");
        assert!(matches!(err, JadeError::RemainderByZero { .. }));
    }

    #[test]
    fn test_vm_remainder_by_zero_float() {
        assert!(try_run_src("let x = 5.0 % 0.0").is_err());
    }

    #[test]
    fn test_vm_invalid_shift_too_large() {
        let err = try_run_src("let x = 1 << 64").err().expect("expected error");
        assert!(matches!(err, JadeError::InvalidShift { amount: 64, .. }));
    }

    #[test]
    fn test_vm_invalid_shift_negative() {
        let err = try_run_src("let x = 1 >> -1").err().expect("expected error");
        assert!(matches!(err, JadeError::InvalidShift { amount: -1, .. }));
    }

    #[test]
    fn test_vm_type_error_bitand_float() {
        assert!(try_run_src("let x = 1.0 & 2.0").is_err());
    }

    #[test]
    fn test_vm_type_error_bitnot_float() {
        assert!(try_run_src("let x = ~1.0").is_err());
    }

    #[test]
    fn test_vm_type_error_neg_bool() {
        assert!(try_run_src("let x = -true").is_err());
    }

    #[test]
    fn test_vm_type_error_add_bool() {
        assert!(try_run_src("let x = true + 1").is_err());
    }

    #[test]
    fn test_vm_undefined_variable() {
        assert!(try_run_src("let x = y").is_err());
    }

    #[test]
    fn test_vm_variable_chain() {
        let s = run_src("let add = 1 + 1\nlet result = add * 2").unwrap();
        assert_eq!(get_int(&s, "add"), 2);
        assert_eq!(get_int(&s, "result"), 4);
    }

    // ── boolean / logical ops (ported from eval.rs) ───────────────────────────

    #[test]
    fn test_vm_not_true() {
        let s = run_src("let x = !true").unwrap();
        assert!(!get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_not_false() {
        let s = run_src("let x = !false").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_double_not() {
        let s = run_src("let x = !!true").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_type_error_and_on_int() {
        assert!(try_run_src("let x = 1 && 0").is_err());
    }

    #[test]
    fn test_vm_type_error_not_on_int() {
        assert!(try_run_src("let x = !1").is_err());
    }

    // ── comparison (ported from eval.rs) ─────────────────────────────────────

    #[test]
    fn test_vm_bool_lt_false_true() {
        let s = run_src("let x = false < true").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_bool_gt_true_false() {
        let s = run_src("let x = true > false").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_bool_eq() {
        let s = run_src("let x = true == true").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_eq_mixed_type_error() {
        assert!(try_run_src("let x = 1 == 1.0").is_err());
    }

    #[test]
    fn test_vm_type_error_lt_bool_int() {
        assert!(try_run_src("let x = true < 1").is_err());
    }

    #[test]
    fn test_vm_compare_chain() {
        let s = run_src("let x = 1 < 2 && 3 > 0").unwrap();
        assert!(get_bool(&s, "x"));
    }

    #[test]
    fn test_vm_float_lt_promotes() {
        let s = run_src("let x = 1 < 2.5").unwrap();
        assert!(get_bool(&s, "x"));
    }

    // ── functions — scope & first-class (ported from eval.rs) ────────────────

    #[test]
    fn test_vm_fn_square() {
        let s = run_src("fn square(x) {\n  return x * x\n}\nlet sq = square(5)").unwrap();
        assert_eq!(get_int(&s, "sq"), 25);
    }

    #[test]
    fn test_vm_fn_multiply_three() {
        let s = run_src("fn multiply(a, b, c) {\n  return a * b * c\n}\nlet r = multiply(2, 3, 4)").unwrap();
        assert_eq!(get_int(&s, "r"), 24);
    }

    #[test]
    fn test_vm_fn_chained_calls() {
        let src = "fn add(a, b) {\n  return a + b\n}\nfn square(x) {\n  return x * x\n}\nlet r = add(square(2), square(3))";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "r"), 13);
    }

    #[test]
    fn test_vm_fn_local_let() {
        let s = run_src("fn get_local() {\n  let x = 42\n  return x\n}\nlet a = get_local()").unwrap();
        assert_eq!(get_int(&s, "a"), 42);
    }

    #[test]
    fn test_vm_fn_uses_param() {
        let s = run_src("fn uses_param(x) {\n  return x + 1\n}\nlet b = uses_param(9)").unwrap();
        assert_eq!(get_int(&s, "b"), 10);
    }

    #[test]
    fn test_vm_fn_local_shadow() {
        let s = run_src("fn local_shadow(x) {\n  let y = x * 2\n  return y\n}\nlet c = local_shadow(5)").unwrap();
        assert_eq!(get_int(&s, "c"), 10);
    }

    #[test]
    fn test_vm_fn_assign_to_let() {
        let src = "fn double(x) {\n  return x * 2\n}\nlet f = double\nlet a = f(5)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "a"), 10);
    }

    #[test]
    fn test_vm_fn_pass_as_arg() {
        let src = "fn double(x) {\n  return x * 2\n}\nfn apply(f, x) {\n  return f(x)\n}\nlet b = apply(double, 6)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "b"), 12);
    }

    #[test]
    fn test_vm_fn_compose() {
        let src = "fn double(x) {\n  return x * 2\n}\nfn compose(f, g, x) {\n  return f(g(x))\n}\nlet d = compose(double, double, 3)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "d"), 12);
    }

    // ── factorial / fibonacci / sum (ported from eval.rs) ────────────────────

    #[test]
    fn test_vm_fn_factorial_0() {
        let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f0 = factorial(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "f0"), 1);
    }

    #[test]
    fn test_vm_fn_factorial_1() {
        let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f1 = factorial(1)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "f1"), 1);
    }

    #[test]
    fn test_vm_fn_factorial_7() {
        let src = "fn factorial(n) {\n  if n <= 1 {\n    return 1\n  }\n  return n * factorial(n - 1)\n}\nlet f7 = factorial(7)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "f7"), 5040);
    }

    #[test]
    fn test_vm_fn_fib_0() {
        let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib0 = fib(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "fib0"), 0);
    }

    #[test]
    fn test_vm_fn_fib_1() {
        let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib1 = fib(1)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "fib1"), 1);
    }

    #[test]
    fn test_vm_fn_fib_10() {
        let src = "fn fib(n) {\n  if n <= 1 {\n    return n\n  }\n  return fib(n - 1) + fib(n - 2)\n}\nlet fib10 = fib(10)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "fib10"), 55);
    }

    #[test]
    fn test_vm_fn_sum_to_0() {
        let src = "fn sum_to(n) {\n  if n <= 0 {\n    return 0\n  }\n  return n + sum_to(n - 1)\n}\nlet s0 = sum_to(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "s0"), 0);
    }

    #[test]
    fn test_vm_fn_sum_to_10() {
        let src = "fn sum_to(n) {\n  if n <= 0 {\n    return 0\n  }\n  return n + sum_to(n - 1)\n}\nlet s10 = sum_to(10)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "s10"), 55);
    }

    // ── if / elif (ported from eval.rs) ──────────────────────────────────────

    #[test]
    fn test_vm_if_max() {
        let src = "fn max(a, b) {\n  if a > b {\n    return a\n  } else {\n    return b\n  }\n}\nlet m1 = max(3, 7)\nlet m2 = max(10, 2)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "m1"), 7);
        assert_eq!(get_int(&s, "m2"), 10);
    }

    #[test]
    fn test_vm_if_is_positive() {
        let src = "fn is_positive(x) {\n  if x > 0 {\n    return true\n  } else {\n    return false\n  }\n}\nlet pos = is_positive(5)\nlet neg = is_positive(-3)";
        let s = run_src(src).unwrap();
        assert!(get_bool(&s, "pos"));
        assert!(!get_bool(&s, "neg"));
    }

    #[test]
    fn test_vm_if_clamp() {
        let src = "fn clamp(x, lo, hi) {\n  if x < lo {\n    return lo\n  }\n  if x > hi {\n    return hi\n  }\n  return x\n}\nlet lo = clamp(1, 5, 10)\nlet mid = clamp(7, 5, 10)\nlet hi = clamp(15, 5, 10)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "lo"), 5);
        assert_eq!(get_int(&s, "mid"), 7);
        assert_eq!(get_int(&s, "hi"), 10);
    }

    #[test]
    fn test_vm_nested_if_sign() {
        let src = "fn sign(x) {\n  if x > 0 {\n    return 1\n  } else {\n    if x < 0 {\n      return -1\n    } else {\n      return 0\n    }\n  }\n}\nlet s1 = sign(10)\nlet s2 = sign(-5)\nlet s3 = sign(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "s1"), 1);
        assert_eq!(get_int(&s, "s2"), -1);
        assert_eq!(get_int(&s, "s3"), 0);
    }

    #[test]
    fn test_vm_nested_if_quadrant() {
        let src = "fn quadrant(a, b) {\n  if a > 0 {\n    if b > 0 {\n      return 1\n    } else {\n      return 4\n    }\n  } else {\n    if b > 0 {\n      return 2\n    } else {\n      return 3\n    }\n  }\n}\nlet q1 = quadrant(1, 1)\nlet q2 = quadrant(-1, 1)\nlet q3 = quadrant(-1, -1)\nlet q4 = quadrant(1, -1)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "q1"), 1);
        assert_eq!(get_int(&s, "q2"), 2);
        assert_eq!(get_int(&s, "q3"), 3);
        assert_eq!(get_int(&s, "q4"), 4);
    }

    #[test]
    fn test_vm_elif_classify() {
        let src = "fn classify(x) {\n  if x > 0 {\n    return 1\n  } elif x < 0 {\n    return -1\n  } else {\n    return 0\n  }\n}\nlet pos = classify(5)\nlet neg = classify(-3)\nlet zero = classify(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "pos"), 1);
        assert_eq!(get_int(&s, "neg"), -1);
        assert_eq!(get_int(&s, "zero"), 0);
    }

    #[test]
    fn test_vm_elif_chain() {
        let src = "fn grade(sc) {\n  if sc >= 90 {\n    return 4\n  } elif sc >= 80 {\n    return 3\n  } elif sc >= 70 {\n    return 2\n  } elif sc >= 60 {\n    return 1\n  } else {\n    return 0\n  }\n}\nlet a = grade(95)\nlet b = grade(85)\nlet c = grade(75)\nlet d = grade(65)\nlet f = grade(50)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "a"), 4);
        assert_eq!(get_int(&s, "b"), 3);
        assert_eq!(get_int(&s, "c"), 2);
        assert_eq!(get_int(&s, "d"), 1);
        assert_eq!(get_int(&s, "f"), 0);
    }

    #[test]
    fn test_vm_elif_no_else() {
        let src = "fn check(x) {\n  if x == 1 {\n    return 10\n  } elif x == 2 {\n    return 20\n  }\n  return 0\n}\nlet r1 = check(1)\nlet r2 = check(2)\nlet r3 = check(3)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "r1"), 10);
        assert_eq!(get_int(&s, "r2"), 20);
        assert_eq!(get_int(&s, "r3"), 0);
    }

    #[test]
    fn test_vm_nested_calls_pipeline() {
        let src = "fn add(a, b) {\n  return a + b\n}\nfn double(x) {\n  return x * 2\n}\nfn square(x) {\n  return x * x\n}\nfn pipeline(a, b) {\n  return double(square(add(a, b)))\n}\nlet pipe = pipeline(1, 2)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "pipe"), 18);
    }

    // ── function error cases (ported from eval.rs) ────────────────────────────

    #[test]
    fn test_vm_arity_mismatch() {
        let err = try_run_src("fn f(a) {\n  return a\n}\nlet x = f(1, 2)").err().expect("expected error");
        assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
    }

    #[test]
    fn test_vm_not_callable() {
        let err = try_run_src("let x = 5\nlet y = x(1)").err().expect("expected error");
        assert!(matches!(err, JadeError::NotCallable { .. }));
    }

    // ── integer overflow (ported from eval.rs) ────────────────────────────────

    #[test]
    fn test_vm_integer_overflow_add() {
        let err = try_run_src(&format!("let x = {} + 1", i64::MAX)).err().expect("expected error");
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_vm_integer_overflow_sub() {
        let err = try_run_src(&format!("let x = -{} - 2", i64::MAX)).err().expect("expected error");
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_vm_integer_overflow_mul() {
        let err = try_run_src(&format!("let x = {} * 2", i64::MAX)).err().expect("expected error");
        assert!(matches!(err, JadeError::IntegerOverflow { .. }));
    }

    #[test]
    fn test_vm_nested_fn_ok() {
        let s = run_src("fn outer() {\n  fn inner() {\n    return 1\n  }\n  return 2\n}").unwrap();
        assert!(s.globals.contains_key("outer"));
    }

    // ── while loops (ported from eval.rs) ────────────────────────────────────

    #[test]
    fn test_vm_while_condition_false_from_start() {
        let s = run_src("let never = 99\nwhile never < 0 {\n  never = never + 1\n}").unwrap();
        assert_eq!(get_int(&s, "never"), 99);
    }

    #[test]
    fn test_vm_while_accumulate_sum() {
        let s = run_src("let sum = 0\nlet i = 1\nwhile i <= 10 {\n  sum = sum + i\n  i = i + 1\n}").unwrap();
        assert_eq!(get_int(&s, "sum"), 55);
    }

    #[test]
    fn test_vm_while_boolean_flag() {
        let s = run_src("let flag = true\nlet steps = 0\nwhile flag {\n  steps = steps + 1\n  if steps == 3 {\n    flag = false\n  }\n}").unwrap();
        assert_eq!(get_int(&s, "steps"), 3);
        assert!(!get_bool(&s, "flag"));
    }

    #[test]
    fn test_vm_while_in_fn_factorial() {
        let src = "fn factorial(n) {\n  let result = 1\n  let i = 1\n  while i <= n {\n    result = result * i\n    i = i + 1\n  }\n  return result\n}\nlet f5 = factorial(5)\nlet f0 = factorial(0)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "f5"), 120);
        assert_eq!(get_int(&s, "f0"), 1);
    }

    #[test]
    fn test_vm_while_return_propagates() {
        let src = "fn first_above(threshold) {\n  let n = 1\n  while n * n <= threshold {\n    n = n + 1\n  }\n  return n\n}\nlet r = first_above(9)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "r"), 4);
    }

    #[test]
    fn test_vm_while_nested() {
        let src = "let total = 0\nlet i = 0\nwhile i < 3 {\n  let j = 0\n  while j < 3 {\n    total = total + 1\n    j = j + 1\n  }\n  i = i + 1\n}";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "total"), 9);
    }

    #[test]
    fn test_vm_while_type_error_condition() {
        assert!(try_run_src("while 1 {\n}").is_err());
    }

    // ── struct error cases (ported from eval.rs) ──────────────────────────────

    #[test]
    fn test_vm_undefined_type_error() {
        let err = try_run_src("let p = Foo { x: 1 }").err().expect("expected error");
        assert!(matches!(err, JadeError::UndefinedType { .. }));
    }

    #[test]
    fn test_vm_missing_field_error() {
        let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1 }").err().expect("expected error");
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn test_vm_extra_field_error() {
        let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, z: 3 }").err().expect("expected error");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    #[test]
    fn test_vm_field_access_on_non_struct_error() {
        let err = try_run_src("let x = 5\nlet v = x.y").err().expect("expected error");
        assert!(matches!(err, JadeError::NotAStruct { .. } | JadeError::TypeMismatch { .. } | JadeError::UndefinedField { .. }));
    }

    #[test]
    fn test_vm_undefined_field_access_error() {
        let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2 }\nlet v = p.z").err().expect("expected error");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    // ── strings (ported from eval.rs) ─────────────────────────────────────────

    #[test]
    fn test_vm_str_eq_true() {
        let s = run_src(r#"let b = "abc" == "abc""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_eq_false() {
        let s = run_src(r#"let b = "abc" == "xyz""#).unwrap();
        assert!(!get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_ne() {
        let s = run_src(r#"let b = "abc" != "xyz""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_lt() {
        let s = run_src(r#"let b = "abc" < "abd""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_gt() {
        let s = run_src(r#"let b = "b" > "a""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_le_equal() {
        let s = run_src(r#"let b = "abc" <= "abc""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_ge() {
        let s = run_src(r#"let b = "z" >= "a""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_str_index() {
        let s = run_src("let sv = \"hello\"\nlet h = sv[0]").unwrap();
        assert_eq!(get_str(&s, "h"), "h");
    }

    #[test]
    fn test_vm_str_index_last() {
        let s = run_src("let sv = \"hello\"\nlet o = sv[4]").unwrap();
        assert_eq!(get_str(&s, "o"), "o");
    }

    #[test]
    fn test_vm_str_index_out_of_bounds() {
        let err = try_run_src("let sv = \"hi\"\nlet x = sv[10]").err().expect("expected error");
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: 10, len: 2, .. }));
    }

    #[test]
    fn test_vm_str_index_negative() {
        let err = try_run_src("let sv = \"hi\"\nlet x = sv[-1]").err().expect("expected error");
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn test_vm_str_add_int_type_error() {
        assert!(try_run_src(r#"let x = "hello" + 1"#).is_err());
    }

    #[test]
    fn test_vm_str_sub_type_error() {
        assert!(try_run_src(r#"let x = "a" - "b""#).is_err());
    }

    #[test]
    fn test_vm_str_escape_tab() {
        let s = run_src(r#"let sv = "a\tb""#).unwrap();
        assert_eq!(get_str(&s, "sv"), "a\tb");
    }

    #[test]
    fn test_vm_str_escape_newline() {
        let s = run_src(r#"let sv = "a\nb""#).unwrap();
        assert_eq!(get_str(&s, "sv"), "a\nb");
    }

    #[test]
    fn test_vm_str_escape_quote() {
        let s = run_src(r#"let sv = "say \"hi\"""#).unwrap();
        assert_eq!(get_str(&s, "sv"), r#"say "hi""#);
    }

    #[test]
    fn test_vm_print_builtin() {
        let s = run_src("let r = 0\nprint(\"hello\")").unwrap();
        assert_eq!(get_int(&s, "r"), 0);
    }

    #[test]
    fn test_vm_print_arity_error() {
        let err = try_run_src(r#"print("a", "b")"#).err().expect("expected error");
        assert!(matches!(err, JadeError::ArityMismatch { expected: 1, got: 2, .. }));
    }

    #[test]
    fn test_vm_triple_quote_simple() {
        let s = run_src(r#"let sv = """hello""""#).unwrap();
        assert_eq!(get_str(&s, "sv"), "hello");
    }

    #[test]
    fn test_vm_triple_quote_with_inner_quotes() {
        let s = run_src(r#"let sv = """he said "hi" to her""""#).unwrap();
        assert_eq!(get_str(&s, "sv"), r#"he said "hi" to her"#);
    }

    #[test]
    fn test_vm_triple_quote_concat() {
        let s = run_src(r#"let sv = """foo""" + """bar""""#).unwrap();
        assert_eq!(get_str(&s, "sv"), "foobar");
    }

    #[test]
    fn test_vm_triple_quote_equals_regular() {
        let s = run_src(r#"let b = """abc""" == "abc""#).unwrap();
        assert!(get_bool(&s, "b"));
    }

    #[test]
    fn test_vm_fstr_literal_only() {
        let s = run_src(r#"let sv = f"hello""#).unwrap();
        assert_eq!(get_str(&s, "sv"), "hello");
    }

    #[test]
    fn test_vm_fstr_str_var() {
        let s = run_src("let name = \"Joe\"\nlet g = f\"hi {name}\"").unwrap();
        assert_eq!(get_str(&s, "g"), "hi Joe");
    }

    #[test]
    fn test_vm_fstr_bool_var() {
        let s = run_src("let b = true\nlet sv = f\"b={b}\"").unwrap();
        assert_eq!(get_str(&s, "sv"), "b=true");
    }

    #[test]
    fn test_vm_fstr_multiple_slots() {
        let s = run_src("let x = 1\nlet y = 2\nlet sv = f\"({x}, {y})\"").unwrap();
        assert_eq!(get_str(&s, "sv"), "(1, 2)");
    }

    #[test]
    fn test_vm_fstr_field_access() {
        let s = run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 3, y: 4 }\nlet sv = f\"({p.x}, {p.y})\"").unwrap();
        assert_eq!(get_str(&s, "sv"), "(3, 4)");
    }

    #[test]
    fn test_vm_fstr_triple_quote() {
        let s = run_src("let name = \"Joe\"\nlet sv = f\"\"\"hi {name}\"\"\"").unwrap();
        assert_eq!(get_str(&s, "sv"), "hi Joe");
    }

    #[test]
    fn test_vm_fstr_no_slots_equals_plain_str() {
        let s = run_src("let a = f\"hello\"\nlet b = \"hello\"").unwrap();
        assert_eq!(get_str(&s, "a"), "hello");
        assert_eq!(get_str(&s, "b"), "hello");
    }

    // ── pipe operator (ported from eval.rs) ───────────────────────────────────

    #[test]
    fn test_vm_pipe_simple() {
        let src = "fn double(x) {\n  return x * 2\n}\nlet n = 5 |> double";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "n"), 10);
    }

    #[test]
    fn test_vm_pipe_chained() {
        let src = "fn double(x) {\n  return x * 2\n}\nlet m = 3 |> double |> double";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "m"), 12);
    }

    #[test]
    fn test_vm_pipe_with_extra_arg() {
        let src = "fn add(a, b) {\n  return a + b\n}\nlet r = 5 |> add(3)";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "r"), 8);
    }

    #[test]
    fn test_vm_pipe_with_string() {
        let src = "fn greet(name) {\n  return f\"hello, {name}!\"\n}\nlet g = \"Jade\" |> greet";
        let s = run_src(src).unwrap();
        assert_eq!(get_str(&s, "g"), "hello, Jade!");
    }

    #[test]
    fn test_vm_pipe_arithmetic_lhs() {
        let src = "fn double(x) {\n  return x * 2\n}\nlet x = (2 + 3) |> double";
        let s = run_src(src).unwrap();
        assert_eq!(get_int(&s, "x"), 10);
    }

    // ── arrays (ported from eval.rs) ──────────────────────────────────────────

    #[test]
    fn test_vm_array_empty() {
        let s = run_src("let a = []").unwrap();
        match s.globals.get("a").unwrap() {
            VmValue::Array(v) => assert!(v.lock().is_empty()),
            v => panic!("expected Array, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_array_int_elements() {
        let s = run_src("let a = [10, 20, 30]").unwrap();
        match s.globals.get("a").unwrap() {
            VmValue::Array(v) => {
                let guard = v.lock();
                assert!(matches!(guard[0], VmValue::Int(10)));
                assert!(matches!(guard[1], VmValue::Int(20)));
                assert!(matches!(guard[2], VmValue::Int(30)));
            }
            v => panic!("expected Array, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_array_index_last() {
        let s = run_src("let a = [10, 20, 30]\nlet x = a[2]").unwrap();
        assert_eq!(get_int(&s, "x"), 30);
    }

    #[test]
    fn test_vm_array_index_out_of_bounds() {
        let err = try_run_src("let a = [1]\nlet x = a[1]").err().expect("expected error");
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: 1, len: 1, .. }));
    }

    #[test]
    fn test_vm_array_index_negative() {
        let err = try_run_src("let a = [1]\nlet x = a[-1]").err().expect("expected error");
        assert!(matches!(err, JadeError::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn test_vm_array_reference_semantics() {
        // Arrays are Arc-wrapped: assigning creates an alias, not a copy.
        let s = run_src("let a = [1, 2]\nlet b = a\nb[0] = 42\nlet x = a[0]").unwrap();
        assert_eq!(get_int(&s, "x"), 42);
    }

    #[test]
    fn test_vm_array_nested() {
        let s = run_src("let m = [[1, 2], [3, 4]]\nlet x = m[0][1]").unwrap();
        assert_eq!(get_int(&s, "x"), 2);
    }

    #[test]
    fn test_vm_array_trailing_comma() {
        let s = run_src("let a = [1, 2, 3,]").unwrap();
        match s.globals.get("a").unwrap() {
            VmValue::Array(v) => assert_eq!(v.lock().len(), 3),
            v => panic!("expected Array, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_len_empty_array() {
        let s = run_src("let n = len([])").unwrap();
        assert_eq!(get_int(&s, "n"), 0);
    }

    #[test]
    fn test_vm_len_type_error() {
        assert!(try_run_src("let n = len(42)").is_err());
    }

    // ── interfaces (ported from eval.rs) ──────────────────────────────────────

    #[test]
    fn test_vm_interface_basic() {
        let src = concat!(
            "interface Displayable {\n",
            "    fn to_str(self) -> str\n",
            "}\n",
            "struct Point {\n  x,\n  y\n}\n",
            "extend Point: Displayable {\n",
            "    fn to_str(self) -> str {\n",
            "        return \"point\"\n",
            "    }\n",
            "}\n",
            "let p = Point { x: 1, y: 2 }\n",
            "let sv = p.to_str()\n",
        );
        let s = run_src(src).unwrap();
        assert_eq!(get_str(&s, "sv"), "point");
    }

    #[test]
    fn test_vm_interface_missing_method_error() {
        let src = concat!(
            "interface Displayable {\n",
            "    fn to_str(self) -> str\n",
            "}\n",
            "struct Point {\n  x,\n  y\n}\n",
            "extend Point: Displayable {\n",
            "    fn area(self) {\n",
            "        return 0\n",
            "    }\n",
            "}\n",
        );
        let err = try_run_src(src).err().expect("expected error");
        assert!(matches!(err, JadeError::MissingInterfaceMethod { .. }));
    }

    #[test]
    fn test_vm_interface_undefined_error() {
        let src = concat!(
            "struct Point {\n  x,\n  y\n}\n",
            "extend Point: Displayable {\n",
            "    fn to_str(self) -> str {\n",
            "        return \"point\"\n",
            "    }\n",
            "}\n",
        );
        let err = try_run_src(src).err().expect("expected error");
        assert!(matches!(err, JadeError::UndefinedInterface { .. }));
    }

    // ── LLM / prompt (ported from eval.rs) ────────────────────────────────────

    #[test]
    fn test_vm_prompt_deref_no_backend_returns_error() {
        let err = try_run_src("prompt p = \"hi\"\nlet x = ?p").err().expect("expected error");
        assert!(matches!(err, JadeError::MissingApiKey { .. }));
    }

    #[test]
    fn test_vm_prompt_deref_not_a_prompt_returns_error() {
        // The type checker catches `?x` where x: int before the VM runs;
        // the error is TypeMismatch (not the treewalk's runtime NotAPrompt).
        assert!(try_run_src("let x = 5\nlet y = ?x").is_err());
    }

    #[test]
    fn test_vm_prompt_deref_field_access_no_backend() {
        let err = try_run_src(
            "struct Agent {\n  prompt system = \"helpful\"\n}\nlet a = Agent {}\nlet r = ?a.system"
        ).err().expect("expected error");
        assert!(matches!(err, JadeError::MissingApiKey { .. }));
    }

    #[test]
    fn test_vm_prompt_deref_field_access_not_a_prompt() {
        let err = run_src_with_mock(
            "struct S {\n  x,\n}\nlet s = S { x: 42 }\nlet r = ?s.x",
            vec![]
        ).err().expect("expected error");
        assert!(matches!(err, JadeError::NotAPrompt { .. }));
    }

    #[test]
    fn test_vm_prompt_deref_field_access_with_mock() {
        let s = run_src_with_mock(
            "struct Agent {\n  prompt system = \"Say hello\"\n}\nlet a = Agent {}\nlet r = ?a.system",
            vec!["hello!"]
        ).unwrap();
        assert_eq!(get_str(&s, "r"), "hello!");
    }

    #[test]
    fn test_vm_typed_deref_int_success() {
        let s = run_src_with_mock("prompt p = \"What is 2+2?\"\nlet n = ?p |> int", vec!["4"]).unwrap();
        assert_eq!(get_int(&s, "n"), 4);
    }

    #[test]
    fn test_vm_typed_deref_float_success() {
        let s = run_src_with_mock("prompt p = \"pi\"\nlet n = ?p |> float", vec!["3.14"]).unwrap();
        assert!((get_float(&s, "n") - 3.14).abs() < 0.001);
    }

    #[test]
    fn test_vm_typed_deref_bool_success() {
        let s = run_src_with_mock("prompt p = \"true?\"\nlet n = ?p |> bool", vec!["true"]).unwrap();
        assert!(get_bool(&s, "n"));
    }

    #[test]
    fn test_vm_typed_deref_str_success() {
        let s = run_src_with_mock("prompt p = \"hello\"\nlet n = ?p |> str", vec!["world"]).unwrap();
        assert_eq!(get_str(&s, "n"), "world");
    }

    #[test]
    fn test_vm_typed_deref_overflow() {
        let err = run_src_with_mock(
            "prompt p = \"bad\"\nlet n = ?p |> int",
            vec!["oops", "still wrong", "nope", "nah"],
        ).err().expect("expected error");
        assert!(matches!(err, JadeError::PromptOverflow { .. }));
    }

    #[test]
    fn test_vm_tokens_incremented_after_deref() {
        let s = run_src_with_mock("prompt p = \"hi\"\nlet x = ?p", vec!["hello"]).unwrap();
        match s.globals.get("__tokens__").unwrap() {
            VmValue::Int(n) => assert!(*n > 0),
            v => panic!("expected Int, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_untyped_deref_returns_str() {
        let s = run_src_with_mock("prompt p = \"test\"\nlet x = ?p", vec!["result"]).unwrap();
        assert_eq!(get_str(&s, "x"), "result");
    }

    #[test]
    fn test_vm_typed_deref_retry_succeeds_on_second_attempt() {
        let s = run_src_with_mock(
            "prompt p = \"number?\"\nlet n = ?p |> int",
            vec!["not a number", "42"],
        ).unwrap();
        assert_eq!(get_int(&s, "n"), 42);
    }

    // ── dicts (ported from eval.rs) ───────────────────────────────────────────

    #[test]
    fn test_vm_dict_empty() {
        let s = run_src("let d = {}").unwrap();
        match s.globals.get("d").unwrap() {
            VmValue::Dict(m) => assert!(m.is_empty()),
            v => panic!("expected Dict, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_dict_string_values() {
        let s = run_src(r#"let d = {"name": "jade", "lang": "cool"}"#).unwrap();
        match s.globals.get("d").unwrap() {
            VmValue::Dict(m) => {
                assert!(matches!(m.get("name"), Some(VmValue::Str(s)) if s == "jade"));
                assert!(matches!(m.get("lang"), Some(VmValue::Str(s)) if s == "cool"));
            }
            v => panic!("expected Dict, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_dict_index_read_string_value() {
        let s = run_src("let d = {\"a\": \"hello\"}\nlet v = d[\"a\"]").unwrap();
        assert_eq!(get_str(&s, "v"), "hello");
    }

    #[test]
    fn test_vm_dict_key_not_found() {
        let err = try_run_src("let d = {\"x\": 1}\nlet v = d[\"y\"]").err().expect("expected error");
        assert!(matches!(err, JadeError::KeyNotFound { key, .. } if key == "y"));
    }

    #[test]
    fn test_vm_dict_index_assign_existing_key() {
        let s = run_src("let d = {\"v\": 1}\nd[\"v\"] = 99").unwrap();
        match s.globals.get("d").unwrap() {
            VmValue::Dict(m) => assert!(matches!(m.get("v"), Some(VmValue::Int(99)))),
            v => panic!("expected Dict, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_dict_index_assign_new_key() {
        let s = run_src("let d = {}\nd[\"k\"] = 5").unwrap();
        match s.globals.get("d").unwrap() {
            VmValue::Dict(m) => assert!(matches!(m.get("k"), Some(VmValue::Int(5)))),
            v => panic!("expected Dict, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_dict_len() {
        let s = run_src("let d = {\"a\": 1, \"b\": 2, \"c\": 3}\nlet n = len(d)").unwrap();
        assert_eq!(get_int(&s, "n"), 3);
    }

    #[test]
    fn test_vm_dict_len_empty() {
        let s = run_src("let d = {}\nlet n = len(d)").unwrap();
        assert_eq!(get_int(&s, "n"), 0);
    }

    #[test]
    fn test_vm_dict_value_semantics() {
        let src = "let d = {\"x\": 1}\nlet d2 = d\nd2[\"x\"] = 99";
        let s = run_src(src).unwrap();
        match s.globals.get("d").unwrap() {
            VmValue::Dict(m) => assert!(matches!(m.get("x"), Some(VmValue::Int(1)))),
            v => panic!("expected Dict, got {:?}", v),
        }
        match s.globals.get("d2").unwrap() {
            VmValue::Dict(m) => assert!(matches!(m.get("x"), Some(VmValue::Int(99)))),
            v => panic!("expected Dict, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_dict_variable_key() {
        let src = "let k = \"name\"\nlet d = {k: \"jade\"}\nlet v = d[\"name\"]";
        let s = run_src(src).unwrap();
        assert_eq!(get_str(&s, "v"), "jade");
    }

    #[test]
    fn test_vm_dict_non_string_index_type_error() {
        assert!(try_run_src("let d = {\"x\": 1}\nlet v = d[0]").is_err());
    }

    // ── struct field defaults (ported from eval.rs) ───────────────────────────

    #[test]
    fn test_vm_struct_default_omitted() {
        let s = run_src("struct Config {\n  let host = \"localhost\"\n}\nlet c = Config {}\nlet h = c.host").unwrap();
        assert_eq!(get_str(&s, "h"), "localhost");
    }

    #[test]
    fn test_vm_struct_default_overridden() {
        let s = run_src("struct Config {\n  let host = \"localhost\"\n}\nlet c = Config { host: \"example.com\" }\nlet h = c.host").unwrap();
        assert_eq!(get_str(&s, "h"), "example.com");
    }

    #[test]
    fn test_vm_struct_all_defaults_empty_literal() {
        let s = run_src("struct Config {\n  let host = \"localhost\"\n  let port = 8080\n}\nlet c = Config {}\nlet h = c.host\nlet p = c.port").unwrap();
        assert_eq!(get_str(&s, "h"), "localhost");
        assert_eq!(get_int(&s, "p"), 8080);
    }

    #[test]
    fn test_vm_struct_required_still_required() {
        let err = try_run_src("struct Mixed {\n  x,\n  let label = \"origin\"\n}\nlet m = Mixed {}").err().expect("expected error");
        assert!(matches!(err, JadeError::MissingField { .. }));
    }

    #[test]
    fn test_vm_struct_mixed_fields() {
        let s = run_src("struct Mixed {\n  x,\n  y,\n  let label = \"origin\"\n}\nlet m = Mixed { x: 1, y: 2 }\nlet lbl = m.label").unwrap();
        assert_eq!(get_str(&s, "lbl"), "origin");
    }

    #[test]
    fn test_vm_struct_prompt_field_default() {
        let s = run_src("struct Agent {\n  prompt system = \"You are helpful\"\n}\nlet a = Agent {}\nlet sv = a.system").unwrap();
        match s.globals.get("sv").unwrap() {
            VmValue::Prompt(t) => assert_eq!(t, "You are helpful"),
            v => panic!("expected Prompt, got {:?}", v),
        }
    }

    #[test]
    fn test_vm_struct_prompt_field_override() {
        let s = run_src("struct Agent {\n  prompt system = \"You are helpful\"\n}\nlet a = Agent { system: \"Custom\" }\nlet sv = a.system").unwrap();
        match s.globals.get("sv").unwrap() {
            VmValue::Prompt(t) => assert_eq!(t, "Custom"),
            v => panic!("expected Prompt, got {:?}", v),
        }
    }

    #[test]
    #[ignore = "VM does not yet validate that prompt struct fields must be strings (treewalk did)"]
    fn test_vm_struct_prompt_field_non_string_error() {
        assert!(try_run_src("struct Bad {\n  prompt sys = 42\n}\nlet b = Bad {}").is_err());
    }

    #[test]
    #[ignore = "VM does not yet validate that prompt struct field overrides must be strings"]
    fn test_vm_struct_prompt_field_override_non_string_error() {
        assert!(try_run_src("struct Agent {\n  prompt system = \"ok\"\n}\nlet a = Agent { system: 99 }").is_err());
    }

    #[test]
    fn test_vm_struct_extra_field_still_errors_with_defaults() {
        let err = try_run_src("struct Agent {\n  let name = \"Jade\"\n}\nlet a = Agent { name: \"x\", extra: 1 }").err().expect("expected error");
        assert!(matches!(err, JadeError::UndefinedField { .. }));
    }

    #[test]
    fn test_vm_struct_duplicate_field_error() {
        let err = try_run_src("struct Point {\n  x,\n  y\n}\nlet p = Point { x: 1, y: 2, x: 3 }").err().expect("expected error");
        assert!(matches!(err, JadeError::DuplicateField { field, .. } if field == "x"));
    }

    #[test]
    fn test_vm_struct_default_references_variable() {
        let s = run_src("let base = 10\nstruct S {\n  let x = base\n}\nlet sv = S {}\nlet v = sv.x").unwrap();
        assert_eq!(get_int(&s, "v"), 10);
    }

    #[test]
    fn test_vm_struct_required_after_let_field() {
        let err = try_run_src("struct S {\n  let x = 0,\n  y\n}\nlet s = S { x: 1 }").err().expect("expected error");
        assert!(matches!(err, JadeError::MissingField { field, .. } if field == "y"));
    }

    // ── std/fs tests ──────────────────────────────────────────────────────────

    #[test]
    fn test_fs_write_and_read() {
        let dir = std::env::temp_dir();
        let path = dir.join("jade_test_fs_write_read.txt");
        let path_str = path.to_str().unwrap();
        let src = format!(
            "use \"std/fs\"\nfs.write(\"{path_str}\", \"hello jade\")\nlet v = fs.read(\"{path_str}\")"
        );
        let s = run_src(&src).unwrap();
        assert_eq!(get_str(&s, "v"), "hello jade");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_fs_exists_true() {
        let dir = std::env::temp_dir();
        let path = dir.join("jade_test_fs_exists_true.txt");
        std::fs::write(&path, "x").unwrap();
        let path_str = path.to_str().unwrap();
        let src = format!("use \"std/fs\"\nlet v = fs.exists(\"{path_str}\")");
        let s = run_src(&src).unwrap();
        assert!(get_bool(&s, "v"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_fs_exists_false() {
        let src = "use \"std/fs\"\nlet v = fs.exists(\"/tmp/jade_test_no_such_file_xyz.txt\")";
        let s = run_src(src).unwrap();
        assert!(!get_bool(&s, "v"));
    }

    #[test]
    fn test_fs_delete() {
        let dir = std::env::temp_dir();
        let path = dir.join("jade_test_fs_delete.txt");
        std::fs::write(&path, "bye").unwrap();
        let path_str = path.to_str().unwrap();
        let src = format!("use \"std/fs\"\nfs.delete(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
        let s = run_src(&src).unwrap();
        assert!(!get_bool(&s, "v"));
    }

    #[test]
    fn test_fs_append() {
        let dir = std::env::temp_dir();
        let path = dir.join("jade_test_fs_append.txt");
        let path_str = path.to_str().unwrap();
        let src = format!(
            "use \"std/fs\"\nfs.write(\"{path_str}\", \"hello\")\nfs.append(\"{path_str}\", \" world\")\nlet v = fs.read(\"{path_str}\")"
        );
        let s = run_src(&src).unwrap();
        assert_eq!(get_str(&s, "v"), "hello world");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_fs_list_dir() {
        let dir = std::env::temp_dir();
        let subdir = dir.join("jade_test_fs_list_dir");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("a.txt"), "").unwrap();
        std::fs::write(subdir.join("b.txt"), "").unwrap();
        let path_str = subdir.to_str().unwrap();
        let src = format!("use \"std/fs\"\nlet v = fs.list_dir(\"{path_str}\")");
        let s = run_src(&src).unwrap();
        match s.globals.get("v").unwrap() {
            VmValue::Array(a) => {
                let names: Vec<String> = a.lock().iter().map(|v| match v {
                    VmValue::Str(s) => s.clone(),
                    _ => panic!("non-str entry"),
                }).collect();
                assert!(names.contains(&"a.txt".to_string()));
                assert!(names.contains(&"b.txt".to_string()));
            }
            _ => panic!("expected array"),
        }
        let _ = std::fs::remove_dir_all(&subdir);
    }

    #[test]
    fn test_fs_mkdir() {
        let dir = std::env::temp_dir();
        let newdir = dir.join("jade_test_fs_mkdir_new/nested");
        let path_str = newdir.to_str().unwrap();
        let _ = std::fs::remove_dir_all(dir.join("jade_test_fs_mkdir_new"));
        let src = format!("use \"std/fs\"\nfs.mkdir(\"{path_str}\")\nlet v = fs.exists(\"{path_str}\")");
        let s = run_src(&src).unwrap();
        assert!(get_bool(&s, "v"));
        let _ = std::fs::remove_dir_all(dir.join("jade_test_fs_mkdir_new"));
    }

    #[test]
    fn test_fs_read_nonexistent_errors() {
        let err = try_run_src("use \"std/fs\"\nlet v = fs.read(\"/tmp/jade_no_such_file_xyz.txt\")").err().expect("expected error");
        assert!(matches!(err, JadeError::IoError { .. }));
    }

    #[test]
    fn test_fs_write_arity_error() {
        let err = try_run_src("use \"std/fs\"\nfs.write(\"path\")").err().expect("expected error");
        assert!(matches!(err, JadeError::ArityMismatch { expected: 2, .. }));
    }
}
