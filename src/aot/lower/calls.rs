//! Call resolution and emission: direct, indirect, and function boxing.
//!
//! Split out of the former monolithic `lower.rs`; see this directory's README.

use super::*;

/// Builtins the Chunk backend can lower directly (devirtualized from
/// `GetGlobal(name)` + `Call`). A name here is only trusted when the program
/// never `SetGlobal`s it (so the global still holds the builtin, not a user
/// value). Grows as more builtins are supported.
pub(super) const LOWERABLE_BUILTINS: &[&str] = &["print", "write", "str", "int", "float", "bool", "len"];

/// The single register an instruction writes, or `None` for pure
/// stores/control-flow. Used to invalidate builtin tracking when a register is
/// overwritten; it must name **every** register-writing opcode (missing one
/// would let a stale devirtualization survive an overwrite = miscompile).
pub(super) fn dest_reg(instr: &Instr) -> Option<Reg> {
    use Instr::*;
    match instr {
        LoadInt(d, _) | LoadFloat(d, _) | LoadBool(d, _) | LoadStr(d, _) | LoadNil(d)
        | LoadFn(d, _) | MakeClosure(d, _) | GetLocal(d, _) | GetGlobal(d, _) => Some(*d),
        Move(d, _) | NegInt(d, _) | NegFloat(d, _) | IntToFloat(d, _) | BitNot(d, _)
        | Not(d, _) | MakePrompt(d, _) | UnaryOp(d, _, _) | GetTypeName(d, _)
        | Await(d, _) | PromptDeref(d, _, _, _) => Some(*d),
        AddInt(d, _, _) | SubInt(d, _, _) | MulInt(d, _, _) | DivInt(d, _, _) | ModInt(d, _, _)
        | AddFloat(d, _, _) | SubFloat(d, _, _) | MulFloat(d, _, _) | DivFloat(d, _, _)
        | ConcatStr(d, _, _) | BitAnd(d, _, _) | BitOr(d, _, _) | BitXor(d, _, _)
        | Shl(d, _, _) | Shr(d, _, _) | BinOp(d, _, _, _) | GetIndex(d, _, _)
        | GetField(d, _, _) => Some(*d),
        CmpEqInt(d, ..) | CmpNeInt(d, ..) | CmpLtInt(d, ..) | CmpGtInt(d, ..) | CmpLeInt(d, ..)
        | CmpGeInt(d, ..) | CmpEqFloat(d, ..) | CmpNeFloat(d, ..) | CmpLtFloat(d, ..)
        | CmpGtFloat(d, ..) | CmpLeFloat(d, ..) | CmpGeFloat(d, ..) | CmpLtIntFloat(d, ..)
        | CmpGtIntFloat(d, ..) | CmpLeIntFloat(d, ..) | CmpGeIntFloat(d, ..)
        | CmpLtFloatInt(d, ..) | CmpGtFloatInt(d, ..) | CmpLeFloatInt(d, ..)
        | CmpGeFloatInt(d, ..) | CmpEqBool(d, ..) | CmpNeBool(d, ..) | CmpLtBool(d, ..)
        | CmpGtBool(d, ..) | CmpLeBool(d, ..) | CmpGeBool(d, ..) | CmpEqStr(d, ..)
        | CmpNeStr(d, ..) | CmpLtStr(d, ..) | CmpGtStr(d, ..) | CmpLeStr(d, ..)
        | CmpGeStr(d, ..) | CmpEq(d, ..) | CmpNe(d, ..) | CmpLt(d, ..) | CmpGt(d, ..)
        | CmpLe(d, ..) | CmpGe(d, ..) => Some(*d),
        Call(d, _, _) | CallNamed(d, _, _) | Spawn(d, _, _) | Join(d, _) | MakeArray(d, _)
        | MakeArrayArena(d, _) | ArenaMark(d) | MakeDict(d, _) | MakeStruct(d, _, _)
        | BuildFStr(d, _) => Some(*d),
        // Handler binds its caught register (in the landing block).
        SetupHandler(r, _) => Some(*r),
        // Pure stores / control flow / no-reg-dest.
        SetGlobal(..) | SetLocal(..) | SetIndex(..) | SetField(..) | Jump(_)
        | JumpIfFalse(..) | JumpIfTrue(..) | Return(_) | Halt | Raise(_) | PopHandler
        | ArenaReset(_) | ImportFile(..) | ImportFrom(..) => None,
    }
}

/// A call the pre-scan devirtualized to a supported builtin.
pub(super) struct BuiltinCall {
    pub(super) name: &'static str,
    pub(super) args: Vec<Reg>,
}

/// Resolve which `Call`s target a lowerable builtin. Tracks, forward over the
/// flat stream, which registers hold a builtin function value (bound by
/// `GetGlobal(builtin)` and never overwritten). Sound because the tracked
/// globals are immutable (the no-`SetGlobal` guard) and any write to a tracked
/// register clears it — so a resolution can never name the wrong callee; at
/// worst it conservatively declines and the Call falls back.
pub(super) fn resolve_builtin_calls(code: &[Instr]) -> HashMap<usize, BuiltinCall> {
    let reassigned: std::collections::HashSet<&str> = code
        .iter()
        .filter_map(|i| match i {
            Instr::SetGlobal(n, _) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    let mut reg_builtin: HashMap<Reg, &'static str> = HashMap::new();
    let mut out: HashMap<usize, BuiltinCall> = HashMap::new();
    for (i, instr) in code.iter().enumerate() {
        match instr {
            Instr::GetGlobal(d, name) => {
                match LOWERABLE_BUILTINS.iter().copied().find(|b| *b == name.as_str()) {
                    Some(b) if !reassigned.contains(name.as_str()) => {
                        reg_builtin.insert(*d, b);
                    }
                    _ => {
                        reg_builtin.remove(d);
                    }
                }
            }
            Instr::Call(d, callee, args) => {
                if let Some(&b) = reg_builtin.get(callee) {
                    // Only resolve arities this backend lowers; others fall back.
                    let ok = match b {
                        "print" | "write" | "str" | "int" | "float" | "bool" | "len" => args.len() == 1,
                        _ => false,
                    };
                    if ok {
                        out.insert(i, BuiltinCall { name: b, args: args.clone() });
                    }
                }
                reg_builtin.remove(d);
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_builtin.remove(&d);
                }
            }
        }
    }
    out
}

/// Assign a stable uid to every `CompiledFn` reachable from `top` (breadth-first
/// so parents precede children), returning the uid→def table and the identity map.
pub(super) fn collect_fns(top: &Chunk) -> (Vec<Arc<CompiledFn>>, HashMap<*const CompiledFn, usize>) {
    let mut defs: Vec<Arc<CompiledFn>> = Vec::new();
    let mut ptr2uid: HashMap<*const CompiledFn, usize> = HashMap::new();
    let mut queue: VecDeque<Arc<CompiledFn>> = top.fn_defs.iter().cloned().collect();
    while let Some(f) = queue.pop_front() {
        let uid = defs.len();
        ptr2uid.insert(Arc::as_ptr(&f), uid);
        for c in &f.chunk.fn_defs {
            queue.push_back(c.clone());
        }
        defs.push(f);
    }
    (defs, ptr2uid)
}

/// Append every extend-block method body to `defs`/`ptr2uid` (assigning uids and
/// BFS-collecting each method's nested `fn_defs`), and return the method-name →
/// candidate-`(uid, required, total)` map (arg counts exclude `self`). A method
/// body is an ordinary `CompiledFn` whose first parameter is `self`, so once it
/// has a uid the normal forward-declare / lower / task-wrapper loops emit it like
/// any other function.
pub(super) fn collect_method_fns(
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    defs: &mut Vec<Arc<CompiledFn>>,
    ptr2uid: &mut HashMap<*const CompiledFn, usize>,
) -> HashMap<String, Vec<(usize, usize, usize)>> {
    let mut candidates: HashMap<String, Vec<(usize, usize, usize)>> = HashMap::new();
    let mut queue: VecDeque<Arc<CompiledFn>> = VecDeque::new();
    // Deterministic order: sort by (type, method) so uids are stable across runs.
    let mut types: Vec<&String> = extend_methods.keys().collect();
    types.sort();
    for ty in types {
        let methods = &extend_methods[ty];
        let mut names: Vec<&String> = methods.keys().collect();
        names.sort();
        for name in names {
            let mfn = &methods[name];
            let uid = match ptr2uid.get(&Arc::as_ptr(mfn)) {
                Some(&u) => u,
                None => {
                    let u = defs.len();
                    ptr2uid.insert(Arc::as_ptr(mfn), u);
                    defs.push(mfn.clone());
                    queue.push_back(mfn.clone());
                    u
                }
            };
            // Arg counts excluding `self` (param 0): `total` = all trailing params,
            // `required` = those without a default.
            let total = mfn.params.len().saturating_sub(1);
            let required = (1..mfn.params.len())
                .filter(|&j| mfn.defaults.get(j).and_then(|d| d.as_ref()).is_none())
                .count();
            candidates.entry(name.clone()).or_default().push((uid, required, total));
        }
    }
    // BFS the method bodies' nested function literals.
    while let Some(f) = queue.pop_front() {
        for c in &f.chunk.fn_defs {
            if !ptr2uid.contains_key(&Arc::as_ptr(c)) {
                let u = defs.len();
                ptr2uid.insert(Arc::as_ptr(c), u);
                defs.push(c.clone());
                queue.push_back(c.clone());
            }
        }
    }
    candidates
}

/// Names that provably hold a function: bound once (whole-program) from a
/// `LoadFn` at top level. The single-assignment guard is checked across *every*
/// chunk, so a name a nested function later rebinds to a non-function is excluded.
pub(super) fn build_global_fns(
    top: &Chunk,
    defs: &[Arc<CompiledFn>],
    ptr2uid: &HashMap<*const CompiledFn, usize>,
) -> HashMap<String, usize> {
    // Count SetGlobal writes to each name across the whole program.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut count_in = |chunk: &Chunk| {
        for instr in &chunk.code {
            if let Instr::SetGlobal(n, _) = instr {
                *counts.entry(n.clone()).or_default() += 1;
            }
        }
    };
    count_in(top);
    for d in defs {
        count_in(&d.chunk);
    }

    // Forward-track which top-level register holds which function, and record
    // the name each such register is stored to.
    let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
    let mut candidate: HashMap<String, usize> = HashMap::new();
    for instr in &top.code {
        match instr {
            Instr::LoadFn(d, idx) => {
                match ptr2uid.get(&Arc::as_ptr(&top.fn_defs[*idx])) {
                    Some(&uid) => { reg_fn.insert(*d, uid); }
                    None => { reg_fn.remove(d); }
                }
            }
            Instr::SetGlobal(name, src) => {
                if let Some(&uid) = reg_fn.get(src) {
                    candidate.insert(name.clone(), uid);
                }
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_fn.remove(&d);
                }
            }
        }
    }

    candidate
        .into_iter()
        .filter(|(name, _)| counts.get(name).copied() == Some(1))
        .collect()
}

/// How the backend lowers a `Call`. A callee whose function is statically known
/// becomes a `Direct` call to `jf_<uid>` (filling omitted trailing defaults); a
/// callee that is a runtime function value (a parameter, a variable, an escaped
/// closure) becomes an `Indirect` call through its boxed function pointer. A
/// builtin call this backend lowers itself (print/str/int/…) is left out of the
/// map (handled by `resolve_builtin_calls`); a call to a reserved builtin we do
/// *not* lower makes the whole program decline (`Err`) to the legacy path.
pub(super) enum CallKind {
    Direct { uid: usize, args: Vec<Reg> },
    /// A keyword-argument call to a known function, pre-resolved to one slot per
    /// parameter: `Some(reg)` was supplied (positionally or by name), `None` is
    /// filled from the parameter's default at the call site.
    DirectNamed { uid: usize, arg_slots: Vec<Option<Reg>> },
    /// A struct method call `obj.name(args)` where `name` is a unique extend-block
    /// method → direct call to `jf_<uid>` with the receiver (`self_reg`) prepended
    /// as `self` (param 0) and omitted trailing defaults filled at the call site.
    MethodDirect { uid: usize, self_reg: Reg, args: Vec<Reg> },
    /// A genuinely-ambiguous struct method call `obj.method(args)` — two types
    /// define `method` with the same arity, so the target depends on `obj`'s
    /// runtime type. Looked up at runtime by (type-name, method) via
    /// `jrt_method_lookup` and called indirectly (`self` prepended). See
    /// `emit_dynamic_method`.
    MethodDynamic { recv: Reg, method: String, args: Vec<Reg> },
    /// `stream(?p)` / `stream(?p, mute_on=[g])` — streaming inference that
    /// prints tokens as they arrive and evaluates to the full response.
    ///
    /// `prompt` is the *un-dereferenced* prompt register: the producing
    /// `PromptDeref` is elided, because letting it run would infer twice (once
    /// for the deref, once for the stream) and print the response twice. That
    /// is the same hazard the non-streaming `?p` lowering documents, arrived at
    /// from the other direction.
    StreamCall { prompt: Reg, grammar: Option<Reg> },
    /// A stdlib module-namespace call `module.method(args)` (`fs.read`, `path.ext`,
    /// …) resolved statically by name to a runtime symbol. Only layout-safe methods
    /// (string/scalar I/O — no legacy-layout collections) are lowered; the rest
    /// decline. See `emit_module_call`.
    ModuleCall { module: String, method: String, args: Vec<Reg> },
    /// A native (C-ABI) package call `__native$<pkgid>$<fn>(args)` → dispatch
    /// through `jrt_native_call` against the `dlopen`'d package handle. Args and
    /// the result are already tagged words. See `emit_native_call`.
    NativeCall { pkgid: u32, fname: String, args: Vec<Reg> },
    /// A string primitive method `s.method(args)` (`trim`/`upper`/`starts_with`/…)
    /// → the shared `jrt_str_*` symbol. Strings have one representation across both
    /// paths, so these reuse the legacy string helpers directly. See
    /// `emit_str_method`. (Method names unique to strings; `contains`/`split` are
    /// excluded — ambiguous with dict / returns a collection.)
    PrimStrMethod { recv: Reg, method: String, args: Vec<Reg> },
    /// An array/dict primitive method `recv.method(args)` whose name is unique to
    /// one collection kind (`push`/`pop`/`sort`/`reverse` → array;
    /// `keys`/`values`/`has`/`get` → dict), so the receiver kind is known by name
    /// (frontend-checked). Lowered via the ObjHeader-aware `jrt_coll_*`/`jrt_karr_*`
    /// helpers. See `emit_val_method`. (`contains`/`len` are ambiguous → excluded.)
    PrimValMethod { recv: Reg, method: String, args: Vec<Reg> },
    Indirect,
    /// `Spawn` of a statically-known async function → `jade_spawn(jf_task_<uid>,
    /// args, n)`. Only exact-arity spawns of a known function are lowered.
    Spawn { uid: usize, args: Vec<Reg> },
}

/// Classify every `Call` in `code`. Function values are first-class (materialized
/// as boxed pointers), so nothing "escapes" — the only decline is a call to a
/// reserved builtin this backend doesn't lower (e.g. `len`), which must go to the
/// legacy path. Direct calls are a devirtualization optimization; every other
/// call (a runtime function value) lowers to an indirect call, sound because the
/// frontend guarantees a `Call`'s callee is callable and non-user-fn callables
/// (builtins/methods) arrive via `GetGlobal(reserved)`/`GetField` — the former
/// handled here, the latter an unsupported opcode that already forces fallback.
pub(super) fn resolve_user_calls(
    code: &[Instr],
    fn_defs: &[Arc<CompiledFn>],
    fnctx: &FnCtx,
) -> Result<(HashMap<usize, CallKind>, std::collections::HashSet<usize>), String> {
    // reg → uid of a statically-known function (for direct-call devirtualization).
    let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
    // local slot → uid (a function stored into a local).
    let mut slot_fn: HashMap<u32, usize> = HashMap::new();
    // reg → the global name it was last loaded from (to classify builtin callees).
    let mut reg_global: HashMap<Reg, String> = HashMap::new();
    // reg holding a `GetField` result → (receiver reg, field/method name, the
    // GetField's instruction index). Calling one is a method call: a unique struct
    // method devirtualizes (self = receiver), anything else declines.
    let mut reg_getfield: HashMap<Reg, (Reg, String, usize)> = HashMap::new();
    // reg holding a `module.method` GetField result → (module name, method, the
    // GetField's instruction index). The base was `GetGlobal`'d from a reserved
    // stdlib module name, so calling it is a module call, not a value method.
    let mut reg_getfield_module: HashMap<Reg, (String, String, usize)> = HashMap::new();
    let mut out: HashMap<usize, CallKind> = HashMap::new();
    // GetField instruction indices whose result is consumed *only* as the callee of
    // a devirtualized method call. Their field is a method (not a data field), so
    // lowering them would raise "undefined field" — the method dispatch replaces
    // them, so `lower_body` skips these opcodes entirely.
    //
    // Also carries `PromptDeref`s subsumed by a `stream()` call; the name is
    // historical, the set is just "instruction indices `lower_body` must skip".
    let mut skip_getfields: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // reg holding an *unconstrained* `?p` result → (the prompt reg, the deref's
    // instruction index). Only `stream()` consumes this: it needs the prompt
    // itself, not an already-inferred response.
    let mut reg_promptderef: HashMap<Reg, (Reg, usize)> = HashMap::new();
    // reg holding an array built from a literal → its element regs, so
    // `mute_on=[g]` can be resolved to the single grammar it carries.
    let mut reg_array_lit: HashMap<Reg, Vec<Reg>> = HashMap::new();

    for (i, instr) in code.iter().enumerate() {
        match instr {
            Instr::GetField(d, obj, field) => {
                reg_fn.remove(d);
                // A field access whose base was loaded from a reserved stdlib
                // module global is a `module.method` access (resolved by name) —
                // UNLESS the program assigns that name (a user variable shadowing
                // the module, e.g. `let sh = []`), in which case it's a value method.
                let module = reg_global
                    .get(obj)
                    .filter(|n| is_stdlib_module(n) && !fnctx.user_globals.contains(n.as_str()))
                    .cloned();
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match module {
                    Some(m) => { reg_getfield_module.insert(*d, (m, field.clone(), i)); }
                    None => { reg_getfield.insert(*d, (*obj, field.clone(), i)); }
                }
                continue;
            }
            Instr::PromptDeref(d, prompt, output_type, grammar) => {
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                reg_array_lit.remove(d);
                reg_promptderef.remove(d);
                // Only an unconstrained deref can be folded into a stream: a
                // typed or grammar-constrained one has its own inference call
                // with different semantics.
                if output_type.is_none() && grammar.is_none() {
                    reg_promptderef.insert(*d, (*prompt, i));
                }
                continue;
            }
            Instr::MakeArray(d, elems) | Instr::MakeArrayArena(d, elems) => {
                // 5b: MakeArrayArena is tracked and materialized exactly like a
                // heap MakeArray, so parity holds. Increment 5c switches the arena
                // case to `jrt_karr_new_arena` + region reset.
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                reg_promptderef.remove(d);
                reg_array_lit.insert(*d, elems.clone());
                continue;
            }
            Instr::LoadFn(d, idx) | Instr::MakeClosure(d, idx) => {
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match fnctx.uid_of(fn_defs, *idx) {
                    Some(uid) => { reg_fn.insert(*d, uid); }
                    None => { reg_fn.remove(d); }
                }
            }
            Instr::Move(d, s) => {
                match reg_fn.get(s).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.remove(d);
                // Propagate method-value-ness so `let m = obj.f; m()` still resolves.
                match reg_getfield.get(s).cloned() {
                    Some(v) => { reg_getfield.insert(*d, v); }
                    None => { reg_getfield.remove(d); }
                }
                match reg_getfield_module.get(s).cloned() {
                    Some(v) => { reg_getfield_module.insert(*d, v); }
                    None => { reg_getfield_module.remove(d); }
                }
            }
            Instr::GetGlobal(d, name) => {
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match fnctx.global_fns.get(name).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.insert(*d, name.clone());
            }
            Instr::GetLocal(d, slot) => {
                match slot_fn.get(slot).copied() {
                    Some(u) => { reg_fn.insert(*d, u); }
                    None => { reg_fn.remove(d); }
                }
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            Instr::SetLocal(slot, src) => match reg_fn.get(src).copied() {
                Some(u) => { slot_fn.insert(*slot, u); }
                None => { slot_fn.remove(slot); }
            },
            Instr::SetGlobal(_, _) => {}
            // Spawn an async function: only a statically-known callee with an
            // exact-arity argument list is lowered (no defaults through spawn).
            Instr::Spawn(d, callee, args) => {
                if let Some(&uid) = reg_fn.get(callee) {
                    let cf = &fnctx.defs[uid];
                    if args.len() > cf.params.len() {
                        return Err("lower.rs: spawn passes more arguments than parameters".into());
                    }
                    for j in args.len()..cf.params.len() {
                        if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                            return Err("lower.rs: spawn omits a required argument".into());
                        }
                    }
                    out.insert(i, CallKind::Spawn { uid, args: args.clone() });
                } else {
                    return Err("lower.rs: spawn of a non-static function".into());
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            Instr::Call(d, callee, args) => {
                if let Some((module, method, gf_idx)) = reg_getfield_module.get(callee).cloned() {
                    // A stdlib module call `module.method(args)`. Lower the
                    // layout-safe subset; anything else declines to the legacy path.
                    if chunk_module_supported(&module, &method, args.len()) {
                        out.insert(i, CallKind::ModuleCall { module, method, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else {
                        return Err(format!("lower.rs: unsupported module call {module}.{method}"));
                    }
                } else if let Some((self_reg, mname, gf_idx)) = reg_getfield.get(callee).cloned() {
                    // A method call `obj.mname(args)`. Devirtualize to the one
                    // extend-block method named `mname` whose arg range accepts this
                    // call's arg count (disambiguating same-named methods by arity);
                    // otherwise try primitive methods, else decline.
                    if let Some(uid) = fnctx.resolve_method(&mname, args.len()) {
                        out.insert(i, CallKind::MethodDirect { uid, self_reg, args: args.clone() });
                        // The producing GetField is a method lookup (would raise as a
                        // data-field access) and its result is now unused → skip it.
                        skip_getfields.insert(gf_idx);
                    } else if fnctx.method_candidates.contains_key(&mname) {
                        // A known extend method whose target is ambiguous by arity →
                        // dispatch on the receiver's runtime type.
                        out.insert(i, CallKind::MethodDynamic { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else if chunk_str_method_supported(&mname, args.len()) {
                        out.insert(i, CallKind::PrimStrMethod { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else if chunk_val_method_supported(&mname, args.len()) {
                        out.insert(i, CallKind::PrimValMethod { recv: self_reg, method: mname, args: args.clone() });
                        skip_getfields.insert(gf_idx);
                    } else {
                        return Err("lower.rs: method call (GetField result) is unsupported".into());
                    }
                } else {
                    let kind = if let Some(&uid) = reg_fn.get(callee) {
                        // Statically-known function → direct call (fill defaults).
                        let cf = &fnctx.defs[uid];
                        if args.len() > cf.params.len() {
                            return Err("lower.rs: call passes more arguments than parameters".into());
                        }
                        for j in args.len()..cf.params.len() {
                            if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                                return Err("lower.rs: call omits a required argument".into());
                            }
                        }
                        Some(CallKind::Direct { uid, args: args.clone() })
                    } else if let Some(name) = reg_global.get(callee) {
                        // A named global callee. A native package reference dispatches
                        // through jrt_native_call; a builtin this backend lowers itself
                        // is left to `resolve_builtin_calls`; any other reserved builtin
                        // declines; otherwise it's a user variable holding a function.
                        if let Some((pkgid, fname)) = parse_native_ref(name) {
                            Some(CallKind::NativeCall { pkgid, fname: fname.to_string(), args: args.clone() })
                        } else {
                            let lowered = LOWERABLE_BUILTINS.contains(&name.as_str())
                                && matches!(name.as_str(), "print" | "write" | "str" | "int" | "float" | "bool" | "len")
                                && args.len() == 1;
                            if lowered {
                                None
                            } else if name == "stream" && args.len() == 1 {
                                // Checked before the reserved-builtin decline
                                // below: `stream` is reserved, and this is the
                                // one shape of it the backend can lower.
                                match reg_promptderef.get(&args[0]) {
                                    Some(&(prompt, deref_idx)) => {
                                        skip_getfields.insert(deref_idx);
                                        Some(CallKind::StreamCall { prompt, grammar: None })
                                    }
                                    // `stream(x)` where x is not a fresh `?p`.
                                    // The VM drains whatever TokenStream it is
                                    // handed; AOT has no such value to hold, so
                                    // this declines rather than guessing.
                                    None => return Err(
                                        "lower.rs: stream() requires a prompt dereference (`stream(?p)`)".into()
                                    ),
                                }
                            } else if RESERVED_BUILTINS.contains(&name.as_str()) {
                                return Err(format!("lower.rs: unsupported builtin call `{name}`"));
                            } else if fnctx.struct_field_names.contains_key(name) {
                                // A struct type is not callable — `City { .. }` is
                                // the one way to build one. This still has to be
                                // recognised rather than left to fall through: a
                                // type name is not a known function, so `Indirect`
                                // would load a fn pointer from a global cell codegen
                                // never assigns and jump through it.
                                return Err(format!(
                                    "lower.rs: `{name}` is a struct type, not a function — build one with `{name} {{ ... }}`"
                                ));
                            } else {
                                Some(CallKind::Indirect)
                            }
                        }
                    } else {
                        // A runtime function value (parameter / temporary) → indirect.
                        Some(CallKind::Indirect)
                    };
                    if let Some(k) = kind {
                        out.insert(i, k);
                    }
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            // Keyword-argument call. Only a direct call to a known function is
            // lowerable (named args need the callee's parameter names, which a
            // runtime function value doesn't carry) — anything else declines.
            Instr::CallNamed(d, callee, pairs) => {
                if reg_getfield.contains_key(callee) {
                    return Err("lower.rs: keyword method call (GetField result) is unsupported".into());
                }
                if let Some((module, method, gf_idx)) = reg_getfield_module.get(callee).cloned() {
                    // The one supported keyword module call: fs.read(path, trust=<bool>).
                    let resolved = if module == "fs" && method == "read" {
                        let (mut path, mut trust, mut ok) = (None, None, true);
                        for (name, reg) in pairs {
                            match name.as_deref() {
                                None if path.is_none() => path = Some(*reg),
                                Some("trust") => trust = Some(*reg),
                                _ => ok = false,
                            }
                        }
                        match (ok, path, trust) {
                            (true, Some(p), Some(t)) => Some(vec![p, t]),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    match resolved {
                        Some(args) => {
                            out.insert(i, CallKind::ModuleCall { module, method, args });
                            skip_getfields.insert(gf_idx);
                        }
                        None => return Err("lower.rs: unsupported keyword module call".into()),
                    }
                    reg_fn.remove(d);
                    reg_global.remove(d);
                    reg_getfield.remove(d);
                    reg_getfield_module.remove(d);
                    continue;
                }
                if let Some(&uid) = reg_fn.get(callee) {
                    let cf = &fnctx.defs[uid];
                    let p = cf.params.len();
                    let mut arg_slots: Vec<Option<Reg>> = vec![None; p];
                    let mut pos = 0usize;
                    for (name, reg) in pairs {
                        let slot = match name {
                            None => {
                                let s = pos;
                                pos += 1;
                                s
                            }
                            Some(n) => cf
                                .params
                                .iter()
                                .position(|param| param == n)
                                .ok_or_else(|| format!("lower.rs: no parameter `{n}`"))?,
                        };
                        if slot >= p || arg_slots[slot].is_some() {
                            return Err("lower.rs: bad keyword-argument call".into());
                        }
                        arg_slots[slot] = Some(*reg);
                    }
                    for i in 0..p {
                        if arg_slots[i].is_none()
                            && cf.defaults.get(i).and_then(|x| x.as_ref()).is_none()
                        {
                            return Err("lower.rs: keyword call omits a required argument".into());
                        }
                    }
                    out.insert(i, CallKind::DirectNamed { uid, arg_slots });
                } else if let Some(name) = reg_global.get(callee) {
                    if name == "stream" {
                        let (mut prompt_reg, mut mute_reg, mut ok) = (None, None, true);
                        for (n, reg) in pairs {
                            match n.as_deref() {
                                None if prompt_reg.is_none() => prompt_reg = Some(*reg),
                                Some("mute_on") => mute_reg = Some(*reg),
                                _ => ok = false,
                            }
                        }
                        let deref = prompt_reg.and_then(|r| reg_promptderef.get(&r).copied());
                        let (Some((prompt, gf_idx)), true) = (deref, ok) else {
                            return Err(
                                "lower.rs: stream() requires a prompt dereference and an optional mute_on=".into()
                            );
                        };
                        // `mute_on` is a list, but the streaming entry takes one
                        // anchor and one stop. A single grammar maps exactly; more
                        // than one would need mute regions the C side cannot
                        // express, so decline rather than silently honour the
                        // first and drop the rest.
                        let grammar = match mute_reg {
                            None => None,
                            Some(r) => match reg_array_lit.get(&r).map(|v| v.as_slice()) {
                                Some([]) => None,
                                Some([g]) => Some(*g),
                                Some(_) => return Err(
                                    "lower.rs: stream() mute_on= supports one grammar".into()
                                ),
                                None => return Err(
                                    "lower.rs: stream() mute_on= must be a list literal".into()
                                ),
                            },
                        };
                        skip_getfields.insert(gf_idx);
                        out.insert(i, CallKind::StreamCall { prompt, grammar });
                        reg_fn.remove(d);
                        reg_global.remove(d);
                        reg_getfield.remove(d);
                        reg_getfield_module.remove(d);
                        continue;
                    }
                    if RESERVED_BUILTINS.contains(&name.as_str()) {
                        return Err(format!("lower.rs: unsupported builtin kwarg call `{name}`"));
                    }
                    return Err("lower.rs: indirect keyword-argument call".into());
                } else {
                    return Err("lower.rs: indirect keyword-argument call".into());
                }
                reg_fn.remove(d);
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            other => {
                if let Some(d) = dest_reg(other) {
                    reg_fn.remove(&d);
                    reg_global.remove(&d);
                    reg_getfield.remove(&d);
                }
            }
        }
    }
    Ok((out, skip_getfields))
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// A first-class function value for `jf_<uid>`: a `TAG_PTR`-tagged pointer to
    /// an 8-aligned internal global `{ ptr fn_ptr@0, i64 kind@8 }`. All fn values
    /// for a uid share one box global (allocation-free). `indirect_call` reads
    /// `fn_ptr` at offset 0 and calls through it; the `kind` word at offset 8 holds
    /// `ObjKind::Fn`, aligned with `ObjHeader.kind`, so the refcount ops
    /// (`jrt_incref`/`jrt_decref`) recognise the box as a non-collection and no-op
    /// on it — which is what lets a program that merely *defines* functions still
    /// be treated as collections-only for refcounting.
    pub(super) fn fn_box_word(&self, uid: usize, f: FunctionValue<'ctx>) -> IntValue<'ctx> {
        let gname = format!("jf_box_{uid}");
        let g = self.module.get_global(&gname).unwrap_or_else(|| {
            let box_ty = self.ctx.struct_type(&[self.ptrt().into(), self.i64t().into()], false);
            let g = self.module.add_global(box_ty, None, &gname);
            let init = self.ctx.const_struct(
                &[
                    f.as_global_value().as_pointer_value().into(),
                    self.i64t().const_int(OBJKIND_FN, false).into(),
                ],
                false,
            );
            g.set_initializer(&init);
            g.set_constant(true);
            g.set_linkage(inkwell::module::Linkage::Internal);
            g.set_alignment(8);
            g
        });
        let asint = self
            .builder
            .build_ptr_to_int(g.as_pointer_value(), self.i64t(), "boxp2i")
            .unwrap();
        self.builder
            .build_or(asint, self.i64t().const_int(TAG_PTR, false), "boxtag")
            .unwrap()
    }

    /// Indirect call through a first-class function value: untag the callee box and
    /// load its `fn_ptr` (field 0). If `fn_ptr` is the `jrt_native_call` sentinel,
    /// the box is a native function value `{ sentinel, kind, env={handle,name} }` —
    /// dispatch through `jrt_native_call`. Otherwise it is an ordinary `jf_<uid>`
    /// box — call it directly with `args` (all tagged i64 words). The callee's arity
    /// equals `args.len()` (the frontend guarantees it).
    pub(super) fn indirect_call(&self, callee: Reg, args: &[Reg]) -> Result<IntValue<'ctx>, String> {
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let b = self.builder;
        let i64_ty = self.i64t();
        let ptrt = self.ptrt();

        let box_ptr = self.untag_ptr(self.load(callee));
        let fn_ptr = b.build_load(ptrt, box_ptr, "fnld").map_err(e)?.into_pointer_value();

        // A bound method (`let f = obj.greet`) is a function value carrying the
        // receiver it will pass as `self`: {fn_ptr@0, kind@8, self@16}. It is
        // told apart by the ObjKind byte at offset 8 rather than by a sentinel
        // address at offset 0 (the older native-fn trick) — every TAG_PTR value
        // must carry that kind byte anyway, so it costs nothing.
        let kind_slot = unsafe {
            b.build_in_bounds_gep(i64_ty, box_ptr, &[i64_ty.const_int(1, false)], "kslot").map_err(e)?
        };
        let kind = b.build_load(i64_ty, kind_slot, "kind").map_err(e)?.into_int_value();
        let is_bound = b
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                kind,
                i64_ty.const_int(OBJKIND_BOUND_METHOD as u64, false),
                "isbm",
            )
            .map_err(e)?;
        let outer_fn = b.get_insert_block().unwrap().get_parent().unwrap();
        let bm_bb = self.ctx.append_basic_block(outer_fn, "icall_bound");
        let plain_bb = self.ctx.append_basic_block(outer_fn, "icall_plain");
        let bm_merge_bb = self.ctx.append_basic_block(outer_fn, "icall_bm_merge");
        b.build_conditional_branch(is_bound, bm_bb, plain_bb).map_err(e)?;

        // ── bound: load self from slot 2, prepend it, call with args+1 ──
        b.position_at_end(bm_bb);
        let self_slot = unsafe {
            b.build_in_bounds_gep(i64_ty, box_ptr, &[i64_ty.const_int(2, false)], "sslot").map_err(e)?
        };
        let self_word = b.build_load(i64_ty, self_slot, "selfw").map_err(e)?.into_int_value();
        let bm_arg_tys = vec![i64_ty.into(); args.len() + 1];
        let bm_fn_ty = i64_ty.fn_type(&bm_arg_tys, false);
        let mut bm_argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len() + 1);
        bm_argv.push(self_word.into());
        for a in args {
            bm_argv.push(self.load(*a).into());
        }
        let bm_ret = b
            .build_indirect_call(bm_fn_ty, fn_ptr, &bm_argv, "bmcall")
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(bm_merge_bb).map_err(e)?;
        let bm_end = b.get_insert_block().unwrap();

        b.position_at_end(plain_bb);

        // Sentinel = the jrt_native_call address.
        let native_fn = self.runtime_fn(
            "jrt_native_call",
            i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
        );
        let sentinel = native_fn.as_global_value().as_pointer_value();
        let fp_int = b.build_ptr_to_int(fn_ptr, i64_ty, "fpi").map_err(e)?;
        let sent_int = b.build_ptr_to_int(sentinel, i64_ty, "si").map_err(e)?;
        let is_native = b
            .build_int_compare(inkwell::IntPredicate::EQ, fp_int, sent_int, "isnat")
            .map_err(e)?;

        let cur_fn = b.get_insert_block().unwrap().get_parent().unwrap();
        let nat_bb = self.ctx.append_basic_block(cur_fn, "icall_native");
        let reg_bb = self.ctx.append_basic_block(cur_fn, "icall_reg");
        let merge_bb = self.ctx.append_basic_block(cur_fn, "icall_merge");
        b.build_conditional_branch(is_native, nat_bb, reg_bb).map_err(e)?;

        // ── native: read env {handle, name}, marshal args, jrt_native_call ──
        // env is at slot 2; slot 1 is the ObjKind word (see emit_native_fn_value).
        b.position_at_end(nat_bb);
        let env_slot = unsafe {
            b.build_in_bounds_gep(ptrt, box_ptr, &[i64_ty.const_int(2, false)], "envs").map_err(e)?
        };
        let env = b.build_load(ptrt, env_slot, "env").map_err(e)?.into_pointer_value();
        let handle = b.build_load(ptrt, env, "nh").map_err(e)?.into_pointer_value();
        let name_slot = unsafe {
            b.build_in_bounds_gep(ptrt, env, &[i64_ty.const_int(1, false)], "nns").map_err(e)?
        };
        let name = b.build_load(ptrt, name_slot, "nn").map_err(e)?.into_pointer_value();
        let argv = if args.is_empty() {
            ptrt.const_null()
        } else {
            let arr = b
                .build_array_alloca(i64_ty, i64_ty.const_int(args.len() as u64, false), "iargv")
                .map_err(e)?;
            for (i, a) in args.iter().enumerate() {
                let slot = unsafe {
                    b.build_in_bounds_gep(i64_ty, arr, &[i64_ty.const_int(i as u64, false)], "ia").map_err(e)?
                };
                b.build_store(slot, self.load(*a)).map_err(e)?;
            }
            arr
        };
        let nat_ret = b
            .build_call(
                native_fn,
                &[handle.into(), name.into(), argv.into(), i64_ty.const_int(args.len() as u64, false).into()],
                "natret",
            )
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(merge_bb).map_err(e)?;
        let nat_end = b.get_insert_block().unwrap();

        // ── regular: direct indirect call jf_ptr(args) ──
        b.position_at_end(reg_bb);
        let arg_tys = vec![i64_ty.into(); args.len()];
        let fn_ty = i64_ty.fn_type(&arg_tys, false);
        let cargv: Vec<BasicMetadataValueEnum> = args.iter().map(|a| self.load(*a).into()).collect();
        let reg_ret = b
            .build_indirect_call(fn_ty, fn_ptr, &cargv, "icall")
            .map_err(e)?
            .as_any_value_enum()
            .into_int_value();
        b.build_unconditional_branch(merge_bb).map_err(e)?;
        let reg_end = b.get_insert_block().unwrap();

        // ── merge ──
        b.position_at_end(merge_bb);
        let phi = b.build_phi(i64_ty, "icall_ret").map_err(e)?;
        phi.add_incoming(&[(&nat_ret, nat_end), (&reg_ret, reg_end)]);
        let plain_ret = phi.as_basic_value().into_int_value();
        b.build_unconditional_branch(bm_merge_bb).map_err(e)?;
        let plain_end = b.get_insert_block().unwrap();

        // ── merge the bound and plain paths ──
        b.position_at_end(bm_merge_bb);
        let outer_phi = b.build_phi(i64_ty, "icall_out").map_err(e)?;
        outer_phi.add_incoming(&[(&bm_ret, bm_end), (&plain_ret, plain_end)]);
        Ok(outer_phi.as_basic_value().into_int_value())
    }

}
