use std::{cell::RefCell, collections::{HashMap, HashSet}, path::PathBuf, rc::Rc};

use crate::{
    compiler::{
        bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg},
        emit::CompiledProgram,
    },
    interpreter::{
        ast::{BinOpKind, StructFieldDef, UnaryOpKind},
        error::{JadeError, Result, Span},
    },
    llm,
};

// ── Token budgets (mirror eval.rs) ────────────────────────────────────────────

const DEFAULT_MAX_TOKENS: u32 = 1024;
const RETRY_MAX_TOKENS: u32 = 64;

// ── Runtime value ─────────────────────────────────────────────────────────────

/// A value at VM runtime.
///
/// Mirrors `eval::Value` but carries `Rc<CompiledFn>` for functions so the VM
/// can execute them without re-running the emitter.
#[derive(Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Fn(Rc<CompiledFn>),
    /// A closure: compiled function + snapshot of globals at creation time.
    Closure(Rc<CompiledFn>, Rc<HashMap<String, VmValue>>),
    Struct(Rc<RefCell<VmStruct>>),
    BoundMethod(Rc<VmBoundMethod>),
    Array(Vec<VmValue>),
    Prompt(String),
    Dict(HashMap<String, VmValue>),
    Nil,
}

pub struct VmStruct {
    pub type_name: String,
    pub fields: HashMap<String, VmValue>,
}

pub struct VmBoundMethod {
    pub receiver: Rc<RefCell<VmStruct>>,
    pub method: Rc<CompiledFn>,
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
                let inst = rc.borrow();
                write!(f, "{} {{...}}", inst.type_name)
            }
            VmValue::BoundMethod(_) => write!(f, "<bound method>"),
            VmValue::Array(v) => write!(f, "Array[{} elem(s)]", v.len()),
            VmValue::Prompt(s) => write!(f, "Prompt({:?})", s),
            VmValue::Dict(m)  => write!(f, "Dict({} key(s))", m.len()),
            VmValue::Nil      => write!(f, "Nil"),
        }
    }
}

// ── Public display helper ─────────────────────────────────────────────────────

/// Convert a `VmValue` to its user-visible string representation.
/// Mirrors `eval::value_to_str`.
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
        VmValue::Array(v) => {
            let parts: Vec<String> = v.iter().map(value_to_display).collect();
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
        VmValue::Fn(_)          => "<fn>".to_string(),
        VmValue::Closure(_, _)  => "<fn>".to_string(),
        VmValue::Struct(_)      => "<struct>".to_string(),
        VmValue::BoundMethod(_) => "<bound method>".to_string(),
        VmValue::Prompt(_)      => "<prompt>".to_string(),
        VmValue::Nil            => "nil".to_string(),
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
    pub extend_methods: HashMap<String, HashMap<String, Rc<CompiledFn>>>,
    /// Struct field definitions (needed for struct instantiation validation).
    pub struct_defs: HashMap<String, Vec<StructFieldDef>>,
    /// Optional LLM inference backend.
    pub inference_backend: Option<Box<dyn llm::InferenceBackend>>,
    pub conversation_history: Vec<llm::Message>,
    pub token_count: i64,
    pub max_retries: usize,
    pub default_model: String,
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
        globals.insert("__retry_log__".to_string(), VmValue::Array(vec![]));
        VmState {
            raised_exception: None,
            globals,
            extend_methods: HashMap::new(),
            struct_defs: HashMap::new(),
            inference_backend: None,
            conversation_history: Vec::new(),
            token_count: 0,
            max_retries: 3,
            default_model: String::new(),
            source_dir: PathBuf::new(),
            import_stack: HashSet::new(),
        }
    }

    fn set_session(&mut self, name: &str, value: VmValue) {
        self.globals.insert(name.to_string(), value);
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
        state.inference_backend = opts.backend;
        state.max_retries = opts.max_retries;
        state.default_model = opts.default_model.clone();
        state.source_dir = opts.source_dir;
        state.set_session("__model__", VmValue::Str(opts.default_model));
        state.set_session("__max_retries__", VmValue::Int(opts.max_retries as i64));
        state
    }
}

/// Options for an `vm::run` invocation.
pub struct VmOpts {
    pub backend: Option<Box<dyn llm::InferenceBackend>>,
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
pub fn run(program: CompiledProgram, opts: VmOpts) -> Result<VmState> {
    let mut state = VmState::new();
    state.inference_backend = opts.backend;
    state.max_retries = opts.max_retries;
    state.default_model = opts.default_model.clone();
    state.source_dir = opts.source_dir;
    state.set_session("__model__", VmValue::Str(opts.default_model));
    state.set_session("__max_retries__", VmValue::Int(opts.max_retries as i64));
    run_with_state(program, &mut state)?;
    Ok(state)
}

/// Execute a compiled program against an existing `VmState`.
///
/// This is the public entry point for the REPL — it lets each snippet share
/// globals, struct definitions, and extend-block methods with prior snippets.
pub fn run_incremental(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    run_with_state(program, state)
}

/// Execute a compiled program against an existing `VmState`.
/// Used internally for imports so they share globals/struct_defs/extend_methods.
fn run_with_state(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    // Merge compile-time metadata into the shared state.
    for (k, v) in program.struct_defs {
        state.struct_defs.insert(k, v);
    }
    for (type_name, methods) in program.extend_methods {
        state.extend_methods.entry(type_name).or_default().extend(methods);
    }

    let mut slots: Vec<VmValue> = vec![VmValue::Nil; program.top_n_slots as usize];
    execute_chunk(&program.top, &mut slots, state)?;
    Ok(())
}

// ── Execution engine ──────────────────────────────────────────────────────────

/// Build a `RuntimeError { message }` struct value for wrapping built-in errors
/// when they are caught by a `try/catch` block.
fn make_vm_runtime_error(message: String) -> VmValue {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), VmValue::Str(message));
    VmValue::Struct(Rc::new(RefCell::new(VmStruct {
        type_name: "RuntimeError".to_string(),
        fields,
    })))
}

/// Execute `chunk` with the provided register frame.  Returns `Some(value)` if
/// a `Return` instruction was executed, `None` if execution ended normally.
fn execute_chunk(
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

                let result = (|| -> Result<()> {
                    let source = std::fs::read_to_string(&canon).map_err(|_| {
                        JadeError::ImportNotFound { path: path.clone(), span }
                    })?;

                    let canon_str = canon.to_string_lossy().into_owned();
                    let hash = crate::cache::file_hash(&canon);

                    let cached_ast = hash.as_ref().and_then(|h| crate::cache::read_ast_cache(h));
                    let program = match cached_ast {
                        Some(p) => p,
                        None => {
                            let tokens = crate::interpreter::lexer::tokenize(&source)?;
                            let p = crate::interpreter::parser::parse(tokens)?;
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

                    let compiled = crate::compiler::emit::emit(tprogram)?;
                    run_with_state(compiled, state)
                })();

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
                let cf = Rc::clone(&chunk.fn_defs[*idx]);
                set(slots, *d, VmValue::Fn(cf));
            }
            Instr::MakeClosure(d, idx) => {
                let cf = Rc::clone(&chunk.fn_defs[*idx]);
                // Capture a snapshot of all current globals at closure-creation time.
                let captured: HashMap<String, VmValue> = state.globals.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                set(slots, *d, VmValue::Closure(cf, Rc::new(captured)));
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
                let v = get(slots, *s).clone();
                state.globals.insert(name.clone(), v);
            }
            Instr::GetLocal(d, slot) => {
                let v = slots.get(*slot as usize)
                    .cloned()
                    .unwrap_or(VmValue::Nil);
                set(slots, *d, v);
            }
            Instr::SetLocal(slot, s) => {
                let v = get(slots, *s).clone();
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
                let result = vm_try!(call_value(callee, args, state, span));
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
                set(slots, *dest, VmValue::Array(elems));
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
                match &mut slots[*obj_reg as usize] {
                    VmValue::Array(v) => {
                        let i = match idx { VmValue::Int(n) => n, _ => { vm_err!(JadeError::TypeError { op: "array index".to_string(), span }); } };
                        let len = v.len();
                        if i < 0 || i as usize >= len { vm_err!(JadeError::IndexOutOfBounds { index: i, len, span }); }
                        v[i as usize] = val;
                    }
                    VmValue::Dict(m) => {
                        let k = match idx { VmValue::Str(s) => s, _ => { vm_err!(JadeError::TypeError { op: "dict index".to_string(), span }); } };
                        m.insert(k, val);
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
                set(slots, *dest, VmValue::Struct(Rc::new(RefCell::new(VmStruct {
                    type_name: type_name.clone(),
                    fields,
                }))));
            }
            Instr::GetField(dest, obj_reg, field) => {
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Struct(rc) => {
                        let type_name = rc.borrow().type_name.clone();
                        if let Some(v) = rc.borrow().fields.get(field.as_str()).cloned() {
                            set(slots, *dest, v);
                        } else if let Some(methods) = state.extend_methods.get(&type_name) {
                            if let Some(mfn) = methods.get(field.as_str()) {
                                set(slots, *dest, VmValue::BoundMethod(Rc::new(VmBoundMethod {
                                    receiver: rc,
                                    method: Rc::clone(mfn),
                                })));
                            } else {
                                vm_err!(JadeError::UndefinedField { type_name, field: field.clone(), span });
                            }
                        } else {
                            vm_err!(JadeError::UndefinedField { type_name, field: field.clone(), span });
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
                        {
                            let b = rc.borrow();
                            if !b.fields.contains_key(field.as_str()) {
                                vm_err!(JadeError::UndefinedField {
                                    type_name: b.type_name.clone(),
                                    field: field.clone(),
                                    span,
                                });
                            }
                        }
                        rc.borrow_mut().fields.insert(field.clone(), val);
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
                let result = vm_try!(vm_prompt_deref(text, output_type.as_deref(), state, span));
                set(slots, *dest, result);
            }

            // ── Built-ins ─────────────────────────────────────────────────────
            Instr::CallPrint(arg_regs) => {
                if arg_regs.len() != 1 {
                    vm_err!(JadeError::ArityMismatch { expected: 1, got: arg_regs.len(), span });
                }
                let v = get(slots, arg_regs[0]).clone();
                println!("{}", value_to_display(&v));
            }
            Instr::CallLen(dest, src) => {
                let result = match get(slots, *src) {
                    VmValue::Str(s)    => VmValue::Int(s.chars().count() as i64),
                    VmValue::Array(v)  => VmValue::Int(v.len() as i64),
                    VmValue::Dict(m)   => VmValue::Int(m.len() as i64),
                    _ => { vm_err!(JadeError::TypeError { op: "len".to_string(), span }); }
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
                    VmValue::Struct(rc) => rc.borrow().type_name.clone(),
                    _ => String::new(),
                };
                set(slots, *dest, VmValue::Str(name));
            }
        }
    }
    Ok(None)
}

// ── Call dispatch ─────────────────────────────────────────────────────────────

fn call_value(
    callee: VmValue,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    match callee {
        VmValue::Fn(cf) => call_fn(&cf, args, state, span),
        VmValue::Closure(cf, captured) => {
            // Temporarily inject captured variables into globals so the closure body
            // sees them via GetGlobal. Save any displaced values and restore after.
            let mut saved: Vec<(String, Option<VmValue>)> = Vec::new();
            for (k, v) in captured.iter() {
                let old = state.globals.insert(k.clone(), v.clone());
                saved.push((k.clone(), old));
            }
            let result = call_fn(&cf, args, state, span);
            for (k, old) in saved {
                match old {
                    Some(v) => { state.globals.insert(k, v); }
                    None    => { state.globals.remove(&k); }
                }
            }
            result
        }
        VmValue::BoundMethod(bm) => {
            let method = Rc::clone(&bm.method);
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(VmValue::Struct(Rc::clone(&bm.receiver)));
            full_args.extend(args);
            call_fn(&method, full_args, state, span)
        }
        _ => Err(JadeError::NotCallable { span }),
    }
}

fn call_fn(
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
        frame[i] = v;
    }
    let result = execute_chunk(&cf.chunk, &mut frame, state)?;
    Ok(result.unwrap_or(VmValue::Nil))
}

// ── Prompt deref ──────────────────────────────────────────────────────────────

fn vm_prompt_deref(
    prompt_text: String,
    output_type: Option<&str>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    let initial_resp = {
        let backend = state.inference_backend.as_ref()
            .ok_or(JadeError::MissingApiKey { span })?;
        backend.infer(llm::InferenceRequest {
            prompt: prompt_text.clone(),
            model: state.default_model.clone(),
            history: state.conversation_history.clone(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }, span)?
    };

    state.conversation_history.push(llm::Message { role: "user".to_string(),      content: prompt_text });
    state.conversation_history.push(llm::Message { role: "assistant".to_string(), content: initial_resp.text.clone() });
    state.token_count += initial_resp.tokens_used;
    let tc = state.token_count;
    state.globals.insert("__tokens__".to_string(), VmValue::Int(tc));

    let Some(type_name) = output_type else {
        return Ok(VmValue::Str(initial_resp.text));
    };

    // Typed deref: retry loop.
    let max_retries = state.max_retries;
    let hist_len_before = state.conversation_history.len();
    let mut current = initial_resp.text;

    for _attempt in 0..max_retries {
        if let Some(v) = coerce(current.trim(), type_name) {
            state.conversation_history.truncate(hist_len_before);
            return Ok(v);
        }
        let correction = format!(
            "Your response '{}' could not be parsed as {}. Please respond with only a single {} value, nothing else.",
            current.trim(), type_name, type_name
        );
        let retry = {
            let backend = state.inference_backend.as_ref()
                .ok_or(JadeError::MissingApiKey { span })?;
            backend.infer(llm::InferenceRequest {
                prompt: correction.clone(),
                model: state.default_model.clone(),
                history: state.conversation_history.clone(),
                max_tokens: RETRY_MAX_TOKENS,
            }, span)?
        };
        state.conversation_history.push(llm::Message { role: "user".to_string(),      content: correction });
        state.conversation_history.push(llm::Message { role: "assistant".to_string(), content: retry.text.clone() });
        current = retry.text;
    }

    if let Some(v) = coerce(current.trim(), type_name) {
        state.conversation_history.truncate(hist_len_before);
        return Ok(v);
    }
    state.conversation_history.truncate(hist_len_before);
    Err(JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: max_retries + 1, span })
}

fn coerce(text: &str, type_name: &str) -> Option<VmValue> {
    match type_name {
        "int"   => text.parse::<i64>().ok().map(VmValue::Int),
        "float" => text.parse::<f64>().ok().map(VmValue::Float),
        "str"   => Some(VmValue::Str(text.to_string())),
        "bool"  => match text.to_lowercase().as_str() {
            "true"  => Some(VmValue::Bool(true)),
            "false" => Some(VmValue::Bool(false)),
            _       => None,
        },
        _ => None,
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
        (VmValue::Array(v), VmValue::Int(i)) => {
            let len = v.len();
            if i < 0 || i as usize >= len {
                Err(JadeError::IndexOutOfBounds { index: i, len, span })
            } else {
                Ok(v[i as usize].clone())
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

fn int2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(i64, i64)> {
    Ok((get_int(slots, l, span)?, get_int(slots, r, span)?))
}

fn flt2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(f64, f64)> {
    Ok((get_flt(slots, l, span)?, get_flt(slots, r, span)?))
}

fn bool2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(bool, bool)> {
    Ok((get_bool(slots, l, span)?, get_bool(slots, r, span)?))
}

fn str2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(String, String)> {
    Ok((get_str(slots, l, span)?, get_str(slots, r, span)?))
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
        |Instr::MakePrompt(d,s)|Instr::CallLen(d,s)
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
        Instr::CallPrint(args) => args.iter().copied().max().unwrap_or(0),
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
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compiler::{emit, type_infer},
        interpreter::{lexer, parser},
    };

    fn run_src(src: &str) -> Result<VmState> {
        let tokens = lexer::tokenize(src).expect("lex failed");
        let program = parser::parse(tokens).expect("parse failed");
        let tprogram = type_infer::infer(program).expect("type inference failed");
        let compiled = emit::emit(tprogram).expect("emit failed");
        run(compiled, VmOpts::default())
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
}
