//! The interpreter dispatch loop.
//!
//! [`execute_chunk`] runs a compiled [`Chunk`] against a register frame: it
//! decodes each `Instr`, drives control flow (jumps, calls, the exception
//! handler stack), and delegates value work to the shared runtime and to the
//! other `vm` submodules (`call`, `coerce`, `ops`, `llm_prompt`). The register
//! slot accessors it relies on live at the bottom of the file.

use super::*;
use crate::frontend::error::FieldOwner;

/// The two array methods whose implementation lives behind a `NativeFnId`
/// rather than a `BuiltinFn`, because each runs a Jade function per element.
///
/// They are the same functions as `array.map` / `array.filter`; only the
/// spelling differs, and only this spelling was missing.
fn array_fn_method(ty: PrimType, field: &str) -> Option<NativeFnId> {
    if ty != PrimType::Array {
        return None;
    }
    match field {
        "map" => Some(NativeFnId::ArrayMap),
        "filter" => Some(NativeFnId::ArrayFilter),
        _ => None,
    }
}

/// Execute `chunk` with the provided register frame.  Returns `Some(value)` if
/// a `Return` instruction was executed, `None` if execution ended normally.
pub(crate) async fn execute_chunk(
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
                    JadeError::Exception { .. } => state
                        .raised_exception
                        .take()
                        .unwrap_or_else(|| VmValue::Str("unknown exception".to_string().into())),
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
                Err(__e) => {
                    vm_err!(__e);
                }
            }
        };
    }

    loop {
        if ip >= chunk.code.len() {
            break;
        }
        let instr = &chunk.code[ip];
        let span = chunk.spans[ip];
        ip += 1;

        match instr {
            Instr::Halt => break,

            // ── Imports ───────────────────────────────────────────────────────
            Instr::ImportFile(path, namespace) => {
                // ── Built-in packages ───────────────────────────────────────
                // stdlib packages always bind under their own global_name; namespace param ignored.
                if let Some(pkg) = builtins::find_package(path) {
                    let val = package_dict_value(pkg);
                    state.globals.insert(pkg.global_name.to_string(), val);
                    continue;
                }

                // ── Native library modules ──────────────────────────────────
                // A `[lib]` module whose file is a .dylib/.so/.dll is loaded over
                // the C ABI and bound (as a dict of functions) under its module name.
                let abs_path = match resolve_user_import(state, path, span)? {
                    ResolvedImport::Native(lib_path) => {
                        let fns = crate::native::load_native_package(&lib_path, span)?;
                        state
                            .globals
                            .insert(namespace.clone(), VmValue::dict(fns.into_iter().collect()));
                        continue;
                    }
                    ResolvedImport::File(p) => p,
                };

                // ── User .jde files — namespaced ────────────────────────────
                let canon = abs_path
                    .canonicalize()
                    .map_err(|_| JadeError::ImportNotFound { path: path.clone(), span })?;

                if state.import_stack.contains(&canon) {
                    return Err(JadeError::CircularImport { path: path.clone(), span });
                }

                state.import_stack.insert(canon.clone());

                let sub_source_dir =
                    canon.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();

                let compile_result: Result<crate::compiler::emit::CompiledProgram> = (|| {
                    let source = std::fs::read_to_string(&canon)
                        .map_err(|_| JadeError::ImportNotFound { path: path.clone(), span })?;

                    let canon_str = canon.to_string_lossy().into_owned();
                    let hash = crate::cache::file_hash(&canon);

                    let cached_ast = hash.as_ref().and_then(crate::cache::read_ast_cache);
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
                })(
                );

                let result: Result<()> = match compile_result {
                    Ok(mut compiled) => {
                        // Stamp source file on all compiled functions so runtime
                        // errors inside module functions attribute to the correct file.
                        let file_label = canon.to_string_lossy().into_owned();
                        stamp_source_file(&mut compiled.top, &file_label);
                        for methods in compiled.extend_methods.values_mut() {
                            for cf_arc in methods.values_mut() {
                                let cf = Arc::make_mut(cf_arc);
                                if cf.source_file.is_empty() {
                                    cf.source_file = file_label.clone();
                                }
                                stamp_source_file(&mut cf.chunk, &file_label);
                            }
                        }
                        // Run the imported file in an isolated sub-state so its
                        // top-level bindings don't bleed into the parent namespace.
                        let mut sub_state = VmState::new();
                        // Capture keys already present so we can filter them out later.
                        let initial_keys: std::collections::HashSet<String> =
                            sub_state.globals.keys().cloned().collect();
                        // Propagate runtime config from parent.
                        sub_state.source_dir = sub_source_dir;
                        sub_state.import_stack = state.import_stack.clone();
                        sub_state.project_root = state.project_root.clone();
                        sub_state.libraries = state.libraries.clone();
                        sub_state.inference_backend = state.inference_backend.clone();

                        let r = Box::pin(run_with_state(compiled, &mut sub_state)).await;
                        if r.is_ok() {
                            // Collect user-defined globals (exclude builtins and internal keys).
                            let mut module_globals: HashMap<String, VmValue> = sub_state
                                .globals
                                .drain()
                                .filter(|(k, _)| !initial_keys.contains(k))
                                .collect();
                            // Stdlib packages imported by the module (e.g. `use std::fs`) must
                            // be promoted to the parent globals so that module functions can
                            // resolve them via GetGlobal when called in the parent context.
                            // They are NOT included in the module dict (they're not exports).
                            let pkg_keys: Vec<String> = module_globals
                                .keys()
                                .filter(|k| builtins::is_package_global_name(k))
                                .cloned()
                                .collect();
                            for k in pkg_keys {
                                if let Some(v) = module_globals.remove(&k) {
                                    state.globals.entry(k).or_insert(v);
                                }
                            }
                            // Create a persistent module scope shared by all functions from
                            // this file. Populated with user-defined module-level values so
                            // that reads and writes inside module functions are stable across
                            // calls. Functions in the scope are stored as Fn (not stamped) —
                            // they inherit the active scope via call_fn's save/restore logic.
                            let module_scope: Arc<Mutex<HashMap<String, VmValue>>> =
                                Arc::new(Mutex::new(module_globals.clone()));
                            // Stamp all Fn values in the exported dict with the module scope.
                            for v in module_globals.values_mut() {
                                if let VmValue::Fn(cf) = v {
                                    let cf_mut = Arc::make_mut(cf);
                                    cf_mut.module_scope = Some(Arc::clone(&module_scope));
                                }
                            }
                            // Qualify any TypeRef values so coercion calls resolve correctly.
                            for v in module_globals.values_mut() {
                                if let VmValue::TypeRef(t) = v {
                                    *t = format!("{}.{}", namespace, t);
                                }
                            }
                            state.globals.insert(
                                namespace.clone(),
                                VmValue::dict(module_globals.into_iter().collect()),
                            );

                            // Merge struct_defs under both the namespaced and the
                            // bare key.
                            //
                            // Two lookup conventions meet here. `TypeRef` coercion
                            // resolves through the qualified name (stamped just
                            // above), but every instance-side lookup uses the name
                            // carried on the instance itself — and that is always
                            // bare, because `infer_expr` normalizes `lib.Cfg` to
                            // `Cfg` (type_infer.rs:971) so that literals written
                            // outside the module agree with the ones written inside
                            // it. Registering only the qualified key left
                            // `MakeStruct` unable to find field defaults and
                            // `GetField` unable to find extend methods for any
                            // imported struct.
                            //
                            // Bare keys never overwrite: the importing file's own
                            // definitions are merged before its imports execute, so
                            // a local type of the same name keeps priority and two
                            // modules exporting the same name resolve to the first
                            // imported rather than the last.
                            for (k, v) in sub_state.struct_defs.drain() {
                                state.struct_defs.entry(k.clone()).or_insert_with(|| v.clone());
                                state.struct_defs.insert(format!("{}.{}", namespace, k), v);
                            }
                            // Merge extend_methods prefixed with the namespace.
                            // Stamp module_scope on each method so they can resolve
                            // module-level variables when called from the parent context.
                            for (type_name, mut methods) in sub_state.extend_methods.drain() {
                                for cf_arc in methods.values_mut() {
                                    let cf = Arc::make_mut(cf_arc);
                                    if cf.module_scope.is_none() {
                                        cf.module_scope = Some(Arc::clone(&module_scope));
                                    }
                                }
                                for (m_name, m_fn) in &methods {
                                    state
                                        .extend_methods
                                        .entry(type_name.clone())
                                        .or_default()
                                        .entry(m_name.clone())
                                        .or_insert_with(|| Arc::clone(m_fn));
                                }
                                state
                                    .extend_methods
                                    .entry(format!("{}.{}", namespace, type_name))
                                    .or_default()
                                    .extend(methods);
                            }
                            // Merge struct_ancestors under both the bare name
                            // and the namespaced one, the way struct_defs and
                            // extend_methods above are. A typed `catch` arm can
                            // name an imported parent either way.
                            for (type_name, anc) in sub_state.struct_ancestors.drain() {
                                if !state.struct_ancestors.contains_key(&type_name) {
                                    state.struct_ancestors.insert(type_name.clone(), anc.clone());
                                }
                                state
                                    .struct_ancestors
                                    .entry(format!("{}.{}", namespace, type_name))
                                    .or_insert(anc);
                            }
                            for (type_name, ps) in sub_state.struct_parents.drain() {
                                state.struct_parents.entry(type_name.clone()).or_insert(ps);
                            }
                            // Everything the module brought is now in reach, so a
                            // local struct that inherits one of its types can
                            // finally be completed. The checker could not: an
                            // imported struct is deliberately absent from the
                            // importing file's `struct_defs`, and this is the
                            // first moment it is not.
                            resolve_imported_parents(state);
                        }
                        r.map_err(|e| JadeError::InFile { file: path.clone(), cause: Box::new(e) })
                    }
                    Err(e) => Err(JadeError::InFile { file: path.clone(), cause: Box::new(e) }),
                };

                state.import_stack.remove(&canon);
                result?;
            }

            Instr::ImportFrom(path, names) => {
                if let Some(pkg) = builtins::find_package(path) {
                    // Build the package dict, then extract only the requested names.
                    let dict = package_dict_value(pkg);
                    if let VmValue::Dict(map) = dict {
                        for name in names {
                            if let Some(val) = map.get(name) {
                                state.globals.insert(name.clone(), val.clone());
                            }
                        }
                    }
                    continue;
                }

                // Native library: load over the C ABI and bind the requested
                // function names directly.
                let abs_path = match resolve_user_import(state, path, span)? {
                    ResolvedImport::Native(lib_path) => {
                        let fns = crate::native::load_native_package(&lib_path, span)?;
                        for name in names {
                            if let Some(val) = fns.get(name) {
                                state.globals.insert(name.clone(), val.clone());
                            }
                        }
                        continue;
                    }
                    ResolvedImport::File(p) => p,
                };

                // File import: run in an isolated sub-state, then bind only the
                // requested names directly into the parent namespace.
                let canon = abs_path
                    .canonicalize()
                    .map_err(|_| JadeError::ImportNotFound { path: path.clone(), span })?;
                if state.import_stack.contains(&canon) {
                    return Err(JadeError::CircularImport { path: path.clone(), span });
                }
                state.import_stack.insert(canon.clone());
                let sub_source_dir =
                    canon.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
                let compile_result: Result<crate::compiler::emit::CompiledProgram> = (|| {
                    let source = std::fs::read_to_string(&canon)
                        .map_err(|_| JadeError::ImportNotFound { path: path.clone(), span })?;
                    let tokens = crate::frontend::lexer::tokenize(&source)?;
                    let p = crate::frontend::parser::parse(tokens)?;
                    let tp = crate::compiler::type_infer::infer(p)?;
                    crate::compiler::emit::emit(tp)
                })(
                );
                let result: Result<()> = match compile_result {
                    Ok(mut compiled) => {
                        let file_label = canon.to_string_lossy().into_owned();
                        stamp_source_file(&mut compiled.top, &file_label);
                        let mut sub_state = VmState::new();
                        sub_state.source_dir = sub_source_dir;
                        sub_state.import_stack = state.import_stack.clone();
                        sub_state.project_root = state.project_root.clone();
                        sub_state.libraries = state.libraries.clone();
                        sub_state.inference_backend = state.inference_backend.clone();
                        let r = Box::pin(run_with_state(compiled, &mut sub_state)).await;
                        if r.is_ok() {
                            // Promote stdlib package imports from the module so that
                            // imported functions can resolve them via GetGlobal.
                            for (k, v) in sub_state.globals.iter() {
                                if builtins::is_package_global_name(k) {
                                    state.globals.entry(k.clone()).or_insert_with(|| v.clone());
                                }
                            }
                            // Build the persistent module scope for from-imports.
                            let initial_keys: std::collections::HashSet<String> =
                                VmState::new().globals.keys().cloned().collect();
                            let scope_map: HashMap<String, VmValue> = sub_state
                                .globals
                                .iter()
                                .filter(|(k, _)| {
                                    !initial_keys.contains(*k)
                                        && !builtins::is_package_global_name(k)
                                })
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect();
                            let module_scope: Arc<Mutex<HashMap<String, VmValue>>> =
                                Arc::new(Mutex::new(scope_map));
                            for name in names {
                                let val = sub_state.globals.remove(name);
                                let val = val.map(|v| match v {
                                    VmValue::Fn(mut cf) => {
                                        Arc::make_mut(&mut cf).module_scope =
                                            Some(Arc::clone(&module_scope));
                                        VmValue::Fn(cf)
                                    }
                                    other => other,
                                });
                                if let Some(val) = val {
                                    state.globals.insert(name.clone(), val);
                                }
                                // If the requested name is a struct type, also import its def.
                                if let Some(def) = sub_state.struct_defs.remove(name) {
                                    state.struct_defs.insert(name.clone(), def);
                                }
                                if let Some(mut methods) = sub_state.extend_methods.remove(name) {
                                    for cf_arc in methods.values_mut() {
                                        let cf = Arc::make_mut(cf_arc);
                                        if cf.module_scope.is_none() {
                                            cf.module_scope = Some(Arc::clone(&module_scope));
                                        }
                                    }
                                    state
                                        .extend_methods
                                        .entry(name.clone())
                                        .or_default()
                                        .extend(methods);
                                }
                            }
                        }
                        r.map_err(|e| JadeError::InFile { file: path.clone(), cause: Box::new(e) })
                    }
                    Err(e) => Err(JadeError::InFile { file: path.clone(), cause: Box::new(e) }),
                };
                state.import_stack.remove(&canon);
                result?;
            }

            // ── Loads ─────────────────────────────────────────────────────────
            Instr::LoadInt(d, v) => set(slots, *d, VmValue::Int(*v)),
            Instr::LoadFloat(d, v) => set(slots, *d, VmValue::Float(*v)),
            Instr::LoadBool(d, v) => set(slots, *d, VmValue::Bool(*v)),
            Instr::LoadStr(d, s) => set(slots, *d, VmValue::Str(s.clone().into())),
            Instr::LoadNil(d) => set(slots, *d, VmValue::Nil),
            Instr::LoadFn(d, idx) => {
                let cf = Arc::clone(&chunk.fn_defs[*idx]);
                set(slots, *d, VmValue::Fn(cf));
            }
            Instr::MakeClosure(d, idx) => {
                let cf = Arc::clone(&chunk.fn_defs[*idx]);
                let mut captured: HashMap<String, VmValue> =
                    state.globals.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                if let Some(sc) = &state.active_module_scope {
                    for (k, v) in sc.lock().iter() {
                        captured.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
                set(slots, *d, VmValue::Closure(cf, Arc::new(captured)));
            }
            Instr::Move(d, s) => {
                let v = get(slots, *s).clone();
                set(slots, *d, v);
            }

            // ── Variables ─────────────────────────────────────────────────────
            Instr::GetGlobal(d, name) => {
                let v = state
                    .active_module_scope
                    .as_ref()
                    .and_then(|sc| sc.lock().get(name).cloned())
                    .or_else(|| state.globals.get(name).cloned())
                    .ok_or_else(|| JadeError::UndefinedVariable { name: name.clone(), span })?;
                set(slots, *d, v);
            }
            Instr::SetGlobal(name, s) => {
                let v = vm_try!(vm_maybe_drain(get(slots, *s).clone(), state, span).await);
                if name == REPL_CAPTURE {
                    // REPL echo capture — never enters the global namespace.
                    state.repl_capture = Some(v);
                } else {
                    let wrote_to_scope = if let Some(sc) = &state.active_module_scope {
                        let mut locked = sc.lock();
                        if locked.contains_key(name) {
                            locked.insert(name.clone(), v.clone());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !wrote_to_scope {
                        state.globals.insert(name.clone(), v);
                    }
                }
            }
            Instr::GetLocal(d, slot) => {
                let v = slots.get(*slot as usize).cloned().unwrap_or(VmValue::Nil);
                set(slots, *d, v);
            }
            Instr::SetLocal(slot, s) => {
                let v = vm_try!(vm_maybe_drain(get(slots, *s).clone(), state, span).await);
                ensure_slot(slots, *slot);
                slots[*slot as usize] = v;
            }

            // ── Integer arithmetic (63-bit; see `int_ok`) ─────────────────────
            Instr::AddInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, vm_try!(int_ok(a.checked_add(b), span)));
            }
            Instr::SubInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, vm_try!(int_ok(a.checked_sub(b), span)));
            }
            Instr::MulInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, vm_try!(int_ok(a.checked_mul(b), span)));
            }
            Instr::DivInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b == 0 {
                    vm_err!(JadeError::DivisionByZero { span });
                }
                set(slots, *d, VmValue::Int(a / b));
            }
            Instr::ModInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if b == 0 {
                    vm_err!(JadeError::RemainderByZero { span });
                }
                set(slots, *d, VmValue::Int(a % b));
            }
            Instr::NegInt(d, s) => {
                let a = vm_try!(get_int(slots, *s, span));
                // Plain `-a` panicked in a debug build at the range edge.
                set(slots, *d, vm_try!(int_ok(a.checked_neg(), span)));
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
                if b == 0.0 {
                    vm_err!(JadeError::DivisionByZero { span });
                }
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
                let a = vm_try!(get_jstr(slots, *l, span));
                let b = vm_try!(get_jstr(slots, *r, span));
                let trust = jade_runtime::trust::combine(a.trust(), b.trust());
                set(
                    slots,
                    *d,
                    VmValue::Str(JStr::with_trust(format!("{}{}", a.as_str(), b.as_str()), trust)),
                );
            }

            // ── Bitwise ───────────────────────────────────────────────────────
            Instr::BitAnd(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(a & b));
            }
            Instr::BitOr(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(a | b));
            }
            Instr::BitXor(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Int(a ^ b));
            }
            Instr::BitNot(d, s) => {
                let a = vm_try!(get_int(slots, *s, span));
                set(slots, *d, VmValue::Int(!a));
            }
            Instr::Shl(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if !(0..64).contains(&b) {
                    vm_err!(JadeError::InvalidShift { amount: b, span });
                }
                set(slots, *d, VmValue::Int(a << b as u32));
            }
            Instr::Shr(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                if !(0..64).contains(&b) {
                    vm_err!(JadeError::InvalidShift { amount: b, span });
                }
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
            Instr::CmpEqInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b));
            }
            Instr::CmpNeInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a != b));
            }
            Instr::CmpLtInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a < b));
            }
            Instr::CmpGtInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a > b));
            }
            Instr::CmpLeInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a <= b));
            }
            Instr::CmpGeInt(d, l, r) => {
                let (a, b) = vm_try!(int2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a >= b));
            }

            // ── Typed comparisons — float ─────────────────────────────────────
            Instr::CmpEqFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b));
            }
            Instr::CmpNeFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a != b));
            }
            Instr::CmpLtFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a < b));
            }
            Instr::CmpGtFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a > b));
            }
            Instr::CmpLeFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a <= b));
            }
            Instr::CmpGeFloat(d, l, r) => {
                let (a, b) = vm_try!(flt2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a >= b));
            }

            // ── Typed comparisons — mixed ─────────────────────────────────────
            Instr::CmpLtIntFloat(d, l, r) => {
                let a = vm_try!(get_int(slots, *l, span)) as f64;
                let b = vm_try!(get_flt(slots, *r, span));
                set(slots, *d, VmValue::Bool(a < b));
            }
            Instr::CmpGtIntFloat(d, l, r) => {
                let a = vm_try!(get_int(slots, *l, span)) as f64;
                let b = vm_try!(get_flt(slots, *r, span));
                set(slots, *d, VmValue::Bool(a > b));
            }
            Instr::CmpLeIntFloat(d, l, r) => {
                let a = vm_try!(get_int(slots, *l, span)) as f64;
                let b = vm_try!(get_flt(slots, *r, span));
                set(slots, *d, VmValue::Bool(a <= b));
            }
            Instr::CmpGeIntFloat(d, l, r) => {
                let a = vm_try!(get_int(slots, *l, span)) as f64;
                let b = vm_try!(get_flt(slots, *r, span));
                set(slots, *d, VmValue::Bool(a >= b));
            }
            Instr::CmpLtFloatInt(d, l, r) => {
                let a = vm_try!(get_flt(slots, *l, span));
                let b = vm_try!(get_int(slots, *r, span)) as f64;
                set(slots, *d, VmValue::Bool(a < b));
            }
            Instr::CmpGtFloatInt(d, l, r) => {
                let a = vm_try!(get_flt(slots, *l, span));
                let b = vm_try!(get_int(slots, *r, span)) as f64;
                set(slots, *d, VmValue::Bool(a > b));
            }
            Instr::CmpLeFloatInt(d, l, r) => {
                let a = vm_try!(get_flt(slots, *l, span));
                let b = vm_try!(get_int(slots, *r, span)) as f64;
                set(slots, *d, VmValue::Bool(a <= b));
            }
            Instr::CmpGeFloatInt(d, l, r) => {
                let a = vm_try!(get_flt(slots, *l, span));
                let b = vm_try!(get_int(slots, *r, span)) as f64;
                set(slots, *d, VmValue::Bool(a >= b));
            }

            // ── Typed comparisons — bool ──────────────────────────────────────
            Instr::CmpEqBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b));
            }
            Instr::CmpNeBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a != b));
            }
            Instr::CmpLtBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(!a && b));
            }
            Instr::CmpGtBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a && !b));
            }
            Instr::CmpLeBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b || (!a && b)));
            }
            Instr::CmpGeBool(d, l, r) => {
                let (a, b) = vm_try!(bool2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b || (a && !b)));
            }

            // ── Typed comparisons — str ───────────────────────────────────────
            // A typed `catch` arm matches the named type or anything that
            // inherits it. The ancestry is flattened nearest-first at compile
            // time, so this is a membership test and never a walk.
            Instr::CatchMatches(d, actual, expected) => {
                let actual = vm_try!(get_str_ref(slots, *actual, span)).to_string();
                let matched = actual == *expected
                    || state
                        .struct_ancestors
                        .get(&actual)
                        .is_some_and(|anc| anc.iter().any(|a| a == expected));
                set(slots, *d, VmValue::Bool(matched));
            }
            Instr::CmpEqStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a == b));
            }
            Instr::CmpNeStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a != b));
            }
            Instr::CmpLtStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a < b));
            }
            Instr::CmpGtStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a > b));
            }
            Instr::CmpLeStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a <= b));
            }
            Instr::CmpGeStr(d, l, r) => {
                let (a, b) = vm_try!(str2(slots, *l, *r, span));
                set(slots, *d, VmValue::Bool(a >= b));
            }

            // ── Dynamic comparisons ───────────────────────────────────────────
            Instr::CmpEq(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, "==", span));
                set(slots, *d, v);
            }
            Instr::CmpNe(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, "!=", span));
                set(slots, *d, v);
            }
            Instr::CmpLt(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, "<", span));
                set(slots, *d, v);
            }
            Instr::CmpGt(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, ">", span));
                set(slots, *d, v);
            }
            Instr::CmpLe(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, "<=", span));
                set(slots, *d, v);
            }
            Instr::CmpGe(d, l, r) => {
                let v = vm_try!(cmp_dynamic(slots, *l, *r, ">=", span));
                set(slots, *d, v);
            }

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
                let args: Vec<VmValue> = arg_regs.iter().map(|&r| get(slots, r).clone()).collect();
                // Common case — calling a plain function value: borrow the
                // `Arc<CompiledFn>` in the slot instead of cloning the whole
                // `VmValue`. `call_fn` only needs `&CompiledFn`, so this skips an
                // atomic refcount bump+drop on every call (the dominant remaining
                // cost for call-heavy code once hashing is cheap). The slot borrow
                // is released before the result is stored back below.
                let result = match get(slots, *callee_reg) {
                    VmValue::Fn(cf) => call_fn(cf, args, state, span).await,
                    _ => {
                        let callee = get(slots, *callee_reg).clone();
                        call_value(callee, args, state, span).await
                    }
                };
                let result = vm_try!(result);
                set(slots, *dest, result);
            }
            Instr::CallNamed(dest, callee_reg, arg_pairs) => {
                let callee = get(slots, *callee_reg).clone();
                let mut positional: Vec<VmValue> = Vec::new();
                let mut named: Vec<(String, VmValue)> = Vec::new();
                for (name_opt, reg) in arg_pairs {
                    let val = get(slots, *reg).clone();
                    match name_opt {
                        None => positional.push(val),
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
                    None => VmValue::Nil,
                };
                return Ok(Some(v));
            }

            // ── Collections ───────────────────────────────────────────────────
            Instr::MakeArray(dest, elem_regs) | Instr::MakeArrayArena(dest, elem_regs) => {
                // Arena vs heap is an AOT-only distinction; the VM always builds a
                // reference-counted heap array, so the two lower identically here.
                let elems: Vec<VmValue> =
                    elem_regs.iter().map(|&r| get(slots, r).clone()).collect();
                set(slots, *dest, VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(elems)))));
            }
            // Arena bookkeeping is a no-op in the VM (it manages lifetime via Arc):
            // `ArenaMark` yields a dummy token, `ArenaReset` does nothing.
            Instr::ArenaMark(dest) => {
                set(slots, *dest, VmValue::Int(0));
            }
            Instr::ArenaReset(_) => {}
            Instr::MakeDict(dest, pairs) => {
                let mut map = DictObj::new();
                for &(kr, vr) in pairs {
                    let key_val = get(slots, kr).clone();
                    let key = match key_val {
                        VmValue::Str(s) => s,
                        ref other => {
                            vm_err!(JadeError::TypeError {
                                message: format!(
                                    "dict key must be str, got {}",
                                    value_type_name(other)
                                ),
                                span
                            });
                        }
                    };
                    let val = get(slots, vr).clone();
                    map.insert(key.into_string(), val);
                }
                set(slots, *dest, VmValue::dict(map));
            }
            Instr::GetIndex(dest, obj_reg, idx_reg) => {
                let obj = get(slots, *obj_reg).clone();
                let idx = get(slots, *idx_reg).clone();
                let result = vm_try!(vm_index(obj, idx, span));
                set(slots, *dest, result);
            }
            // `d[k] = v` where `d` is a global. Taking the dict out of the
            // binding for the write is the whole point: a dict is copy-on-write,
            // so leaving the global holding it would make every write copy the
            // whole dict, and filling one quadratic. Nothing can observe the gap
            // — the value is put back before the next instruction runs, and a
            // raise restores it on the way out.
            Instr::SetIndexGlobal(name, idx_reg, val_reg) => {
                let idx = get(slots, *idx_reg).clone();
                let val = get(slots, *val_reg).clone();
                let taken = state
                    .active_module_scope
                    .as_ref()
                    .and_then(|sc| sc.lock().remove(name))
                    .or_else(|| state.globals.remove(name));
                let Some(obj) = taken else {
                    vm_err!(JadeError::UndefinedVariable { name: name.clone(), span });
                };
                let put_back = |state: &mut VmState, v: VmValue| {
                    let scoped = state
                        .active_module_scope
                        .as_ref()
                        .is_some_and(|sc| sc.lock().contains_key(name));
                    match scoped {
                        true => {
                            if let Some(sc) = &state.active_module_scope {
                                sc.lock().insert(name.clone(), v);
                            }
                        }
                        false => {
                            state.globals.insert(name.clone(), v);
                        }
                    }
                };
                match obj {
                    VmValue::Dict(mut m) => {
                        let VmValue::Str(k) = idx else {
                            put_back(state, VmValue::Dict(m));
                            vm_err!(JadeError::TypeError {
                                message: format!(
                                    "dict index must be str, got {}",
                                    value_type_name(&idx)
                                ),
                                span
                            });
                        };
                        dict_mut(&mut m).insert(k.into_string(), val);
                        put_back(state, VmValue::Dict(m));
                    }
                    // An array has reference semantics, so the write goes
                    // straight through and the same value goes back.
                    VmValue::Array(arc) => {
                        put_back(state, VmValue::Array(Arc::clone(&arc)));
                        let VmValue::Int(i) = idx else {
                            vm_err!(JadeError::TypeError {
                                message: format!(
                                    "array index must be int, got {}",
                                    value_type_name(&idx)
                                ),
                                span
                            });
                        };
                        let len = arc.lock().len();
                        if i < 0 || i as usize >= len {
                            vm_err!(JadeError::IndexOutOfBounds { index: i, len, span });
                        }
                        arc.lock()[i as usize] = val;
                    }
                    // A blob has reference semantics too, so the same value
                    // goes back and the octet lands in place.
                    VmValue::Bytes(arc) => {
                        put_back(state, VmValue::Bytes(Arc::clone(&arc)));
                        if let Err(e) = write_octet(&arc, &idx, &val, span) {
                            vm_err!(e);
                        }
                    }
                    other => {
                        let name = value_type_name(&other).to_string();
                        put_back(state, other);
                        vm_err!(JadeError::TypeError {
                            message: format!("cannot index-assign into {name}"),
                            span
                        });
                    }
                }
            }
            Instr::SetIndex(obj_reg, idx_reg, val_reg) => {
                let idx = get(slots, *idx_reg).clone();
                let val = get(slots, *val_reg).clone();
                // Take the object out of its slot rather than cloning it. The
                // borrow is what has to go — `vm_err!` re-borrows `slots` via
                // `set` — and taking gives that up just as well as copying did.
                //
                // For a dict the difference is the whole cost of the opcode. A
                // dict is a value in Jade, so `VmValue::clone` deep-copies every
                // entry, and cloning here made a write O(n) in the dict's size:
                // filling one by assignment was quadratic, 4,000 keys taking 4s
                // against a rounding error for the same number of array pushes.
                // Nothing could observe the copy — a register's dict is owned by
                // that register, since anything that shared it copied on the way
                // in — so the value semantics are unchanged and only the cost is
                // gone. The array arm is unaffected either way: an array is an
                // `Arc`, and cloning one is a refcount bump.
                //
                // Only the dict arm is taken, and it is the only one that writes
                // a value back. Everything else still clones, so a raise out of
                // an error arm leaves the slot holding what it always held.
                let obj = match get(slots, *obj_reg) {
                    VmValue::Dict(_) => {
                        std::mem::replace(&mut slots[*obj_reg as usize], VmValue::Nil)
                    }
                    other => other.clone(),
                };
                match obj {
                    VmValue::Array(arc) => {
                        let i = match idx {
                            VmValue::Int(n) => n,
                            ref other => {
                                vm_err!(JadeError::TypeError {
                                    message: format!(
                                        "array index must be int, got {}",
                                        value_type_name(other)
                                    ),
                                    span
                                });
                            }
                        };
                        let len = arc.lock().len();
                        if i < 0 || i as usize >= len {
                            vm_err!(JadeError::IndexOutOfBounds { index: i, len, span });
                        }
                        arc.lock()[i as usize] = val;
                    }
                    VmValue::Bytes(arc) => {
                        if let Err(e) = write_octet(&arc, &idx, &val, span) {
                            vm_err!(e);
                        }
                    }
                    VmValue::Dict(mut m) => {
                        let k = match idx {
                            VmValue::Str(s) => s,
                            ref other => {
                                vm_err!(JadeError::TypeError {
                                    message: format!(
                                        "dict index must be str, got {}",
                                        value_type_name(other)
                                    ),
                                    span
                                });
                            }
                        };
                        // Copy-on-write: `dict_mut` clones only if something
                        // else is holding this dict, which is exactly when a
                        // caller could tell the difference.
                        dict_mut(&mut m).insert(k.into_string(), val);
                        slots[*obj_reg as usize] = VmValue::Dict(m);
                    }
                    ref other => {
                        vm_err!(JadeError::TypeError {
                            message: format!(
                                "value of type {} is not indexable",
                                value_type_name(other)
                            ),
                            span
                        });
                    }
                }
            }

            // ── Struct ────────────────────────────────────────────────────────
            Instr::MakeStruct(dest, type_name, field_specs) => {
                let mut sobj = StructObj::<VmValue>::new(type_name);
                for (fname, freg, is_prompt) in field_specs {
                    let mut val = get(slots, *freg).clone();
                    if *is_prompt {
                        val = match val {
                            VmValue::Str(text) => VmValue::Prompt(text.to_string()),
                            other => other, // already Prompt, or wrong type caught at type-check
                        };
                    }
                    sobj.set_field(fname, val);
                }
                // Fill in defaults for any fields omitted from the literal.
                // Needed when the struct type was unknown at compile time (imported type).
                if let Some(def_fields) = state.struct_defs.get(type_name.as_str()).cloned() {
                    for def_field in &def_fields {
                        match def_field {
                            StructFieldDef::Let { name, default } => {
                                if sobj.get_field(name).is_none()
                                    && let Some(v) = eval_literal_default(default)
                                {
                                    sobj.set_field(name, v);
                                }
                            }
                            StructFieldDef::Prompt { name, default } => {
                                if sobj.get_field(name).is_none()
                                    && let Some(v) = eval_literal_default(default)
                                {
                                    let v = match v {
                                        VmValue::Str(s) => VmValue::Prompt(s.to_string()),
                                        other => other,
                                    };
                                    sobj.set_field(name, v);
                                }
                            }
                            StructFieldDef::Required(_) => {}
                        }
                    }
                }
                set(slots, *dest, VmValue::Struct(Arc::new(Mutex::new(sobj))));
            }
            Instr::GetField(dest, obj_reg, field) => {
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Struct(rc) => {
                        let (type_name, field_val) = {
                            let guard = rc.lock();
                            (
                                guard.type_name().to_string(),
                                guard.get_field(field.as_str()).cloned(),
                            )
                        };
                        if let Some(v) = field_val {
                            set(slots, *dest, v);
                        } else if let Some(methods) = state.extend_methods.get(&type_name) {
                            if let Some(mfn) = methods.get(field.as_str()) {
                                set(
                                    slots,
                                    *dest,
                                    VmValue::BoundMethod(Arc::new(VmBoundMethod {
                                        receiver: rc,
                                        method: Arc::clone(mfn),
                                    })),
                                );
                            } else {
                                vm_err!(JadeError::UndefinedField {
                                    type_name,
                                    field: field.clone(),
                                    owner: FieldOwner::Struct,
                                    span
                                });
                            }
                        } else {
                            vm_err!(JadeError::UndefinedField {
                                type_name,
                                field: field.clone(),
                                owner: FieldOwner::Struct,
                                span
                            });
                        }
                    }
                    // Dict: check HashMap entries first (package namespaces), then primitive methods.
                    VmValue::Dict(ref map) => {
                        if let Some(v) = map.get(field.as_str()) {
                            set(slots, *dest, v.clone());
                        } else if let Some(method) =
                            builtins::find_primitive_method(PrimType::Dict, field)
                        {
                            set(
                                slots,
                                *dest,
                                VmValue::NativeBoundMethod(Arc::new(NativeBoundMethod {
                                    receiver: obj.clone(),
                                    method,
                                })),
                            );
                        } else {
                            vm_err!(JadeError::UndefinedField {
                                type_name: "dict".to_string(),
                                field: field.clone(),
                                owner: FieldOwner::Dict,
                                span,
                            });
                        }
                    }
                    // Primitive method dispatch for str/array/int/float.
                    ref prim @ (VmValue::Str(_)
                    | VmValue::Array(_)
                    | VmValue::Bytes(_)
                    | VmValue::Int(_)
                    | VmValue::Float(_)) => {
                        if let Some(ty) = PrimType::from_value(prim) {
                            // `a.map(f)` / `a.filter(f)`. Not reachable through
                            // `find_primitive_method`, which returns a pure
                            // `BuiltinFn` — these two run a Jade function per
                            // element and need the VM's call context, so they
                            // bind the receiver to the native id instead.
                            if let Some(id) = array_fn_method(ty, field) {
                                set(
                                    slots,
                                    *dest,
                                    VmValue::BoundNativeFn(Arc::new((id, prim.clone()))),
                                );
                            } else if let Some(method) = builtins::find_primitive_method(ty, field)
                            {
                                set(
                                    slots,
                                    *dest,
                                    VmValue::NativeBoundMethod(Arc::new(NativeBoundMethod {
                                        receiver: prim.clone(),
                                        method,
                                    })),
                                );
                            } else {
                                vm_err!(JadeError::UndefinedField {
                                    type_name: ty.type_name().to_string(),
                                    field: field.clone(),
                                    owner: FieldOwner::Value,
                                    span,
                                });
                            }
                        } else {
                            vm_err!(JadeError::NotAStruct { span });
                        }
                    }
                    // A function is an object: fn.name, fn.params.
                    VmValue::Fn(ref cf) => {
                        let v = match field.as_str() {
                            "name" => VmValue::Str(cf.chunk.name.clone().into()),
                            "params" => {
                                let arr: Vec<VmValue> = cf
                                    .params
                                    .iter()
                                    .map(|p| VmValue::Str(p.clone().into()))
                                    .collect();
                                VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(arr))))
                            }
                            _ => vm_err!(JadeError::UndefinedField {
                                type_name: "a function".to_string(),
                                field: field.clone(),
                                owner: FieldOwner::Value,
                                span,
                            }),
                        };
                        set(slots, *dest, v);
                    }
                    _ => {
                        vm_err!(JadeError::NotAStruct { span });
                    }
                }
            }
            Instr::SetField(obj_reg, field, val_reg) => {
                let val = get(slots, *val_reg).clone();
                let obj = get(slots, *obj_reg).clone();
                match obj {
                    VmValue::Struct(rc) => {
                        let error_type_name = {
                            let guard = rc.lock();
                            if guard.get_field(field.as_str()).is_some() {
                                None
                            } else {
                                Some(guard.type_name().to_string())
                            }
                        };
                        if let Some(type_name) = error_type_name {
                            vm_err!(JadeError::UndefinedField {
                                type_name,
                                field: field.clone(),
                                owner: FieldOwner::Struct,
                                span,
                            });
                        }
                        rc.lock().set_field(field, val);
                    }
                    _ => {
                        vm_err!(JadeError::NotAStruct { span });
                    }
                }
            }

            // ── FStr ──────────────────────────────────────────────────────────
            Instr::BuildFStr(dest, parts) => {
                // Literal segments come from source and are trusted; an
                // interpolated value contributes its own trust. The result is as
                // untrustworthy as the least trustworthy thing in it — otherwise
                // f"{tainted}" would launder taint exactly as `"" + tainted` did.
                let mut result = String::new();
                let mut trust = jade_runtime::trust::TRUSTED;
                for part in parts {
                    match part {
                        FStrPart::Literal(s) => result.push_str(s),
                        FStrPart::Reg(r) => {
                            let v = get(slots, *r);
                            if let VmValue::Str(s) = v {
                                trust = jade_runtime::trust::combine(trust, s.trust());
                            }
                            result.push_str(&value_to_display(v));
                        }
                    }
                }
                set(slots, *dest, VmValue::Str(JStr::with_trust(result, trust)));
            }

            // ── Prompt ────────────────────────────────────────────────────────
            Instr::MakePrompt(dest, text_reg) => {
                let text = match get(slots, *text_reg).clone() {
                    VmValue::Str(s) => s,
                    _ => {
                        vm_err!(JadeError::TypeError {
                            message: "prompt declaration requires a string body".to_string(),
                            span,
                        });
                    }
                };
                set(slots, *dest, VmValue::Prompt(text.to_string()));
            }
            Instr::Yield(src) => {
                let v = get(slots, *src).clone();
                match state.yield_stack.last() {
                    Some(buf) => buf.lock().push(v),
                    // Unreachable from source: the parser rejects a top-level
                    // `yield`, and every generator pushes a buffer before its
                    // body runs.
                    None => {
                        vm_err!(JadeError::YieldOutsideFunction { span });
                    }
                }
            }
            Instr::PromptDeref(dest, prompt_reg, output_type, grammar_reg) => {
                let text = match get(slots, *prompt_reg).clone() {
                    VmValue::Prompt(t) => t,
                    _ => {
                        vm_err!(JadeError::NotAPrompt { name: "<expr>".to_string(), span });
                    }
                };
                let grammar = match grammar_reg {
                    None => None,
                    Some(r) => match get(slots, *r).clone() {
                        VmValue::Grammar(g) => Some(g),
                        // A grammar expression that evaluated to nil (e.g.
                        // `self.grammar` before it was set) means no constraint,
                        // not an error.
                        VmValue::Nil => None,
                        other => vm_err!(JadeError::TypeError {
                            message: format!(
                                "|> constraint must be a Grammar value or type name, got {}",
                                value_type_name(&other)
                            ),
                            span,
                        }),
                    },
                };
                // Only a *type* stage collapses the stream, because a coerced
                // value cannot exist until generation finishes. A grammar stage
                // constrains how the reply is produced and leaves it a stream,
                // which is what replaced `stream(?p, mute_on=[g])`: printing it
                // streams live and mutes, reading it gives the full text.
                let result = match output_type.as_deref() {
                    None => vm_try!(vm_prompt_deref_stream(text, grammar.as_deref(), state, span)),
                    Some(ty) => {
                        let (gbnf, anchor, stop) = match &grammar {
                            Some(g) => (Some(g.to_gbnf()), g.anchor.clone(), g.stop.clone()),
                            None => (None, None, None),
                        };
                        vm_try!(
                            vm_prompt_deref(text, Some(ty), gbnf, anchor, stop, state, span).await
                        )
                    }
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
                    VmValue::Struct(rc) => rc.lock().type_name().to_string(),
                    _ => String::new(),
                };
                set(slots, *dest, VmValue::Str(name.into()));
            }

            // ── Async ─────────────────────────────────────────────────────────
            Instr::Spawn(dest, callee_reg, arg_regs) => {
                let callee = get(slots, *callee_reg).clone();
                let args: Vec<VmValue> = arg_regs.iter().map(|&r| get(slots, r).clone()).collect();
                let child_state = state.new_for_spawn();
                let handle = tokio::spawn(call_value_standalone(callee, args, child_state, span));
                set(
                    slots,
                    *dest,
                    VmValue::Future(Arc::new(JadeFuture { handle: Mutex::new(Some(handle)) })),
                );
            }
            Instr::Await(dest, future_reg) => {
                let fut_val = get(slots, *future_reg).clone();
                match fut_val {
                    VmValue::Future(jade_fut) => {
                        // SAFETY: .take() consumes the JoinHandle as an owned value before
                        // reaching .await, so the MutexGuard is dropped synchronously here —
                        // std::sync::MutexGuard is never held across an await point.
                        let handle = vm_try!(
                            jade_fut.handle.lock().take().ok_or(JadeError::DoubleAwait { span })
                        );
                        let join_result = handle.await;
                        let (task_result, child_raised) =
                            vm_try!(join_result.map_err(|e| JadeError::AsyncPanic {
                                message: e.to_string(),
                                span,
                            }));
                        if let Some(v) = child_raised {
                            state.raised_exception = Some(v);
                        }
                        let value = vm_try!(task_result);
                        set(slots, *dest, value);
                    }
                    _ => {
                        vm_err!(JadeError::NotAFuture { span });
                    }
                }
            }
            Instr::Join(dest, future_regs) => {
                let mut handles = Vec::with_capacity(future_regs.len());
                for &r in future_regs {
                    match get(slots, r).clone() {
                        VmValue::Future(jade_fut) => {
                            // SAFETY: same as Instr::Await — .take() is synchronous.
                            let handle = vm_try!(
                                jade_fut
                                    .handle
                                    .lock()
                                    .take()
                                    .ok_or(JadeError::DoubleAwait { span })
                            );
                            handles.push(handle);
                        }
                        _ => {
                            vm_err!(JadeError::NotAFuture { span });
                        }
                    }
                }
                let mut results = Vec::with_capacity(handles.len());
                for handle in handles {
                    let join_result = handle.await;
                    let (task_result, child_raised) = vm_try!(
                        join_result
                            .map_err(|e| JadeError::AsyncPanic { message: e.to_string(), span })
                    );
                    if let Some(v) = child_raised {
                        state.raised_exception = Some(v);
                    }
                    let value = vm_try!(task_result);
                    results.push(value);
                }
                set(
                    slots,
                    *dest,
                    VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(results)))),
                );
            }
        }
    }
    Ok(None)
}

#[inline]
pub(crate) fn get(slots: &[VmValue], r: Reg) -> &VmValue {
    // Registers outside the allocated range are treated as Nil; safe
    // because we size frames conservatively in execute_chunk.
    slots.get(r as usize).unwrap_or(&VmValue::Nil)
}

#[inline]
pub(crate) fn set(slots: &mut Vec<VmValue>, r: Reg, v: VmValue) {
    ensure_slot(slots, r);
    slots[r as usize] = v;
}

#[inline]
/// `b[i] = v` on a blob, for both write opcodes.
///
/// One place, because `SetIndex` and `SetIndexGlobal` are separate instructions
/// — the emitter picks between them by whether the binding is a local — and a
/// blob arm added to only one of them makes `b[0] = 1` work inside a function
/// and raise at the top level.
///
/// The index error is `IndexOutOfBounds`, matching the read side at
/// `vm::ops::vm_index`, and the octet-range wording comes from
/// `jade_runtime::bytesf` so the compiled backend raises the same sentence.
/// Complete any struct whose parent only arrived with an import.
///
/// The compiled backend never needs this: it inlines every module into one
/// stream before emitting, so a cross-file parent is already in reach and the
/// fields were folded in then. The VM has no such moment — `ImportFile` runs
/// during execution, after the importing program was emitted — so the same fold
/// happens here instead, and the two engines end up with the same struct.
///
/// Idempotent by name, exactly as the emitter's fold is: a field already present
/// is either the struct's own or one folded in earlier. A genuine clash was
/// refused at check time and cannot reach this.
fn resolve_imported_parents(state: &mut VmState) {
    let names: Vec<String> = state.struct_parents.keys().cloned().collect();
    for name in names {
        let parents = state.struct_parents.get(&name).cloned().unwrap_or_default();
        let Some(mut fields) = state.struct_defs.get(&name).cloned() else {
            continue;
        };
        let mut added: Vec<crate::frontend::ast::StructFieldDef> = Vec::new();
        let mut methods: Vec<(String, std::sync::Arc<CompiledFn>)> = Vec::new();
        let mut chain: Vec<String> = Vec::new();
        let mut queue = parents.clone();
        while let Some(p) = queue.first().cloned() {
            queue.remove(0);
            if chain.contains(&p) {
                continue;
            }
            chain.push(p.clone());
            queue.extend(state.struct_parents.get(&p).cloned().unwrap_or_default());
            for f in state.struct_defs.get(&p).into_iter().flatten() {
                let known = fields.iter().chain(added.iter()).any(|g| g.name() == f.name());
                if !known {
                    added.push(f.clone());
                }
            }
            for (m, cf) in state.extend_methods.get(&p).into_iter().flatten() {
                methods.push((m.clone(), std::sync::Arc::clone(cf)));
            }
        }
        if !added.is_empty() {
            added.append(&mut fields);
            state.struct_defs.insert(name.clone(), added);
        }
        if !methods.is_empty() {
            let own = state.extend_methods.entry(name.clone()).or_default();
            for (m, cf) in methods {
                own.entry(m).or_insert(cf);
            }
        }
        if !chain.is_empty() {
            state.struct_ancestors.insert(name, chain);
        }
    }
}

pub(crate) fn write_octet(
    arc: &Arc<Mutex<jade_runtime::bytesf::BytesObj>>,
    idx: &VmValue,
    val: &VmValue,
    span: Span,
) -> Result<()> {
    let VmValue::Int(i) = idx else {
        return Err(JadeError::TypeError {
            message: format!("bytes index must be int, got {}", value_type_name(idx)),
            span,
        });
    };
    let VmValue::Int(v) = val else {
        return Err(JadeError::TypeError {
            message: format!("bytes value must be int, got {}", value_type_name(val)),
            span,
        });
    };
    // One guard for the bounds check and the write; `parking_lot::Mutex` is not
    // reentrant, so a second `lock()` inside this scope would hang.
    let mut g = arc.lock();
    let len = g.len();
    if *i < 0 || *i as usize >= len {
        return Err(JadeError::IndexOutOfBounds { index: *i, len, span });
    }
    jade_runtime::bytesf::set(&mut g, *i, *v)
        .map_err(|message| JadeError::TypeError { message, span })
}

pub(crate) fn ensure_slot(slots: &mut Vec<VmValue>, r: Reg) {
    if r as usize >= slots.len() {
        slots.resize(r as usize + 1, VmValue::Nil);
    }
}

pub(crate) fn get_int(slots: &[VmValue], r: Reg, span: Span) -> Result<i64> {
    match get(slots, r) {
        VmValue::Int(i) => Ok(*i),
        _ => Err(JadeError::TypeError { message: "expected int".to_string(), span }),
    }
}

pub(crate) fn get_flt(slots: &[VmValue], r: Reg, span: Span) -> Result<f64> {
    match get(slots, r) {
        VmValue::Float(f) => Ok(*f),
        _ => Err(JadeError::TypeError { message: "expected float".to_string(), span }),
    }
}

pub(crate) fn get_bool(slots: &[VmValue], r: Reg, span: Span) -> Result<bool> {
    match get(slots, r) {
        VmValue::Bool(b) => Ok(*b),
        _ => Err(JadeError::TypeError { message: "expected bool".to_string(), span }),
    }
}

/// Read a string slot **with its trust**.
///
/// Use this anywhere the result is used to build another Jade string. Reading a
/// slot as a bare `String` and converting it back into a value marks it trusted
/// — which is exactly how `"" + tainted` laundered taint and let an untrusted
/// command reach `sh.exec`.
pub(crate) fn get_jstr(slots: &[VmValue], r: Reg, span: Span) -> Result<JStr> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.clone()),
        _ => Err(JadeError::TypeError { message: "expected str".to_string(), span }),
    }
}

/// Borrow a string slot by reference.  Use this when the caller only needs to
/// read the string (e.g. for comparisons) and does not need an owned `String`.
/// Avoids a heap allocation per comparison.
pub(crate) fn get_str_ref(slots: &[VmValue], r: Reg, span: Span) -> Result<&str> {
    match get(slots, r) {
        VmValue::Str(s) => Ok(s.as_str()),
        _ => Err(JadeError::TypeError { message: "expected str".to_string(), span }),
    }
}

pub(crate) fn int2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(i64, i64)> {
    Ok((get_int(slots, l, span)?, get_int(slots, r, span)?))
}

pub(crate) fn flt2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(f64, f64)> {
    Ok((get_flt(slots, l, span)?, get_flt(slots, r, span)?))
}

pub(crate) fn bool2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(bool, bool)> {
    Ok((get_bool(slots, l, span)?, get_bool(slots, r, span)?))
}

/// Borrow both string slots for comparison.  Returns `(&str, &str)` to avoid
/// cloning both `String`s when only an equality or ordering check is needed.
pub(crate) fn str2(slots: &[VmValue], l: Reg, r: Reg, span: Span) -> Result<(&str, &str)> {
    Ok((get_str_ref(slots, l, span)?, get_str_ref(slots, r, span)?))
}

/// Walk an instruction and return the highest register index it references.
/// Used to size the slots vec defensively in `execute_chunk`.
/// Evaluate a struct field default expression if it is a simple literal.
/// Returns None for non-literal defaults (they stay unset and will cause a
/// runtime error if accessed — the same behaviour as before this fix).
pub(crate) fn eval_literal_default(expr: &crate::frontend::ast::Expr) -> Option<VmValue> {
    use crate::frontend::ast::Expr;
    match expr {
        Expr::Str { value, .. } => Some(VmValue::Str(value.clone().into())),
        Expr::Integer { value, .. } => Some(VmValue::Int(*value)),
        Expr::Float { value, .. } => Some(VmValue::Float(*value)),
        Expr::Bool { value, .. } => Some(VmValue::Bool(*value)),
        Expr::Identifier { name, .. } if name == "nil" || name == "None" || name == "null" => {
            Some(VmValue::Nil)
        }
        Expr::Array { elements, .. } if elements.is_empty() => {
            Some(VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(vec![])))))
        }
        Expr::Dict { entries, .. } if entries.is_empty() => Some(VmValue::dict(DictObj::new())),
        _ => None,
    }
}

pub(crate) fn instr_max_reg(instr: &Instr) -> u32 {
    match instr {
        Instr::CatchMatches(d, s, _) => (*d).max(*s),
        Instr::SetIndexGlobal(_, i, v) => (*i).max(*v),
        Instr::LoadInt(d, _)
        | Instr::LoadFloat(d, _)
        | Instr::LoadBool(d, _)
        | Instr::LoadStr(d, _)
        | Instr::LoadNil(d)
        | Instr::LoadFn(d, _)
        | Instr::MakeClosure(d, _) => *d,
        Instr::GetLocal(d, _) | Instr::GetGlobal(d, _) => *d,
        Instr::Yield(s) => *s,
        Instr::Move(d, s)
        | Instr::NegInt(d, s)
        | Instr::NegFloat(d, s)
        | Instr::IntToFloat(d, s)
        | Instr::BitNot(d, s)
        | Instr::Not(d, s)
        | Instr::MakePrompt(d, s)
        | Instr::UnaryOp(d, _, s)
        | Instr::PromptDeref(d, s, _, None) => (*d).max(*s),
        Instr::PromptDeref(d, s, _, Some(g)) => (*d).max(*s).max(*g),
        Instr::SetGlobal(_, s) | Instr::SetLocal(_, s) => *s,
        Instr::AddInt(d, l, r)
        | Instr::SubInt(d, l, r)
        | Instr::MulInt(d, l, r)
        | Instr::DivInt(d, l, r)
        | Instr::ModInt(d, l, r)
        | Instr::AddFloat(d, l, r)
        | Instr::SubFloat(d, l, r)
        | Instr::MulFloat(d, l, r)
        | Instr::DivFloat(d, l, r)
        | Instr::ConcatStr(d, l, r)
        | Instr::BitAnd(d, l, r)
        | Instr::BitOr(d, l, r)
        | Instr::BitXor(d, l, r)
        | Instr::Shl(d, l, r)
        | Instr::Shr(d, l, r)
        | Instr::CmpEqInt(d, l, r)
        | Instr::CmpNeInt(d, l, r)
        | Instr::CmpLtInt(d, l, r)
        | Instr::CmpGtInt(d, l, r)
        | Instr::CmpLeInt(d, l, r)
        | Instr::CmpGeInt(d, l, r)
        | Instr::CmpEqFloat(d, l, r)
        | Instr::CmpNeFloat(d, l, r)
        | Instr::CmpLtFloat(d, l, r)
        | Instr::CmpGtFloat(d, l, r)
        | Instr::CmpLeFloat(d, l, r)
        | Instr::CmpGeFloat(d, l, r)
        | Instr::CmpLtIntFloat(d, l, r)
        | Instr::CmpGtIntFloat(d, l, r)
        | Instr::CmpLeIntFloat(d, l, r)
        | Instr::CmpGeIntFloat(d, l, r)
        | Instr::CmpLtFloatInt(d, l, r)
        | Instr::CmpGtFloatInt(d, l, r)
        | Instr::CmpLeFloatInt(d, l, r)
        | Instr::CmpGeFloatInt(d, l, r)
        | Instr::CmpEqBool(d, l, r)
        | Instr::CmpNeBool(d, l, r)
        | Instr::CmpLtBool(d, l, r)
        | Instr::CmpGtBool(d, l, r)
        | Instr::CmpLeBool(d, l, r)
        | Instr::CmpGeBool(d, l, r)
        | Instr::CmpEqStr(d, l, r)
        | Instr::CmpNeStr(d, l, r)
        | Instr::CmpLtStr(d, l, r)
        | Instr::CmpGtStr(d, l, r)
        | Instr::CmpLeStr(d, l, r)
        | Instr::CmpGeStr(d, l, r)
        | Instr::CmpEq(d, l, r)
        | Instr::CmpNe(d, l, r)
        | Instr::CmpLt(d, l, r)
        | Instr::CmpGt(d, l, r)
        | Instr::CmpLe(d, l, r)
        | Instr::CmpGe(d, l, r)
        | Instr::BinOp(d, _, l, r)
        | Instr::GetIndex(d, l, r) => (*d).max(*l).max(*r),
        Instr::GetField(d, o, _) => (*d).max(*o),
        Instr::SetIndex(o, i, v) => (*o).max(*i).max(*v),
        Instr::SetField(o, _, v) => (*o).max(*v),
        Instr::JumpIfFalse(c, _) | Instr::JumpIfTrue(c, _) => *c,
        Instr::Jump(_)
        | Instr::Halt
        | Instr::Return(None)
        | Instr::ImportFile(_, _)
        | Instr::ImportFrom(_, _) => 0,
        Instr::Return(Some(r)) => *r,
        Instr::Call(d, c, args) => {
            let mut m = (*d).max(*c);
            for &a in args {
                m = m.max(a);
            }
            m
        }
        Instr::MakeArray(d, regs) | Instr::MakeArrayArena(d, regs) => {
            let mut m = *d;
            for &r in regs {
                m = m.max(r);
            }
            m
        }
        Instr::ArenaMark(d) => *d,
        Instr::ArenaReset(r) => *r,
        Instr::MakeDict(d, pairs) => {
            let mut m = *d;
            for &(k, v) in pairs {
                m = m.max(k).max(v);
            }
            m
        }
        Instr::MakeStruct(d, _, fields) => {
            let mut m = *d;
            for (_, r, _) in fields {
                m = m.max(*r);
            }
            m
        }
        Instr::BuildFStr(d, parts) => {
            let mut m = *d;
            for p in parts {
                if let FStrPart::Reg(r) = p {
                    m = m.max(*r);
                }
            }
            m
        }
        Instr::Raise(r) => *r,
        Instr::SetupHandler(r, _) => *r,
        Instr::PopHandler => 0,
        Instr::GetTypeName(d, s) => (*d).max(*s),
        Instr::Spawn(d, c, args) => {
            let mut m = (*d).max(*c);
            for &a in args {
                m = m.max(a);
            }
            m
        }
        Instr::Await(d, s) => (*d).max(*s),
        Instr::Join(d, regs) => {
            let mut m = *d;
            for &r in regs {
                m = m.max(r);
            }
            m
        }
        Instr::CallNamed(d, c, pairs) => {
            let mut m = (*d).max(*c);
            for (_, r) in pairs {
                m = m.max(*r);
            }
            m
        }
    }
}
