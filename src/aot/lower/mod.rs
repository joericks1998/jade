//! Bytecode `Instr` → LLVM IR lowering (brick 2 of the Chunk→LLVM backend).
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
//! mostly free after optimization. Boxed floats and calls still hit the runtime
//! (added in later bricks).
//!
//! This brick handles the no-exception, no-call, no-heap subset: constant loads,
//! `Move`, integer arithmetic, control flow, and `Return`. Unsupported opcodes
//! return an `Err` so the daemon can fall back to the legacy `expr.rs` path
//! while this backend grows (the migration never runs an incomplete lowering).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use inkwell::attributes::AttributeLoc;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicMetadataValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use crate::bytecode::{Chunk, CompiledFn, FStrPart, Instr, Reg};
use crate::vm::VmValue;
use crate::frontend::ast::{BinOpKind, Expr, StructFieldDef, UnaryOpKind};

use super::cfg;

mod abi;
mod arith;
mod builtins;
mod calls;
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
use calls::*;
use builtins::*;
use instr::lower_instr;
use llm::emit_stream_call;
use strings::emit_str_method;
use abi::default_word_const;

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
    /// Reference-counting enabled for this program (collections-only; see
    /// `FnCtx::refcount`). When false every rc method below is a no-op, so the
    /// emitted IR is byte-identical to the pre-B4.2 backend.
    refcount: bool,
    /// Parameter count of the function being lowered. Parameter slots (`0..n_params`)
    /// hold references the *caller* owns (borrowed), so scope-exit release covers
    /// only the locals (`n_params..`).
    n_params: usize,
    /// Slot holding the exception-handler depth on entry, for functions that
    /// contain a `try`. Every return restores it, so a handler frame cannot
    /// outlive the stack frame holding its `jmp_buf` — see `emit_exc_restore`.
    /// `None` when the function has no handler and its depth cannot change.
    exc_depth_slot: Option<PointerValue<'ctx>>,
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {

































    // ── Reference counting (B4.2; all no-ops unless `self.refcount`) ──────────

























    // ── Dynamic (Unknown-operand) ops → tag-dispatching runtime (A7) ─────────
    // Operands are tagged words in slots, so — unlike the legacy static-SSA
    // backend — we pass the slot words straight to the `jrt_*_any` helpers (the
    // same decision core the VM runs via `dynop`). Arithmetic returns a tagged
    // word; comparisons return an i32 that we fold into a bool word.








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
    /// Whether the whole program is "collections-only" (no first-class functions,
    /// no async): if so, every `TAG_PTR` word is guaranteed to be an `ObjHeader`
    /// collection, so codegen emits reference-counting (incref/decref/scope-exit)
    /// and turns the runtime's `RC_ACTIVE` on via `jrt_rc_enable`. Otherwise no rc
    /// is emitted and heap objects leak (the pre-B4.2 behavior). See `gc.rs`.
    refcount: bool,
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
            refcount: false,
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

/// Whether the whole program is **refcount-safe**: every heap value it can put
/// in a `TAG_PTR` word is header-carrying, so `jrt_incref`/`jrt_decref` (and the
/// destructor's child cascade) can dispatch on the `ObjKind` byte at offset 8
/// and never touch a header-less allocation.
///
/// This is now unconditionally true, and the scan is gone. Every `TAG_PTR`
/// producer accounts for offset 8:
///
///  * collections (Array/Dict/Struct) and futures carry a real `ObjHeader`;
///  * grammar objects carry `ObjKind::Grammar` (`grammarf.rs`);
///  * ordinary fn boxes (`fn_box_word`) and native fn values
///    (`emit_native_fn_value`) carry `ObjKind::Fn` there, so the refcount ops
///    recognise them and no-op;
///  * a prompt is not a heap kind at all — `MakePrompt` stores the underlying
///    string, and a `TAG_STR` word is rejected by tag before any header is read.
///
/// It is kept as a named predicate rather than inlined as `true` so that the
/// invariant has somewhere to live: **anything that introduces a new `TAG_PTR`
/// value must put an `ObjKind` at offset 8, or re-introduce a veto here.** The
/// last holdout was the native fn value, whose offset 8 held the `env` pointer —
/// a heap address whose low byte, if it happened to be 2/3/4, would have sent
/// `free_obj` off to reclaim it as an Array/Dict/Struct.
fn program_collections_only(_top: &Chunk, _defs: &[Arc<CompiledFn>]) -> bool {
    true
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
        let ptys: Vec<inkwell::types::BasicMetadataTypeEnum> =
            vec![i64_ty.into(); cf.params.len()];
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
                        .build_in_bounds_gep(i64_ty, args_ptr, &[i64_ty.const_int(i as u64, false)], "argslot")
                        .map_err(|e| e.to_string())?
                };
                let v = builder.build_load(i64_ty, slot, "arg").map_err(|e| e.to_string())?.into_int_value();
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
    let refcount = program_collections_only(top, &defs);
    let fnctx = FnCtx { funcs, defs, ptr2uid, global_fns, struct_defaults, struct_field_names, method_candidates, user_globals, refcount };

    for uid in 0..fnctx.defs.len() {
        let cf = fnctx.defs[uid].clone();
        lower_body(
            context,
            module,
            fnctx.funcs[uid],
            &cf.chunk.code,
            &cf.chunk.fn_defs,
            cf.n_slots,
            cf.params.len(),
            &fnctx,
        )?;
    }

    let top_fn = module.add_function("jade_toplevel", i64_ty.fn_type(&[], false), None);
    lower_body(context, module, top_fn, &top.code, &top.fn_defs, top_n_slots, 0, &fnctx)?;

    // Turn on runtime reference counting for a collections-only program, once, at
    // the very start of `jade_toplevel` (before any collection is allocated). This
    // flips `RC_ACTIVE` so the runtime builders retain inserted/copy-shared
    // elements; codegen has already emitted the matching incref/decref/scope-exit
    // under the same `fnctx.refcount` decision. See gc.rs / program_collections_only.
    if fnctx.refcount {
        let en = module.get_function("jrt_rc_enable").unwrap_or_else(|| {
            module.add_function("jrt_rc_enable", context.void_type().fn_type(&[], false), None)
        });
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
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
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        for (ty, m, uid) in regs {
            let tcstr = rb.build_global_string_ptr(ty, "mtype").map_err(|e| e.to_string())?.as_pointer_value();
            let mcstr = rb.build_global_string_ptr(m, "mname").map_err(|e| e.to_string())?.as_pointer_value();
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
        let entry = top_fn.get_first_basic_block().ok_or("lower.rs: jade_toplevel has no entry block")?;
        let rb = context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => rb.position_before(&first),
            None => rb.position_at_end(entry),
        }
        let mut types: Vec<(&String, &Vec<String>)> = fnctx.struct_field_names.iter().collect();
        types.sort_by(|a, b| a.0.cmp(b.0)); // deterministic IR
        for (tn, fields) in types {
            let tcstr = rb.build_global_string_ptr(tn, "sftype").map_err(|e| e.to_string())?.as_pointer_value();
            let defaults = fnctx.struct_defaults.get(tn);
            for f in fields {
                let fcstr = rb.build_global_string_ptr(f, "sffield").map_err(|e| e.to_string())?.as_pointer_value();
                // The prompt flag is not consulted here: this registry feeds
                // struct *coercion* (`?p |> City`), which stores the default word
                // as-is on both engines. Boxing only happens where a literal is
                // built, which is `MakeStruct` above.
                let dv = defaults.and_then(|ds| ds.iter().find(|(n, _, _)| n == f)).map(|(_, v, _)| v);
                let (word, has) = match dv {
                    Some(v) => (default_word_const(context, module, &rb, v)?, 1u64),
                    None => (i64_ty2.const_int(NIL, false), 0u64),
                };
                rb.build_call(
                    reg_field,
                    &[tcstr.into(), fcstr.into(), word.into(), i32_ty2.const_int(has, false).into()],
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
    lower_body(context, module, function, code, &[], n_slots, 0, &FnCtx::empty())?;
    Ok(function)
}

/// Lower `code` into an already-declared `function`. `n_params` incoming i64
/// parameters are copied into slots `0..n_params` (the VM frame convention);
/// `fn_defs` are this chunk's nested function literals (for `LoadFn`).
fn lower_body<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    code: &[Instr],
    fn_defs: &[Arc<CompiledFn>],
    n_slots: u32,
    n_params: usize,
    fnctx: &FnCtx<'ctx>,
) -> Result<(), String> {
    let i64_ty = context.i64_type();
    let builder = context.create_builder();

    // Entry block: one alloca per register slot (at least enough for the params).
    let entry = context.append_basic_block(function, "entry");
    builder.position_at_end(entry);
    let n = (n_slots as usize).max(n_params);
    let mut slots = Vec::with_capacity(n);
    for i in 0..n {
        slots.push(
            builder
                .build_alloca(i64_ty, &format!("r{i}"))
                .map_err(|e| e.to_string())?,
        );
    }
    // In refcount mode, nil-initialize every slot so the release-old-value logic
    // in `store`/`store_idx` (and scope-exit `decref`) never reads uninitialized
    // stack: a first store releases nil (a no-op), and an unwritten slot decref's
    // nil at scope exit. Done before the param copies so params overwrite the nil.
    if fnctx.refcount {
        let nil = i64_ty.const_int(NIL, false);
        for s in &slots {
            builder.build_store(*s, nil).map_err(|e| e.to_string())?;
        }
    }
    // Copy incoming parameters into slots 0..n_params (params are the first
    // locals; see `emit_fn`). Callers fill any omitted defaults, so every
    // parameter slot receives an argument.
    for i in 0..n_params {
        let p = function
            .get_nth_param(i as u32)
            .ok_or("lower_body: missing parameter")?
            .into_int_value();
        builder.build_store(slots[i], p).map_err(|e| e.to_string())?;
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

    // One LLVM block per reconstructed basic block; entry branches to the first.
    let graph = cfg::build(code);
    if graph.blocks.is_empty() {
        builder
            .build_return(Some(&i64_ty.const_int(NIL, false)))
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let llblocks: Vec<LlvmBlock> = (0..graph.blocks.len())
        .map(|bi| context.append_basic_block(function, &format!("bb{bi}")))
        .collect();
    builder
        .build_unconditional_branch(llblocks[0])
        .map_err(|e| e.to_string())?;

    let low = Lowerer {
        ctx: context,
        module,
        builder: &builder,
        slots: &slots,
        refcount: fnctx.refcount,
        n_params,
        exc_depth_slot,
    };
    let call_builtins = resolve_builtin_calls(code);
    let (user_calls, skip_getfields) = resolve_user_calls(code, fn_defs, fnctx)?;

    for (bi, block) in graph.blocks.iter().enumerate() {
        builder.position_at_end(llblocks[bi]);
        let mut terminated = false;
        for idx in block.start..block.end {
            // Skip a GetField whose only use is a devirtualized method call (its
            // field is a method, so lowering it would raise "undefined field").
            if skip_getfields.contains(&idx) {
                continue;
            }
            terminated = lower_instr(
                &low,
                &code[idx],
                idx,
                &llblocks,
                &graph,
                &handler_bufs,
                &call_builtins,
                &user_calls,
                fn_defs,
                fnctx,
            )?;
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
                    builder
                        .build_return(Some(&i64_ty.const_int(NIL, false)))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(())
}
