//! Bytecode `Chunk` → LLVM IR.
//!
//! The whole translation, and nothing about producing a file: `cfg` turns the
//! flat instruction stream into basic blocks, and the rest lowers one opcode at
//! a time. `aot/` is the caller — it resolves imports, wraps what comes out of
//! here in a `main()`, and drives the linker.
//!
//! ## Model: tagged register slots
//!
//! `emit.rs` produces a register machine. We give every register an `alloca i64`
//! holding a **tagged value word** (the same tagged ABI the runtime uses, see
//! `jade-runtime`/`runtime.h`). Instructions load their operand slots, untag to a
//! native value, compute, re-tag, and store the result slot. This is simpler
//! than tracking a static type per register (the emitter reuses register slots
//! across types), and LLVM's `mem2reg` + `instcombine` promote the allocas to
//! SSA and fold the `untag(tag(x))` round-trips away, so the tag arithmetic is
//! mostly free after optimization. Boxed floats and calls hit the runtime.
//!
//! An opcode with no lowering here returns an `Err`, which is a hard build
//! error. There is no second backend to fall back to, so anything `emit.rs`
//! can produce has to be handled in this directory.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use inkwell::attributes::AttributeLoc;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{
    AnyValue, BasicMetadataValueEnum, FloatValue, FunctionValue, GlobalValue, IntValue,
    PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use crate::bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg};
use crate::frontend::ast::{BinOpKind, Expr, StructFieldDef, UnaryOpKind};
use crate::vm::VmValue;

mod abi;
mod arith;
mod builtins;
mod calls;
pub mod cfg;
mod exc;
mod instr;
mod llm;
mod rc;
mod strings;
#[cfg(test)]
mod tests;

// The submodules are peers that call freely into one another: `instr` dispatches
// to all of them, `calls` reaches `builtins`, and every one of them uses the
// `Lowerer` helpers in `abi`. Rather than have each file name every sibling it
// touches, each does `use super::*` and this block lifts their `pub(super)`
// items into the shared parent scope. Splitting the old monolith was a pure
// move, so this keeps every call site exactly as it was written.
use abi::default_word_const;
use builtins::*;
use calls::*;
use instr::lower_instr;
use llm::emit_stream_call;
use strings::emit_str_method;

// Tagged-immediate bit patterns (mirror runtime.h / jade-runtime value.rs).
const NIL: u64 = 0x07;
const TRUE: u64 = 0x1f;
const FALSE: u64 = 0x0f;
// Low-3-bit heap tags (mirror runtime.h JRT_TAG_*).
const TAG_PTR: u64 = 1; // 0b001 non-string heap object (incl. a boxed fn value)
const TAG_STR: u64 = 5; // 0b101 heap string pointer
// Trust byte for literal (compile-time-known) strings.
const TRUSTED: u64 = 0;
// ObjKind::Fn (mirror jade-runtime heap.rs). A function-value box carries this in
// the ObjKind byte (header offset 8) so the refcount ops recognise it as a
// non-collection and no-op on it, while indirect_call still reads fn_ptr at
// offset 0. See fn_box_word.
const OBJKIND_FN: u64 = 5;
// ObjKind::BoundMethod (mirror jade-runtime heap.rs). Same shape as a function
// box plus the receiver at offset 16; `indirect_call` reads the kind byte to
// decide whether to prepend it as `self`. Built by jrt_bind_method_new, never
// by codegen directly.
const OBJKIND_BOUND_METHOD: u64 = 9;

// Receiver kinds a primitive method accepts, passed to `jrt_require_kind` as a
// bitmask. Mirror the JRT_WANT_* macros in runtime_aot/runtime.h.
const WANT_STR: u64 = 0x1;
const WANT_ARRAY: u64 = 0x2;
const WANT_DICT: u64 = 0x4;

/// Per-function lowering helper: bundles the builder, the i64 slot type, and the
/// register `alloca`s so the instruction handlers stay terse.
struct Lowerer<'a, 'ctx> {
    ctx: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    slots: &'a [PointerValue<'ctx>],
    /// Parameter count of the function being lowered. Parameter slots (`0..n_params`)
    /// hold references the *caller* owns (borrowed), so scope-exit release covers
    /// only the locals (`n_params..`).
    n_params: usize,
    /// Slot holding the exception-handler depth on entry, for functions that
    /// contain a `try`. Every return restores it, so a handler frame cannot
    /// outlive the stack frame holding its `jmp_buf` — see `emit_exc_restore`.
    /// `None` when the function has no handler and its depth cannot change.
    exc_depth_slot: Option<PointerValue<'ctx>>,
    /// Slot holding the recursion-depth counter on entry — every *ordinary*
    /// function call counts against the recursion limit (not just one inside a
    /// `try`, unlike `exc_depth_slot`), so this is `Some` far more often. Every
    /// return and every exception landing pad restores it; see
    /// `emit_recur_restore`. `None` for a body lowered with `track_recursion =
    /// false` (`jade_toplevel`, `lower_chunk`'s isolated bodies) — a body that
    /// never opened a frame must not close one either.
    recur_depth_slot: Option<PointerValue<'ctx>>,
    /// Whether this function is a `yield`ing stream producer. When true a
    /// `Return` discards the body's value and hands back the generator's buffer
    /// instead — that is what makes calling a generator give you a stream.
    is_generator: bool,
    /// Scratch stack buffers a call site needs, keyed by purpose and length and
    /// allocated once in the entry block. See [`Lowerer::entry_buf`].
    entry_bufs: RefCell<HashMap<(&'static str, usize), PointerValue<'ctx>>>,
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// A `[n x i64]` scratch buffer in the function's **entry block**, reused by
    /// every call site that asks for the same `tag` and `n`.
    ///
    /// A call site that marshals its arguments into memory needs somewhere to
    /// put them, and the obvious spelling — an `alloca` right where the call is
    /// emitted — is wrong here. LLVM does not reclaim an `alloca` until the
    /// function returns, so a call inside a loop walks the stack down once per
    /// iteration until it hits the guard page. An FFI call in a `while` loop
    /// died at a fixed iteration count for exactly this reason, and the count
    /// scaled with `ulimit -s`, which is what identified it as stack exhaustion
    /// rather than a leak. The entry block runs once per frame, so a buffer
    /// allocated there costs the same whether the call runs once or ten million
    /// times. `handler_bufs` in [`lower_body`] follows the same rule for the
    /// same reason.
    ///
    /// **Sharing is sound because no two of these buffers are ever live at
    /// once.** Every site fills the buffer from register slots — plain loads,
    /// which emit no call — and hands it straight to the call that consumes it,
    /// so one site's fill can never interleave with another's. Re-entrancy is
    /// not a hazard either: an `alloca` belongs to the frame, so a recursive or
    /// concurrent call marshals into its own copy. The one pairing that must
    /// *not* share is a buffer the callee writes back into while reading
    /// another — `Join` passes its futures and its results together — which is
    /// why the `tag` is part of the key.
    ///
    /// The alloca is emitted before the entry block's terminator with a private
    /// builder, so the caller's insertion point is untouched.
    fn entry_buf(
        &self,
        tag: &'static str,
        n: usize,
    ) -> Result<PointerValue<'ctx>, String> {
        if let Some(p) = self.entry_bufs.borrow().get(&(tag, n)) {
            return Ok(*p);
        }
        let entry = self
            .builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .and_then(|f| f.get_first_basic_block())
            .ok_or("entry_buf: no entry block to allocate in")?;
        let eb = self.ctx.create_builder();
        match entry.get_terminator() {
            Some(t) => eb.position_before(&t),
            None => eb.position_at_end(entry),
        }
        let buf = eb
            .build_alloca(self.i64t().array_type(n as u32), &format!("{tag}{n}"))
            .map_err(|e| e.to_string())?;
        self.entry_bufs.borrow_mut().insert((tag, n), buf);
        Ok(buf)
    }
}

// ── User-function lowering (A6b) ───────────────────────────────────────────────
//
// The top-level chunk and every reachable `fn_def`/closure becomes its own LLVM
// function `jf_<uid>`. Function values are **first-class**: `LoadFn`/`MakeClosure`
// materialize a boxed function pointer (`fn_box_word`), so a function can be
// stored, passed as an argument, and returned. A `Call` whose callee is a
// statically-known function is **devirtualized** to a direct `call jf_<uid>`
// (filling omitted defaults); any other function value is called **indirectly**
// through its box. Closures capture only globals (the language forbids nested
// `fn`s and local capture — a closure body reads outer names as `GetGlobal`), so
// a closure is just a plain `jf_<uid>` with no environment. Only a call to a
// reserved builtin this backend doesn't lower (e.g. `len`) forces a fallback.

/// Whole-program function registry, shared across every chunk being lowered.
struct FnCtx<'ctx> {
    /// uid → the pre-declared `jf_<uid>` LLVM function (declared before any body
    /// is lowered, so recursion and mutual recursion resolve).
    funcs: Vec<FunctionValue<'ctx>>,
    /// uid → the compiled function (params/defaults/body).
    defs: Vec<Arc<CompiledFn>>,
    /// `Arc<CompiledFn>` identity → uid, for resolving a chunk-local `LoadFn`
    /// index (`chunk.fn_defs[idx]`) to its global uid.
    ptr2uid: HashMap<*const CompiledFn, usize>,
    /// Global variable name → uid, for a name that is *provably* a function: it
    /// is `SetGlobal`'d exactly once in the whole program, from a `LoadFn`. A
    /// decorated function (`@dec`) writes the global twice, so it is excluded —
    /// its identity changes at runtime, and the call must fall back.
    global_fns: HashMap<String, usize>,
    /// Struct type name → its optional fields' (name, scalar-literal default),
    /// used to fill fields a struct literal omits (the VM fills these at runtime).
    struct_defaults: HashMap<String, Vec<(String, VmValue, bool)>>,
    /// Every declared struct type name.
    ///
    /// Used to recognise a *call* on a type name, which is an error worth
    /// naming: a struct type is not a function, and without this the call falls
    /// through to `Indirect` and jumps through a global cell codegen never
    /// assigns for a type name.
    ///
    /// The field lists are carried too, rather than just the names, because
    /// `struct_defaults` cannot answer "what fields does this type have?" — it
    /// keeps only optional fields with scalar-literal defaults.
    struct_field_names: HashMap<String, Vec<String>>,
    /// Extend-block method NAME → its candidate implementations `(uid, required,
    /// total)`, where `required`/`total` are the arg counts (excluding `self`)
    /// the method accepts (`required` = params without a default, `total` = all
    /// params). A call `obj.name(k args)` resolves to the *one* candidate whose
    /// range accepts `k` (`required <= k <= total`); if exactly one matches it
    /// devirtualizes to a direct `jf_<uid>` call, so same-named methods on
    /// different types disambiguate by arity (`put(a,b)` vs `put(a)`) with no
    /// runtime type dispatch. Two candidates accepting the same `k` (same name +
    /// arity on two types) are genuinely ambiguous → decline.
    method_candidates: HashMap<String, Vec<(usize, usize, usize)>>,
    /// Global names the program `SetGlobal`s (assigns) anywhere. A name here is a
    /// user variable, so `name.method(...)` is a value method, NOT a stdlib module
    /// call — even when `name` happens to be a reserved module name (`let sh = []`
    /// shadows `use std::sh`). Guards `module.method` recognition against shadowing.
    user_globals: std::collections::HashSet<String>,
}

impl<'ctx> FnCtx<'ctx> {
    fn empty() -> Self {
        FnCtx {
            funcs: Vec::new(),
            defs: Vec::new(),
            ptr2uid: HashMap::new(),
            global_fns: HashMap::new(),
            struct_defaults: HashMap::new(),
            struct_field_names: HashMap::new(),
            method_candidates: HashMap::new(),
            user_globals: std::collections::HashSet::new(),
        }
    }

    /// uid of the function `fn_defs[idx]` refers to (by `Arc` identity).
    fn uid_of(&self, fn_defs: &[Arc<CompiledFn>], idx: usize) -> Option<usize> {
        fn_defs.get(idx).and_then(|f| self.ptr2uid.get(&Arc::as_ptr(f)).copied())
    }

    /// Resolve an extend-method call `obj.name(k args)` to a single `jf_<uid>`:
    /// the one candidate named `name` whose arg range accepts `k` (`required <= k
    /// <= total`, both excluding `self`). Returns `None` if no candidate matches
    /// (not a struct method) or more than one does (same name+arity on two types
    /// — genuinely ambiguous, needs runtime type dispatch).
    fn resolve_method(&self, name: &str, k: usize) -> Option<usize> {
        let cands = self.method_candidates.get(name)?;
        let mut hit = None;
        for &(uid, required, total) in cands {
            if required <= k && k <= total {
                if hit.is_some() {
                    return None; // ambiguous
                }
                hit = Some(uid);
            }
        }
        hit
    }
}

/// A struct field default the backend can materialize (scalar literals only;
/// mirrors the VM's `eval_literal_default` for the representable subset).
fn eval_scalar_default(e: &Expr) -> Option<VmValue> {
    match e {
        Expr::Str { value, .. } => Some(VmValue::Str(value.clone().into())),
        Expr::Integer { value, .. } => Some(VmValue::Int(*value)),
        Expr::Float { value, .. } => Some(VmValue::Float(*value)),
        Expr::Bool { value, .. } => Some(VmValue::Bool(*value)),
        Expr::Identifier { name, .. } if name == "nil" || name == "None" || name == "null" => {
            Some(VmValue::Nil)
        }
        _ => None,
    }
}

/// Struct type name → its optional fields' (name, scalar default). Non-scalar or
/// non-literal defaults (e.g. `let xs = []`) are skipped — a literal that omits
/// such a field would leave it unset, but those appear on method-bearing structs
/// (declined). Mirrors what the VM fills at `MakeStruct` time.
/// Per type, the scalar defaults for fields a literal may omit, each flagged with
/// whether it is a `prompt` field — a prompt default is boxed as a prompt value
/// rather than stored as the bare string, matching what the VM does.
fn build_struct_defaults(
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
) -> HashMap<String, Vec<(String, VmValue, bool)>> {
    let mut out = HashMap::new();
    for (tn, fields) in struct_defs {
        let mut ds = Vec::new();
        for f in fields {
            let (name, default, is_prompt) = match f {
                StructFieldDef::Let { name, default } => (name, default, false),
                StructFieldDef::Prompt { name, default } => (name, default, true),
                StructFieldDef::Required(_) => continue,
            };
            if let Some(v) = eval_scalar_default(default) {
                ds.push((name.clone(), v, is_prompt));
            }
        }
        if !ds.is_empty() {
            out.insert(tn.clone(), ds);
        }
    }
    out
}

/// A lowered program: its top-level entry plus the named functions it defines.
pub struct LoweredProgram<'ctx> {
    /// `jade_toplevel() -> i64`, which `main` (or `jade_pkg_init`) calls.
    pub toplevel: FunctionValue<'ctx>,
    /// Global name → the `jf_<uid>` it holds. `jade build --lib` needs this to
    /// export a function under the name the Jade source gave it; nothing else
    /// recovers a source-level name once lowering has mangled everything to uids.
    pub global_fns: HashMap<String, FunctionValue<'ctx>>,
}

/// Lower a whole program: every reachable `fn_def` becomes a `jf_<uid>` function
/// and the top-level chunk becomes `jade_toplevel() -> i64`. Returns the
/// top-level function (for `main` to call), or `Err` on any opcode/construct the
/// backend can't lower yet (the daemon then falls back to `expr.rs`).
pub fn lower_program<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    top: &Chunk,
    top_n_slots: u32,
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
) -> Result<LoweredProgram<'ctx>, String> {
    let (mut defs, mut ptr2uid) = collect_fns(top);

    // Extend-block methods are ordinary compiled functions (`self` is param 0),
    // but they live in the `extend_methods` side table, not in `top`'s reachable
    // tree — so append them as functions with their own uids (BFS their nested
    // fn_defs too), and record which method NAMES are unique across all types.
    // A unique name devirtualizes to a direct call; an ambiguous name (same
    // method on >1 type) needs runtime type dispatch and is left out, so calls to
    // it decline (`resolve_user_calls`).
    let method_candidates = collect_method_fns(extend_methods, &mut defs, &mut ptr2uid);

    // Native (C-ABI FFI) references — `__native$<pkgid>$<fn>` globals produced by
    // import namespacing — are lowered directly: a `Call` on one dispatches through
    // `jrt_native_call` (against the package `dlopen`'d in main's prologue), and a
    // native ref used as a value materializes a `jade_fn_t` sentinel value that the
    // indirect-call path recognizes. See `parse_native_ref`, `emit_native_call`,
    // and `emit_native_fn_value` / `indirect_call`.

    let global_fns = build_global_fns(top, &defs, &ptr2uid);
    let i64_ty = context.i64_type();

    // Forward-declare every function first, so bodies can call each other.
    let mut funcs = Vec::with_capacity(defs.len());
    for (uid, cf) in defs.iter().enumerate() {
        let ptys: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![i64_ty.into(); cf.params.len()];
        funcs.push(module.add_function(&format!("jf_{uid}"), i64_ty.fn_type(&ptys, false), None));
    }
    // Async task-entry wrappers: `jf_task_<uid>(i64* args, i32 n) -> i64` unpacks
    // the argument array and calls `jf_<uid>` — the `jade_task_fn` ABI expected by
    // `jade_spawn`. Emitted for every function (unused ones are DCE'd); each only
    // references the already-declared `jf_<uid>`.
    {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i32_ty = context.i32_type();
        let builder = context.create_builder();
        for (uid, cf) in defs.iter().enumerate() {
            let wrapper = module.add_function(
                &format!("jf_task_{uid}"),
                i64_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false),
                Some(inkwell::module::Linkage::Internal),
            );
            let entry = context.append_basic_block(wrapper, "entry");
            builder.position_at_end(entry);
            let args_ptr = wrapper.get_nth_param(0).unwrap().into_pointer_value();
            let n = cf.params.len();
            let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(n);
            for i in 0..n {
                let slot = unsafe {
                    builder
                        .build_in_bounds_gep(
                            i64_ty,
                            args_ptr,
                            &[i64_ty.const_int(i as u64, false)],
                            "argslot",
                        )
                        .map_err(|e| e.to_string())?
                };
                let v = builder
                    .build_load(i64_ty, slot, "arg")
                    .map_err(|e| e.to_string())?
                    .into_int_value();
                argv.push(v.into());
            }
            let r = builder
                .build_call(funcs[uid], &argv, "taskret")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            builder.build_return(Some(&r)).map_err(|e| e.to_string())?;
        }
    }

    let struct_defaults = build_struct_defaults(struct_defs);
    let struct_field_names: HashMap<String, Vec<String>> = struct_defs
        .iter()
        .map(|(tn, fields)| (tn.clone(), fields.iter().map(|f| f.name().to_string()).collect()))
        .collect();
    // Every global the program assigns (SetGlobal) anywhere — a user variable that
    // shadows any reserved module name (so `name.method()` is not a module call).
    let mut user_globals = std::collections::HashSet::new();
    let mut collect_setglobals = |chunk: &Chunk| {
        for instr in &chunk.code {
            if let Instr::SetGlobal(n, _) = instr {
                user_globals.insert(n.clone());
            }
        }
    };
    collect_setglobals(top);
    for d in &defs {
        collect_setglobals(&d.chunk);
    }
    let fnctx = FnCtx {
        funcs,
        defs,
        ptr2uid,
        global_fns,
        struct_defaults,
        struct_field_names,
        method_candidates,
        user_globals,
    };

    for uid in 0..fnctx.defs.len() {
        let cf = fnctx.defs[uid].clone();
        lower_body(
            context,
            module,
            fnctx.funcs[uid],
            &cf.chunk,
            &fnctx,
            BodyOpts {
                n_slots: cf.n_slots,
                n_params: cf.params.len(),
                is_generator: cf.is_generator,
                track_recursion: true,
            },
        )?;
    }

    let top_fn = module.add_function("jade_toplevel", i64_ty.fn_type(&[], false), None);
    lower_body(
        context,
        module,
        top_fn,
        top,
        &fnctx,
        BodyOpts { n_slots: top_n_slots, n_params: 0, is_generator: false, track_recursion: false },
    )?;

    // Turn on runtime reference counting once, at the very start of
    // `jade_toplevel` (before any collection is allocated). This flips
    // `RC_ACTIVE` so the runtime builders retain inserted/copy-shared elements,
    // matching the incref/decref/scope-exit codegen has already emitted around
    // them. See gc.rs, and rc.rs for what every heap value owes this.
    {
        let en = module.get_function("jrt_rc_enable").unwrap_or_else(|| {
            module.add_function("jrt_rc_enable", context.void_type().fn_type(&[], false), None)
        });
        let entry =
            top_fn.get_first_basic_block().ok_or("codegen: jade_toplevel has no entry block")?;
        let eb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => eb.position_before(&first),
            None => eb.position_at_end(entry),
        }
        eb.build_call(en, &[], "").map_err(|e| e.to_string())?;
    }

    // Populate the runtime method registry (for dynamic dispatch of
    // ambiguous-arity extend methods) at the very start of `jade_toplevel`, which
    // `main` calls before any user code. Registering every extend method is
    // harmless; only ambiguous names are ever looked up.
    let regs: Vec<(&String, &String, usize)> = {
        let mut v = Vec::new();
        for (ty, methods) in extend_methods {
            for (m, mfn) in methods {
                if let Some(&uid) = fnctx.ptr2uid.get(&Arc::as_ptr(mfn)) {
                    v.push((ty, m, uid));
                }
            }
        }
        v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1))); // deterministic order
        v
    };
    if !regs.is_empty() {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let void_ty = context.void_type();
        let reg_fn = module.get_function("jrt_method_register").unwrap_or_else(|| {
            module.add_function(
                "jrt_method_register",
                void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false),
                None,
            )
        });
        let entry =
            top_fn.get_first_basic_block().ok_or("codegen: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        for (ty, m, uid) in regs {
            let tcstr = rb
                .build_global_string_ptr(ty, "mtype")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let mcstr = rb
                .build_global_string_ptr(m, "mname")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let fnptr = fnctx.funcs[uid].as_global_value().as_pointer_value();
            rb.build_call(reg_fn, &[tcstr.into(), mcstr.into(), fnptr.into()], "")
                .map_err(|e| e.to_string())?;
        }
    }

    // Emit the type -> field table a struct-typed prompt deref coerces against,
    // in declaration order, in the same startup prologue as the method
    // registry. A field with a compile-time default is marked as such, since
    // "omitted optional" and "omitted required" behave differently: one is
    // filled, the other re-prompts.
    if !fnctx.struct_field_names.is_empty() {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i64_ty2 = context.i64_type();
        let i32_ty2 = context.i32_type();
        let void_ty = context.void_type();
        let reg_field = module.get_function("jrt_struct_field").unwrap_or_else(|| {
            module.add_function(
                "jrt_struct_field",
                void_ty.fn_type(
                    &[ptr_ty.into(), ptr_ty.into(), i64_ty2.into(), i32_ty2.into()],
                    false,
                ),
                None,
            )
        });
        let entry =
            top_fn.get_first_basic_block().ok_or("codegen: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        let mut types: Vec<(&String, &Vec<String>)> = fnctx.struct_field_names.iter().collect();
        types.sort_by(|a, b| a.0.cmp(b.0)); // deterministic IR
        for (tn, fields) in types {
            let tcstr = rb
                .build_global_string_ptr(tn, "sftype")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let defaults = fnctx.struct_defaults.get(tn);
            for f in fields {
                let fcstr = rb
                    .build_global_string_ptr(f, "sffield")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                // The prompt flag is not consulted here: this registry feeds
                // struct *coercion* (`?p |> City`), which stores the default word
                // as-is on both engines. Boxing only happens where a literal is
                // built, which is `MakeStruct` above.
                let dv =
                    defaults.and_then(|ds| ds.iter().find(|(n, _, _)| n == f)).map(|(_, v, _)| v);
                let (word, has) = match dv {
                    Some(v) => (default_word_const(context, module, &rb, v)?, 1u64),
                    None => (i64_ty2.const_int(NIL, false), 0u64),
                };
                rb.build_call(
                    reg_field,
                    &[
                        tcstr.into(),
                        fcstr.into(),
                        word.into(),
                        i32_ty2.const_int(has, false).into(),
                    ],
                    "",
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    // Recover source-level names for the lowered functions, so `--lib` can
    // export `add` rather than `jf_7`.
    let named = fnctx
        .global_fns
        .iter()
        .filter_map(|(name, uid)| fnctx.funcs.get(*uid).map(|f| (name.clone(), *f)))
        .collect();

    Ok(LoweredProgram { toplevel: top_fn, global_fns: named })
}

/// Lower a single `Chunk` body into a new LLVM function `name() -> i64` (a
/// tagged value word). Thin wrapper over [`lower_body`] with no functions in
/// scope — used by the unit tests and any caller lowering an isolated body.
pub fn lower_chunk<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    code: &[Instr],
    n_slots: u32,
) -> Result<FunctionValue<'ctx>, String> {
    let function = module.add_function(name, context.i64_type().fn_type(&[], false), None);
    // An isolated body with no spans: the helper is handed bare instructions,
    // so a diagnostic from it carries no position, exactly as before.
    let chunk = Chunk {
        name: name.to_string(),
        code: code.to_vec(),
        spans: Vec::new(),
        fn_defs: Vec::new(),
    };
    lower_body(
        context,
        module,
        function,
        &chunk,
        &FnCtx::empty(),
        BodyOpts { n_slots, n_params: 0, is_generator: false, track_recursion: false },
    )?;
    Ok(function)
}

/// How a body is lowered, as opposed to what is in it.
///
/// Four scalars that always travel together and are always read together, on a
/// function that has plenty of arguments already.
#[derive(Clone, Copy)]
struct BodyOpts {
    n_slots: u32,
    /// Incoming parameters, copied into slots `0..n_params`.
    n_params: usize,
    is_generator: bool,
    /// Whether this body counts against the recursion limit. False for
    /// `jade_toplevel`, which is not itself a call — see `MAX_CALL_DEPTH`.
    track_recursion: bool,
}

/// Lower `code` into an already-declared `function`. `n_params` incoming i64
/// parameters are copied into slots `0..n_params` (the VM frame convention);
/// `fn_defs` are this chunk's nested function literals (for `LoadFn`).
///
/// `track_recursion` opts this body into the `jrt_recur_enter`/`_restore`
/// prologue-and-restore pair that counts it against the recursion limit. It is
/// false only for `jade_toplevel` (and the `lower_chunk` test helper, which
/// lowers an isolated body the same way): the VM's counterpart, `execute_chunk`
/// running the top-level script, is not itself a call through `call_fn` and so
/// never increments `VmState::call_depth` either — see `vm::call::MAX_CALL_DEPTH`.
/// Counting it here and not there would let a program pass on `jade run` at
/// exactly the depth it failed on under `jade build`, the same one-past-the-cap
/// disagreement TOOLCHAIN-BUGS #10 is about, just moved to a different frame.
fn lower_body<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    // The whole chunk rather than its `code` and `fn_defs` separately: it also
    // carries `spans`, one per instruction, which is what lets a build error
    // name a line. That was the only thing standing between the resolver and a
    // position — and it costs a parameter rather than adding one.
    chunk: &Chunk,
    fnctx: &FnCtx<'ctx>,
    opts: BodyOpts,
) -> Result<(), String> {
    let BodyOpts { n_slots, n_params, is_generator, track_recursion } = opts;
    let (code, fn_defs) = (&chunk.code[..], &chunk.fn_defs[..]);
    let i64_ty = context.i64_type();
    let builder = context.create_builder();

    // Entry block: one alloca per register slot (at least enough for the params).
    let entry = context.append_basic_block(function, "entry");
    builder.position_at_end(entry);
    let n = (n_slots as usize).max(n_params);
    let mut slots = Vec::with_capacity(n);
    for i in 0..n {
        slots.push(builder.build_alloca(i64_ty, &format!("r{i}")).map_err(|e| e.to_string())?);
    }
    // Nil-initialize every slot so the release-old-value logic in
    // `store`/`store_idx` (and scope-exit `decref`) never reads uninitialized
    // stack: a first store releases nil (a no-op), and an unwritten slot decref's
    // nil at scope exit. Done before the param copies so params overwrite the nil.
    {
        let nil = i64_ty.const_int(NIL, false);
        for s in &slots {
            builder.build_store(*s, nil).map_err(|e| e.to_string())?;
        }
    }
    // A generator opens its buffer before the body runs, and every `Return`
    // path closes it and hands the buffer back instead of the body's value.
    // The buffer is an ordinary array, so `len`, indexing, `for`, and printing
    // over a stream reuse what arrays already do.
    if is_generator {
        let f = module.get_function("jrt_yield_push").unwrap_or_else(|| {
            module.add_function("jrt_yield_push", context.void_type().fn_type(&[], false), None)
        });
        builder.build_call(f, &[], "").map_err(|e| e.to_string())?;
    }

    // Copy incoming parameters into slots 0..n_params (params are the first
    // locals; see `emit_fn`). Callers fill any omitted defaults, so every
    // parameter slot receives an argument.
    for (i, slot) in slots.iter().enumerate().take(n_params) {
        let p = function
            .get_nth_param(i as u32)
            .ok_or("lower_body: missing parameter")?
            .into_int_value();
        builder.build_store(*slot, p).map_err(|e| e.to_string())?;
    }
    // One jmp_buf per SetupHandler, allocated *in the entry block* so a handler
    // inside a loop reuses one stable buffer instead of growing the stack each
    // iteration. 256 bytes is conservative for every x86_64/arm64 jmp_buf.
    let buf_ty = context.i8_type().array_type(256);
    let mut handler_bufs: HashMap<usize, PointerValue> = HashMap::new();
    for (idx, instr) in code.iter().enumerate() {
        if matches!(instr, Instr::SetupHandler(..)) {
            let buf = builder
                .build_alloca(buf_ty, &format!("exc_buf{idx}"))
                .map_err(|e| e.to_string())?;
            handler_bufs.insert(idx, buf);
        }
    }
    // Snapshot the handler depth for a function that opens one, so every return
    // can unwind back to it. A function with no `try` never pushes a frame, and
    // (with this restore in place) no callee leaks one, so its depth on exit
    // already equals its depth on entry — it needs neither the call nor the slot.
    let exc_depth_slot = if handler_bufs.is_empty() {
        None
    } else {
        let i32_ty = context.i32_type();
        let f = match module.get_function("jade_exc_depth") {
            Some(f) => f,
            None => module.add_function("jade_exc_depth", i32_ty.fn_type(&[], false), None),
        };
        let d = builder
            .build_call(f, &[], "exc_depth0")
            .map_err(|e| e.to_string())?
            .as_any_value_enum()
            .into_int_value();
        let slot = builder.build_alloca(i32_ty, "exc_depth_entry").map_err(|e| e.to_string())?;
        builder.build_store(slot, d).map_err(|e| e.to_string())?;
        Some(slot)
    };

    // Snapshot the recursion-depth counter on entry and bump it — unlike
    // `exc_depth_slot` this runs for every *ordinary* function, since every
    // call counts against the limit, not just ones inside a `try`.
    // `jrt_recur_enter` raises (via `jrt_throw_runtime`) on its own if the
    // limit is already at its cap, so there is nothing to branch on here —
    // see runtime_aot's `common.c`.
    //
    // `jade_toplevel` (and `lower_chunk`'s isolated test bodies) opt out via
    // `track_recursion` — see this function's doc comment for why counting
    // them would disagree with the VM by exactly one frame.
    let recur_depth_slot = if track_recursion {
        let i32_ty = context.i32_type();
        let recur_depth_fn = match module.get_function("jrt_recur_depth") {
            Some(f) => f,
            None => module.add_function("jrt_recur_depth", i32_ty.fn_type(&[], false), None),
        };
        let recur_depth0 = builder
            .build_call(recur_depth_fn, &[], "recur_depth0")
            .map_err(|e| e.to_string())?
            .as_any_value_enum()
            .into_int_value();
        let slot = builder.build_alloca(i32_ty, "recur_depth_entry").map_err(|e| e.to_string())?;
        builder.build_store(slot, recur_depth0).map_err(|e| e.to_string())?;
        let recur_enter_fn = match module.get_function("jrt_recur_enter") {
            Some(f) => f,
            None => module.add_function(
                "jrt_recur_enter",
                context.void_type().fn_type(&[], false),
                None,
            ),
        };
        builder.build_call(recur_enter_fn, &[], "").map_err(|e| e.to_string())?;
        Some(slot)
    } else {
        None
    };

    let low = Lowerer {
        is_generator,
        ctx: context,
        module,
        builder: &builder,
        slots: &slots,
        n_params,
        exc_depth_slot,
        recur_depth_slot,
        entry_bufs: RefCell::new(HashMap::new()),
    };

    // One LLVM block per reconstructed basic block; entry branches to the first.
    let graph = cfg::build(code);
    if graph.blocks.is_empty() {
        // An empty body still opened a recursion frame above and must close it.
        low.emit_recur_restore()?;
        builder.build_return(Some(&i64_ty.const_int(NIL, false))).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let llblocks: Vec<LlvmBlock> = (0..graph.blocks.len())
        .map(|bi| context.append_basic_block(function, &format!("bb{bi}")))
        .collect();
    builder.build_unconditional_branch(llblocks[0]).map_err(|e| e.to_string())?;
    let call_builtins = resolve_builtin_calls(code);
    let (user_calls, skip_getfields) = resolve_user_calls(code, &chunk.spans, fn_defs, fnctx)?;

    let body = instr::BodyCtx {
        llblocks: &llblocks,
        graph: &graph,
        handler_bufs: &handler_bufs,
        call_builtins: &call_builtins,
        user_calls: &user_calls,
        fn_defs,
        fnctx,
    };

    for (bi, block) in graph.blocks.iter().enumerate() {
        builder.position_at_end(llblocks[bi]);
        let mut terminated = false;
        // Indexed on purpose: `idx` is the instruction's absolute position, which
        // `lower_instr` and `skip_getfields` both need. Iterating the slice would
        // mean carrying the offset separately to rebuild the same number.
        #[allow(clippy::needless_range_loop)]
        for idx in block.start..block.end {
            // Skip a GetField whose only use is a devirtualized method call (its
            // field is a method, so lowering it would raise "undefined field").
            if skip_getfields.contains(&idx) {
                continue;
            }
            terminated = lower_instr(&low, &code[idx], idx, &body)?;
        }
        // A block whose last instruction wasn't a terminator falls through.
        if !terminated {
            match block.succs.first() {
                Some(&succ) => {
                    builder
                        .build_unconditional_branch(llblocks[succ])
                        .map_err(|e| e.to_string())?;
                }
                None => {
                    // Implicit `return nil` (function ran off the end): release
                    // the local slots first, matching an explicit `Return`.
                    low.emit_scope_exit();
                    low.emit_exc_restore()?;
                    low.emit_recur_restore()?;
                    builder
                        .build_return(Some(&i64_ty.const_int(NIL, false)))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(())
}
