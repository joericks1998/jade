//! Call resolution and emission: direct, indirect, and function boxing.
//!
//! See this directory's README.

use super::*;

/// Builtins the Chunk backend can lower directly (devirtualized from
/// `GetGlobal(name)` + `Call`). A name here is only trusted when the program
/// never `SetGlobal`s it (so the global still holds the builtin, not a user
/// value). Grows as more builtins are supported.
pub(super) const LOWERABLE_BUILTINS: &[&str] =
    &["print", "write", "str", "int", "float", "bool", "char", "len"];

/// The single register an instruction writes, or `None` for pure
/// stores/control-flow. Used to invalidate builtin tracking when a register is
/// overwritten; it must name **every** register-writing opcode (missing one
/// would let a stale devirtualization survive an overwrite = miscompile).
pub(super) fn dest_reg(instr: &Instr) -> Option<Reg> {
    use Instr::*;
    match instr {
        // A yield writes to the generator's buffer and produces no register.
        Instr::Yield(_) => None,
        // Writes through a named global, not a register.
        Instr::SetIndexGlobal(..) => None,
        LoadInt(d, _)
        | LoadFloat(d, _)
        | LoadBool(d, _)
        | LoadStr(d, _)
        | LoadNil(d)
        | LoadFn(d, _)
        | MakeClosure(d, _)
        | GetLocal(d, _)
        | GetGlobal(d, _) => Some(*d),
        Move(d, _)
        | NegInt(d, _)
        | NegFloat(d, _)
        | IntToFloat(d, _)
        | BitNot(d, _)
        | Not(d, _)
        | MakePrompt(d, _)
        | UnaryOp(d, _, _)
        | GetTypeName(d, _)
        | Await(d, _)
        | PromptDeref(d, _, _, _) => Some(*d),
        AddInt(d, _, _)
        | SubInt(d, _, _)
        | MulInt(d, _, _)
        | DivInt(d, _, _)
        | ModInt(d, _, _)
        | AddFloat(d, _, _)
        | SubFloat(d, _, _)
        | MulFloat(d, _, _)
        | DivFloat(d, _, _)
        | ConcatStr(d, _, _)
        | BitAnd(d, _, _)
        | BitOr(d, _, _)
        | BitXor(d, _, _)
        | Shl(d, _, _)
        | Shr(d, _, _)
        | BinOp(d, _, _, _)
        | GetIndex(d, _, _)
        | GetField(d, _, _) => Some(*d),
        CmpEqInt(d, ..)
        | CmpNeInt(d, ..)
        | CmpLtInt(d, ..)
        | CmpGtInt(d, ..)
        | CmpLeInt(d, ..)
        | CmpGeInt(d, ..)
        | CmpEqFloat(d, ..)
        | CmpNeFloat(d, ..)
        | CmpLtFloat(d, ..)
        | CmpGtFloat(d, ..)
        | CmpLeFloat(d, ..)
        | CmpGeFloat(d, ..)
        | CmpLtIntFloat(d, ..)
        | CmpGtIntFloat(d, ..)
        | CmpLeIntFloat(d, ..)
        | CmpGeIntFloat(d, ..)
        | CmpLtFloatInt(d, ..)
        | CmpGtFloatInt(d, ..)
        | CmpLeFloatInt(d, ..)
        | CmpGeFloatInt(d, ..)
        | CmpEqBool(d, ..)
        | CmpNeBool(d, ..)
        | CmpLtBool(d, ..)
        | CmpGtBool(d, ..)
        | CmpLeBool(d, ..)
        | CmpGeBool(d, ..)
        | CatchMatches(d, ..)
        | CmpEqStr(d, ..)
        | CmpNeStr(d, ..)
        | CmpLtStr(d, ..)
        | CmpGtStr(d, ..)
        | CmpLeStr(d, ..)
        | CmpGeStr(d, ..)
        | CmpEq(d, ..)
        | CmpNe(d, ..)
        | CmpLt(d, ..)
        | CmpGt(d, ..)
        | CmpLe(d, ..)
        | CmpGe(d, ..) => Some(*d),
        Call(d, _, _)
        | CallNamed(d, _, _)
        | Spawn(d, _, _)
        | Join(d, _)
        | MakeArray(d, _)
        | MakeArrayArena(d, _)
        | ArenaMark(d)
        | MakeDict(d, _)
        | MakeStruct(d, _, _, _)
        | BuildFStr(d, _) => Some(*d),
        // Handler binds its caught register (in the landing block).
        SetupHandler(r, _) => Some(*r),
        // Pure stores / control flow / no-reg-dest.
        SetGlobal(..) | SetLocal(..) | SetIndex(..) | SetField(..) | Jump(_) | JumpIfFalse(..)
        | JumpIfTrue(..) | Return(_) | Halt | Raise(_) | PopHandler | ArenaReset(_)
        | ImportFile(..) | ImportFrom(..) => None,
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
                        // `print(x, end)` replaces the newline, which is how you
                        // write without one. Only the one-argument form lowered,
                        // so a program using the second refused to build.
                        "print" => args.len() == 1 || args.len() == 2,
                        "write" | "str" | "int" | "float" | "bool" | "char" | "len" => {
                            args.len() == 1
                        }
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
pub(super) fn collect_fns(
    top: &Chunk,
) -> (Vec<Arc<CompiledFn>>, HashMap<*const CompiledFn, usize>) {
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
/// One `extend` method a call could resolve to: which function, which type
/// declares it, and the argument counts it accepts (excluding `self`).
///
/// The type name is what a devirtualized call site guards on. Resolution picks
/// a candidate by name and arity alone, because bytecode carries no types, so
/// the receiver's type has to be checked where the values actually are.
/// The primitive method of the same name, when a struct declares one that
/// collides with a built-in method on strings, arrays or dicts.
///
/// Resolution reaches the struct method first, so `[1, 2].contains(x)` in a
/// program that also has `extend S { fn contains(self, x) { … } }` resolved to
/// S's — and the receiver is not an S. The call site keeps this so the branch
/// that discovers the receiver is not a struct has somewhere correct to go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PrimFallback {
    Str,
    Val,
}

#[derive(Clone)]
pub(super) struct MethodCand {
    pub uid: usize,
    pub type_name: String,
    pub required: usize,
    pub total: usize,
}

pub(super) fn collect_method_fns(
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    defs: &mut Vec<Arc<CompiledFn>>,
    ptr2uid: &mut HashMap<*const CompiledFn, usize>,
) -> HashMap<String, Vec<MethodCand>> {
    let mut candidates: HashMap<String, Vec<MethodCand>> = HashMap::new();
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
            candidates.entry(name.clone()).or_default().push(MethodCand {
                uid,
                type_name: ty.clone(),
                required,
                total,
            });
        }
    }
    // BFS the method bodies' nested function literals.
    while let Some(f) = queue.pop_front() {
        for c in &f.chunk.fn_defs {
            if let std::collections::hash_map::Entry::Vacant(e) = ptr2uid.entry(Arc::as_ptr(c)) {
                let u = defs.len();
                e.insert(u);
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
            Instr::LoadFn(d, idx) => match ptr2uid.get(&Arc::as_ptr(&top.fn_defs[*idx])) {
                Some(&uid) => {
                    reg_fn.insert(*d, uid);
                }
                None => {
                    reg_fn.remove(d);
                }
            },
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

    candidate.into_iter().filter(|(name, _)| counts.get(name).copied() == Some(1)).collect()
}

/// How the backend lowers a `Call`. A callee whose function is statically known
/// becomes a `Direct` call to `jf_<uid>` (filling omitted trailing defaults); a
/// callee that is a runtime function value (a parameter, a variable, an escaped
/// closure) becomes an `Indirect` call through its boxed function pointer. A
/// builtin call this backend lowers itself (print/str/int/…) is left out of the
/// map (handled by `resolve_builtin_calls`); a call to a reserved builtin we do
/// *not* lower makes the whole program decline (`Err`) to the legacy path.
pub(super) enum CallKind {
    Direct {
        uid: usize,
        args: Vec<Reg>,
    },
    /// A keyword-argument call to a known function, pre-resolved to one slot per
    /// parameter: `Some(reg)` was supplied (positionally or by name), `None` is
    /// filled from the parameter's default at the call site.
    DirectNamed {
        uid: usize,
        arg_slots: Vec<Option<Reg>>,
    },
    /// A struct method call `obj.name(args)` where `name` is a unique extend-block
    /// method → direct call to `jf_<uid>` with the receiver (`self_reg`) prepended
    /// as `self` (param 0) and omitted trailing defaults filled at the call site.
    MethodDirect {
        uid: usize,
        /// The struct type that declares this method. The call site checks the
        /// receiver against it before trusting the resolution — see
        /// `MethodCand` and `jrt_struct_is_type`.
        type_name: String,
        /// The method name, for the dynamic fallback the guard branches to.
        method: String,
        /// The primitive method of this name, when one exists — see
        /// [`PrimFallback`].
        prim: Option<PrimFallback>,
        self_reg: Reg,
        args: Vec<Reg>,
    },
    /// A genuinely-ambiguous struct method call `obj.method(args)` — two types
    /// define `method` with the same arity, so the target depends on `obj`'s
    /// runtime type. Looked up at runtime by (type-name, method) via
    /// `jrt_method_lookup` and called indirectly (`self` prepended). See
    /// `emit_dynamic_method`.
    MethodDynamic {
        recv: Reg,
        method: String,
        /// See [`PrimFallback`].
        prim: Option<PrimFallback>,
        args: Vec<Reg>,
    },
    /// `stream(?p)` / `stream(?p, mute_on=[g])` — streaming inference that
    /// prints tokens as they arrive and evaluates to the full response.
    ///
    /// `prompt` is the *un-dereferenced* prompt register: the producing
    /// `PromptDeref` is elided, because letting it run would infer twice (once
    /// for the deref, once for the stream) and print the response twice. That
    /// is the same hazard the non-streaming `?p` lowering documents, arrived at
    /// from the other direction.
    StreamCall {
        prompt: Reg,
        grammar: Option<Reg>,
    },
    /// A stdlib module-namespace call `module.method(args)` (`fs.read`, `path.ext`,
    /// …) resolved statically by name to a runtime symbol. Only layout-safe methods
    /// (string/scalar I/O — no legacy-layout collections) are lowered; the rest
    /// decline. See `emit_module_call`.
    ModuleCall {
        module: String,
        method: String,
        args: Vec<Reg>,
    },
    /// A native (C-ABI) package call `__native$<pkgid>$<fn>(args)` → dispatch
    /// through `jrt_native_call` against the `dlopen`'d package handle. Args and
    /// the result are already tagged words. See `emit_native_call`.
    NativeCall {
        pkgid: u32,
        fname: String,
        args: Vec<Reg>,
    },
    /// A string primitive method `s.method(args)` (`trim`/`upper`/`starts_with`/…)
    /// → the shared `jrt_str_*` symbol. Strings have one representation across both
    /// paths, so these reuse the legacy string helpers directly. See
    /// `emit_str_method`. (Method names unique to strings; `contains`/`split` are
    /// excluded — ambiguous with dict / returns a collection.)
    PrimStrMethod {
        recv: Reg,
        method: String,
        args: Vec<Reg>,
    },
    /// An array/dict primitive method `recv.method(args)` whose name is unique to
    /// one collection kind (`push`/`pop`/`sort`/`reverse` → array;
    /// `keys`/`values`/`has`/`get` → dict), so the receiver kind is known by name
    /// (frontend-checked). Lowered via the ObjHeader-aware `jrt_coll_*`/`jrt_karr_*`
    /// helpers. See `emit_val_method`. (`contains`/`len` are ambiguous → excluded.)
    PrimValMethod {
        recv: Reg,
        method: String,
        args: Vec<Reg>,
    },
    Indirect,
    /// `Spawn` of a statically-known async function → `jade_spawn(jf_task_<uid>,
    /// args, n)`. Only exact-arity spawns of a known function are lowered.
    Spawn {
        uid: usize,
        args: Vec<Reg>,
    },
}

/// Classify every `Call` in `code`. Function values are first-class (materialized
/// as boxed pointers), so nothing "escapes" — the only decline is a call to a
/// reserved builtin this backend doesn't lower (e.g. `len`), which must go to the
/// legacy path. Direct calls are a devirtualization optimization; every other
/// call (a runtime function value) lowers to an indirect call, sound because the
/// frontend guarantees a `Call`'s callee is callable and non-user-fn callables
/// (builtins/methods) arrive via `GetGlobal(reserved)`/`GetField` — the former
/// handled here, the latter an unsupported opcode that already forces fallback.
/// Prefix a build diagnostic with the source position of instruction `i`.
///
/// The interpreter's errors all read `[line:col] …`, and a build error that
/// named neither a line nor a file gave a large program nothing to search for.
/// `spans` is parallel to `code`; when it is empty — the isolated-body test
/// helper hands over bare instructions — the message goes out unprefixed rather
/// than carrying a position that would be a guess.
fn at(spans: &[crate::frontend::error::Span], i: usize, msg: String) -> String {
    match spans.get(i) {
        Some(sp) => format!("[{}:{}] {msg}", sp.line, sp.col),
        None => msg,
    }
}

pub(super) fn resolve_user_calls(
    code: &[Instr],
    // One per instruction, parallel to `code`. Empty when the caller had none
    // (the isolated-body test helper), in which case a diagnostic goes out
    // without a position rather than with a wrong one.
    spans: &[crate::frontend::error::Span],
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
    let mut reg_promptderef: HashMap<Reg, (Reg, usize, Option<Reg>)> = HashMap::new();
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
                    Some(m) => {
                        reg_getfield_module.insert(*d, (m, field.clone(), i));
                    }
                    None => {
                        reg_getfield.insert(*d, (*obj, field.clone(), i));
                    }
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
                // An unconstrained or grammar-constrained deref is still a
                // stream, so printing it can be fused into one live streaming
                // call. A *typed* one is not: coercion cannot happen until
                // generation finishes, so it has its own blocking call.
                if output_type.is_none() {
                    reg_promptderef.insert(*d, (*prompt, i, *grammar));
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
                    Some(uid) => {
                        reg_fn.insert(*d, uid);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                }
            }
            Instr::Move(d, s) => {
                match reg_fn.get(s).copied() {
                    Some(u) => {
                        reg_fn.insert(*d, u);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                }
                reg_global.remove(d);
                // Propagate method-value-ness so `let m = obj.f; m()` still resolves.
                match reg_getfield.get(s).cloned() {
                    Some(v) => {
                        reg_getfield.insert(*d, v);
                    }
                    None => {
                        reg_getfield.remove(d);
                    }
                }
                match reg_getfield_module.get(s).cloned() {
                    Some(v) => {
                        reg_getfield_module.insert(*d, v);
                    }
                    None => {
                        reg_getfield_module.remove(d);
                    }
                }
            }
            Instr::GetGlobal(d, name) => {
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
                match fnctx.global_fns.get(name).copied() {
                    Some(u) => {
                        reg_fn.insert(*d, u);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                }
                reg_global.insert(*d, name.clone());
            }
            Instr::GetLocal(d, slot) => {
                match slot_fn.get(slot).copied() {
                    Some(u) => {
                        reg_fn.insert(*d, u);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                }
                reg_global.remove(d);
                reg_getfield.remove(d);
                reg_getfield_module.remove(d);
            }
            Instr::SetLocal(slot, src) => match reg_fn.get(src).copied() {
                Some(u) => {
                    slot_fn.insert(*slot, u);
                }
                None => {
                    slot_fn.remove(slot);
                }
            },
            Instr::SetGlobal(_, _) => {}
            // Spawn an async function: only a statically-known callee with an
            // exact-arity argument list is lowered (no defaults through spawn).
            Instr::Spawn(d, callee, args) => {
                if let Some(&uid) = reg_fn.get(callee) {
                    let cf = &fnctx.defs[uid];
                    if args.len() > cf.params.len() {
                        return Err(
                            "this spawn passes more arguments than the function takes.".into()
                        );
                    }
                    for j in args.len()..cf.params.len() {
                        if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                            return Err("this spawn omits an argument that has no default.".into());
                        }
                    }
                    out.insert(i, CallKind::Spawn { uid, args: args.clone() });
                } else {
                    return Err("codegen: spawn of a non-static function".into());
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
                        return Err(format!("codegen: unsupported module call {module}.{method}"));
                    }
                } else if let Some((self_reg, mname, gf_idx)) = reg_getfield.get(callee).cloned() {
                    // A struct method wins the resolution below, so remember
                    // whether the same name also names a primitive method: the
                    // receiver might be an array or a string at runtime.
                    let prim = if chunk_str_method_supported(&mname, args.len()) {
                        Some(PrimFallback::Str)
                    } else if chunk_val_method_supported(&mname, args.len()) {
                        Some(PrimFallback::Val)
                    } else {
                        None
                    };
                    // A method call `obj.mname(args)`. Devirtualize to the one
                    // extend-block method named `mname` whose arg range accepts this
                    // call's arg count (disambiguating same-named methods by arity);
                    // otherwise try primitive methods, else decline.
                    if let Some((uid, type_name)) = fnctx.resolve_method(&mname, args.len()) {
                        out.insert(
                            i,
                            CallKind::MethodDirect {
                                uid,
                                type_name,
                                method: mname.clone(),
                                prim,
                                self_reg,
                                args: args.clone(),
                            },
                        );
                        // The producing GetField is a method lookup (would raise as a
                        // data-field access) and its result is now unused → skip it.
                        skip_getfields.insert(gf_idx);
                    } else if fnctx.method_candidates.contains_key(&mname) {
                        // A known extend method whose target is ambiguous by arity →
                        // dispatch on the receiver's runtime type.
                        out.insert(
                            i,
                            CallKind::MethodDynamic {
                                recv: self_reg,
                                method: mname,
                                prim,
                                args: args.clone(),
                            },
                        );
                        skip_getfields.insert(gf_idx);
                    } else if chunk_str_method_supported(&mname, args.len()) {
                        out.insert(
                            i,
                            CallKind::PrimStrMethod {
                                recv: self_reg,
                                method: mname,
                                args: args.clone(),
                            },
                        );
                        skip_getfields.insert(gf_idx);
                    } else if chunk_val_method_supported(&mname, args.len()) {
                        out.insert(
                            i,
                            CallKind::PrimValMethod {
                                recv: self_reg,
                                method: mname,
                                args: args.clone(),
                            },
                        );
                        skip_getfields.insert(gf_idx);
                    } else {
                        // Names the method, because that is what a reader can
                        // search for and what is almost always misspelled. It
                        // used to name `lower.rs` and call the construct
                        // "unsupported", which reads as "Jade cannot compile
                        // method calls" — alarming, and untrue. The receiver's
                        // type is a run-time thing, so unlike the interpreter
                        // this cannot say *which* type lacks it.
                        // `chunk_*_method_supported` answers false both for a
                        // name no type defines and for a real method called
                        // with the wrong number of arguments, and those are
                        // very different mistakes to be told about. Asking the
                        // arity table separates them, so `"abc".upper(1, 2, 3)`
                        // is told its arity rather than that `upper` does not
                        // exist — which it plainly does.
                        return Err(at(
                            spans,
                            i,
                            match crate::builtins::primitive_method_arity(&mname) {
                                Some(want) => format!(
                                    "`{mname}` takes {want} argument{}, but {} were given.",
                                    if want == 1 { "" } else { "s" },
                                    args.len()
                                ),
                                None => format!(
                                    "no method named `{mname}`. Method calls compile fine — this \
                                 one does not name a method any type defines, so check the \
                                 spelling against the type it is called on. `jade run` on the \
                                 same file will name that type."
                                ),
                            },
                        ));
                    }
                } else {
                    let kind = if let Some(&uid) = reg_fn.get(callee) {
                        // Statically-known function → direct call (fill defaults).
                        let cf = &fnctx.defs[uid];
                        if args.len() > cf.params.len() {
                            return Err(at(
                                spans,
                                i,
                                format!(
                                    "this call passes {} arguments, but the function takes {}.",
                                    args.len(),
                                    cf.params.len()
                                ),
                            ));
                        }
                        for j in args.len()..cf.params.len() {
                            if cf.defaults.get(j).and_then(|x| x.as_ref()).is_none() {
                                return Err(at(
                                    spans,
                                    i,
                                    format!(
                                        "this call omits argument {} (`{}`), which has no \
                                         default.",
                                        j + 1,
                                        cf.params.get(j).map(|p| p.as_str()).unwrap_or("?")
                                    ),
                                ));
                            }
                        }
                        Some(CallKind::Direct { uid, args: args.clone() })
                    } else if let Some(name) = reg_global.get(callee) {
                        // A named global callee. A native package reference dispatches
                        // through jrt_native_call; a builtin this backend lowers itself
                        // is left to `resolve_builtin_calls`; any other reserved builtin
                        // declines; otherwise it's a user variable holding a function.
                        if let Some((pkgid, fname)) = parse_native_ref(name) {
                            Some(CallKind::NativeCall {
                                pkgid,
                                fname: fname.to_string(),
                                args: args.clone(),
                            })
                        } else {
                            // `print(?p)` / `print(?p |> g)` fuse into one live
                            // streaming call. That is where muting lives now
                            // that `stream(?p, mute_on=[g])` is gone: the mute
                            // anchors ride on the Grammar the stage names.
                            //
                            // Fusing also avoids printing twice. The deref on
                            // its own returns text without emitting anything, so
                            // if the streaming call ran at the deref *and* print
                            // printed the result, `let r = ?p; print(r)` would
                            // show the response twice.
                            if name == "print"
                                && args.len() == 1
                                && let Some(&(prompt, deref_idx, gram)) =
                                    reg_promptderef.get(&args[0])
                            {
                                skip_getfields.insert(deref_idx);
                                out.insert(i, CallKind::StreamCall { prompt, grammar: gram });
                                reg_fn.remove(d);
                                reg_global.remove(d);
                                reg_getfield.remove(d);
                                reg_getfield_module.remove(d);
                                continue;
                            }
                            let lowered = LOWERABLE_BUILTINS.contains(&name.as_str())
                                && match name.as_str() {
                                    // `print(x, end)` replaces the newline.
                                    "print" => args.len() == 1 || args.len() == 2,
                                    "write" | "str" | "int" | "float" | "bool" | "char" | "len" => {
                                        args.len() == 1
                                    }
                                    _ => false,
                                };
                            if lowered {
                                None
                            } else if RESERVED_BUILTINS.contains(&name.as_str()) {
                                return Err(format!("codegen: unsupported builtin call `{name}`"));
                            } else if fnctx.struct_field_names.contains_key(name) {
                                // A struct type is not callable — `City { .. }` is
                                // the one way to build one. This still has to be
                                // recognised rather than left to fall through: a
                                // type name is not a known function, so `Indirect`
                                // would load a fn pointer from a global cell codegen
                                // never assigns and jump through it.
                                return Err(format!(
                                    "codegen: `{name}` is a struct type, not a function — build one with `{name} {{ ... }}`"
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
                    // This one really is a limitation rather than a mistake, so
                    // it says so — and says what to write instead, which the
                    // old wording did not.
                    return Err(at(
                        spans,
                        i,
                        "a method call with named arguments is not compiled yet. Pass them \
                         positionally, or run the file with `jade run`."
                            .to_string(),
                    ));
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
                        None => return Err("codegen: unsupported keyword module call".into()),
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
                                .ok_or_else(|| format!("codegen: no parameter `{n}`"))?,
                        };
                        if slot >= p || arg_slots[slot].is_some() {
                            return Err("codegen: bad keyword-argument call".into());
                        }
                        arg_slots[slot] = Some(*reg);
                    }
                    for (i, slot) in arg_slots.iter().enumerate().take(p) {
                        if slot.is_none() && cf.defaults.get(i).and_then(|x| x.as_ref()).is_none() {
                            return Err("codegen: keyword call omits a required argument".into());
                        }
                    }
                    out.insert(i, CallKind::DirectNamed { uid, arg_slots });
                } else if let Some(name) = reg_global.get(callee) {
                    if RESERVED_BUILTINS.contains(&name.as_str()) {
                        return Err(format!("codegen: unsupported builtin kwarg call `{name}`"));
                    }
                    return Err("codegen: indirect keyword-argument call".into());
                } else {
                    return Err("codegen: indirect keyword-argument call".into());
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
    /// `f` must be the function's **indirect entry** (`jf_ind_<uid>`), not its
    /// body: a value is entered through `jrt_call_value`, which passes
    /// `(argc, argv)`. See `trampoline.rs`.
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
        let asint =
            self.builder.build_ptr_to_int(g.as_pointer_value(), self.i64t(), "boxp2i").unwrap();
        self.builder.build_or(asint, self.i64t().const_int(TAG_PTR, false), "boxtag").unwrap()
    }

    /// Indirect call through a first-class function value.
    ///
    /// Packs the arguments into a buffer and hands the whole thing to
    /// `jrt_call_value`, which is the one place that knows how to enter a plain
    /// function, a bound method (receiver prepended) and a native binding.
    ///
    /// This used to be built here, in LLVM: read the callee's kind byte, branch
    /// three ways, and build a fixed-arity call out of the arguments the site
    /// happened to have. The last part was the problem. A call site does not
    /// know which function the value holds, so it cannot know how many
    /// parameters that function has — it dropped extra arguments, read missing
    /// ones from uninitialised registers, and never filled a default. Arity now
    /// belongs to the callee's own entry (`trampoline.rs`), which knows it.
    pub(super) fn indirect_call(
        &self,
        callee: Reg,
        args: &[Reg],
    ) -> Result<IntValue<'ctx>, String> {
        let e = |x: inkwell::builder::BuilderError| x.to_string();
        let b = self.builder;
        let i64_ty = self.i64t();
        let ptrt = self.ptrt();

        // Entry-block buffer, not an `alloca` here: an indirect call inside a
        // loop would otherwise walk the stack down once per iteration. See
        // `Lowerer::entry_buf`.
        let argv = if args.is_empty() {
            ptrt.const_null()
        } else {
            let buf = self.entry_buf("icallv", args.len())?;
            for (i, a) in args.iter().enumerate() {
                let slot = unsafe {
                    b.build_in_bounds_gep(i64_ty, buf, &[i64_ty.const_int(i as u64, false)], "ia")
                        .map_err(e)?
                };
                b.build_store(slot, self.load(*a)).map_err(e)?;
            }
            buf
        };

        let f = self.runtime_fn(
            "jrt_call_value",
            i64_ty.fn_type(&[i64_ty.into(), i64_ty.into(), ptrt.into()], false),
        );
        Ok(b.build_call(
            f,
            &[
                self.load(callee).into(),
                i64_ty.const_int(args.len() as u64, false).into(),
                argv.into(),
            ],
            "icall",
        )
        .map_err(e)?
        .as_any_value_enum()
        .into_int_value())
    }
}
