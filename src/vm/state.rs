//! The VM's execution state ([`VmState`]) and per-run options ([`VmOpts`]).
//!
//! `VmState` is the whole world the interpreter mutates: globals, struct/method
//! tables, the import cycle guard, the inference backend, and the REPL capture
//! slot. `VmOpts` is the caller-supplied configuration `run` applies onto it.

use super::*;

/// Internal sentinel the REPL assigns a bare trailing expression to so it can
/// echo the value. Not a valid Jade identifier (leading NUL), so it can never
/// collide with a user global, and it is routed into `VmState.repl_capture`
/// rather than the global namespace. Replaces the old `__repl_result__` global.
pub(crate) const REPL_CAPTURE: &str = "\u{0}repl";

pub struct VmState {
    /// Buffers of the generators currently running, innermost last.
    ///
    /// A stack rather than a single slot because a generator can call another
    /// generator: each `yield` must land in its own function's buffer, not in
    /// whichever one happened to start first.
    pub yield_stack: Vec<std::sync::Arc<parking_lot::Mutex<Vec<VmValue>>>>,
    /// The value most recently raised by `Instr::Raise` that propagated past its
    /// frame's handler stack. Consumed by the nearest enclosing `SetupHandler`.
    pub raised_exception: Option<VmValue>,
    /// All top-level (global) bindings after execution. Uses `FxHashMap` — the
    /// interpreter hashes a variable name on every `GetGlobal`, and these are
    /// short internal keys, so the default SipHash's cost isn't worth its DoS
    /// resistance here.
    pub globals: rustc_hash::FxHashMap<String, VmValue>,
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
    /// REPL only: the value of the last bare trailing expression, captured
    /// out-of-band (see [`REPL_CAPTURE`]) so the REPL can echo it without a
    /// global. `None` outside the REPL and after each snippet is read.
    pub repl_capture: Option<VmValue>,
    /// Memoisation cache: maps `(prompt_text, output_type)` → the raw response
    /// text that produced a successful result. Mirrors the same cache in `Env`.
    pub prompt_cache: HashMap<(String, Option<String>), String>,
    /// Directory of the currently-executing file — used to resolve relative `use` paths.
    pub source_dir: PathBuf,
    /// Set of canonical paths currently being imported (cycle detection).
    pub import_stack: HashSet<PathBuf>,
    /// Project root (the dir holding the `jade.toml` with `[project]`). Anchor
    /// for resolving registered `[lib]` library imports.
    pub project_root: Option<PathBuf>,
    /// Registered libraries from `jade.toml` `[lib]`: name → {path, files}.
    pub libraries: HashMap<String, crate::project::LibraryEntry>,
    /// The module scope of the currently-executing module function, if any.
    /// `GetGlobal` checks here before `globals`; `SetGlobal` writes here when
    /// the name already exists in scope, preserving mutations across calls.
    pub active_module_scope: Option<Arc<Mutex<HashMap<String, VmValue>>>>,
    /// Test-only stdout capture. When set, `vm_drain_token_stream_printing` writes
    /// here instead of stdout so tests can assert on the printed output.
    #[cfg(test)]
    pub test_stdout: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
}

impl VmState {
    pub(crate) fn new() -> Self {
        let mut globals = rustc_hash::FxHashMap::default();
        builtins::seed_globals(&mut globals);
        VmState {
            yield_stack: Vec::new(),
            raised_exception: None,
            globals,
            extend_methods: HashMap::new(),
            struct_defs: HashMap::new(),
            struct_decorators: HashMap::new(),
            route_configs: HashMap::new(),
            inference_backend: None,
            repl_capture: None,
            prompt_cache: HashMap::new(),
            source_dir: PathBuf::new(),
            import_stack: HashSet::new(),
            project_root: None,
            libraries: HashMap::new(),
            active_module_scope: None,
            #[cfg(test)]
            test_stdout: None,
        }
    }

    /// Apply `VmOpts` fields onto this state.
    ///
    /// Extracted to avoid duplicating the same block in `run` and `new_for_repl`.
    pub(crate) fn apply_opts(&mut self, opts: VmOpts) {
        self.inference_backend = opts.backend;
        self.source_dir = opts.source_dir;
        self.project_root = opts.project_root;
        self.libraries = opts.libraries;
        #[cfg(test)]
        {
            self.test_stdout = opts.test_stdout;
        }
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
    /// Mutations inside the spawned task do NOT propagate back to the parent.
    pub fn new_for_spawn(&self) -> Self {
        VmState {
            yield_stack: Vec::new(),
            raised_exception: None,
            globals: self.globals.clone(),
            extend_methods: self.extend_methods.clone(),
            struct_defs: self.struct_defs.clone(),
            struct_decorators: self.struct_decorators.clone(),
            route_configs: self.route_configs.clone(),
            inference_backend: self.inference_backend.clone(),
            repl_capture: None,
            prompt_cache: self.prompt_cache.clone(),
            source_dir: self.source_dir.clone(),
            import_stack: HashSet::new(),
            project_root: self.project_root.clone(),
            libraries: self.libraries.clone(),
            active_module_scope: None,
            #[cfg(test)]
            test_stdout: self.test_stdout.clone(),
        }
    }
}

/// Options for an `vm::run` invocation.
pub struct VmOpts {
    pub backend: Option<std::sync::Arc<dyn llm::InferenceBackend>>,
    /// Directory of the source file being run — used to resolve relative `use` paths.
    /// Defaults to the current working directory when running in-memory (tests, REPL).
    pub source_dir: PathBuf,
    /// Project root — anchor for registered `[lib]` library imports.
    pub project_root: Option<PathBuf>,
    /// Registered libraries from `jade.toml` `[lib]`: name → {path, files}.
    pub libraries: HashMap<String, crate::project::LibraryEntry>,
    /// Test-only stdout capture buffer. See `VmState::test_stdout`.
    #[cfg(test)]
    pub test_stdout: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
}

impl Default for VmOpts {
    fn default() -> Self {
        VmOpts {
            backend: None,
            source_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            project_root: None,
            libraries: HashMap::new(),
            #[cfg(test)]
            test_stdout: None,
        }
    }
}
