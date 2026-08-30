//! Builtin dispatch: native packages, stdlib modules, and dynamic methods.
//!
//! See this directory's README.

use super::*;

/// Reserved global names that resolve to a runtime builtin (not a user value),
/// mirroring `crate::builtins::seed_globals`. A `Call` whose callee is
/// `GetGlobal(<one of these>)` and which this backend does not itself lower is a
/// builtin call we can't emit — the whole program falls back to `expr.rs`. (A
/// user function that shadows one of these names is bound via `SetGlobal`, so it
/// is tracked as a known function and takes precedence over this check.)
pub(super) const RESERVED_BUILTINS: &[&str] = &[
    // core + native + type-constructor globals
    "write", "len", "input", "print", "route", "int", "float", "bool", "str", "char", "func",
    "Grammar",
    // stdlib package globals (accessed via `use`; a bare call is invalid, but
    // reserving them keeps a stray Call from mis-lowering to an indirect call)
    "llm", "string", "math", "array", "dict", "fs", "time", "http", "uhttp", "sh", "json", "env",
    "path", "random", "bytes",
];

/// Reject a program that reads a global nothing ever binds.
///
/// A `GetGlobal` of an unbound name has no lowering to decline — `global_slot`
/// happily creates the cell, initialized to nil, and the program builds. Calling
/// one then traps in the runtime with no message at all, so a name that does not
/// exist produces a binary that compiles clean and dies silently. The VM has
/// always raised `undefined variable` for the same program, so this is also the
/// two engines agreeing again.
///
/// Type inference catches most of these first. What reaches here is the case it
/// has to stay lenient about: a file importing a user module, where a bare name
/// may well be one of that module's globals. By this point every import is
/// inlined, so the question is finally answerable — the program's own bindings
/// plus the runtime's are all there is.
///
/// Deliberately conservative. Only `GetGlobal` is checked, only against names
/// nothing anywhere in the program stores to, and native refs are exempt because
/// they resolve through `jrt_native_call` rather than a cell.
pub(super) fn check_globals_bound(
    top: &Chunk,
    defs: &[Arc<CompiledFn>],
    struct_defs: &HashMap<String, Vec<StructFieldDef>>,
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
) -> Result<(), String> {
    let mut bound: std::collections::HashSet<String> = std::collections::HashSet::new();

    // The runtime's own globals, read from `seed_globals` rather than restated,
    // so a new builtin cannot make valid programs stop building.
    let mut seeded: HashMap<String, VmValue> = HashMap::new();
    crate::builtins::seed_globals(&mut seeded);
    bound.extend(seeded.into_keys());

    // A type name reaches here as a `GetGlobal` that nothing ever stores to —
    // it lives in these side tables, not in the instruction stream. Counting it
    // as bound is also what leaves the better diagnostic in place: calling one
    // has its own error saying a struct is not a function.
    bound.extend(struct_defs.keys().cloned());
    bound.extend(extend_methods.keys().cloned());

    let chunks = std::iter::once(top).chain(defs.iter().map(|f| &f.chunk));
    for c in chunks.clone() {
        for instr in &c.code {
            if let Instr::SetGlobal(name, _) = instr {
                bound.insert(name.clone());
            }
        }
    }

    for c in chunks {
        for (i, instr) in c.code.iter().enumerate() {
            let Instr::GetGlobal(_, name) = instr else { continue };
            if bound.contains(name)
                || parse_native_ref(name).is_some()
                || crate::builtins::is_package_global_name(name)
            {
                continue;
            }
            let (line, col) = c.spans.get(i).map_or((0, 0), |s| (s.line, s.col));
            return Err(format!(
                "[{line}:{col}] undefined variable '{name}'{}",
                crate::frontend::error::undefined_variable_hint(name)
            ));
        }
    }
    Ok(())
}

/// Parse a native package reference `__native$<pkgid>$<fn>` (produced by import
/// namespacing) into `(pkgid, fn_name)`. `None` for ordinary names.
pub(super) fn parse_native_ref(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("__native$")?;
    let (id, fname) = rest.split_once('$')?;
    Some((id.parse().ok()?, fname))
}

/// Stdlib module namespaces whose `module.method(...)` calls the Chunk backend can
/// recognize (a `GetField` whose base was `GetGlobal`'d from one of these names is
/// a module call, not a struct/primitive method).
pub(super) fn is_stdlib_module(name: &str) -> bool {
    matches!(
        name,
        "math"
            | "json"
            | "llm"
            | "path"
            | "time"
            | "env"
            | "fs"
            | "random"
            | "http"
            | "uhttp"
            | "sh"
            | "dict"
            | "array"
            | "string"
            | "bytes"
            | "Grammar"
    )
}

/// Whether `module.method(recv, …)` is exactly `recv.method(…)`, so the package
/// spelling can lower as the method.
///
/// **Only where the two really are the same function.** `std/string` and
/// `std/dict` expose each of theirs both ways and both are pure, so the spelling
/// carries no meaning. `std/array` does *not*: its package functions are the
/// functional style, and `array.sort(a)` returns a sorted copy where `a.sort()`
/// sorts in place. Routing those two together would have quietly changed what a
/// compiled program does, which is why this asks the package table for the name
/// rather than assuming any `module.method` with a receiver first is the method.
///
/// The table is the authority for a second reason: `len` is a method on both
/// types and a package function on neither, so `dict.len(d)` is not a call the
/// interpreter accepts either. It reads the length of the *namespace* — a
/// package is a dict at run time — and reports `expected 0, got 1`. Accepting it
/// here would have built a program `jade run` refuses.
fn package_fn_is_the_method(module: &str, method: &str) -> bool {
    let import = match module {
        "string" => "std/string",
        "dict" => "std/dict",
        _ => return false,
    };
    crate::builtins::find_package(import).is_some_and(|p| p.fns.iter().any(|f| f.name == method))
}

/// The receiver and remaining arguments of a package-form collection call.
fn as_receiver_first<'a>(module: &str, method: &str, args: &'a [Reg]) -> Option<(Reg, &'a [Reg])> {
    if !package_fn_is_the_method(module, method) {
        return None;
    }
    let (&recv, rest) = args.split_first()?;
    let supported = chunk_str_method_supported(method, rest.len())
        || chunk_val_method_supported(method, rest.len());
    supported.then_some((recv, rest))
}

/// Whether the Chunk backend can lower `module.method` with `argc` explicit args.
///
/// Keep in lockstep with `emit_module_call`. A name this answers `false` for is
/// a hard build error, so the set has to match what `jade run` accepts — the two
/// engines disagreeing about whether a program is valid at all is the failure
/// this predicate is most likely to cause, and it is how `string.upper(s)` came
/// to run under `jade run` and refuse to build until v1.3.21.
pub(super) fn chunk_module_supported(module: &str, method: &str, argc: usize) -> bool {
    // The package spelling of a collection method, which lowers as the method.
    if argc >= 1 && package_fn_is_the_method(module, method) {
        let rest = argc - 1;
        if chunk_str_method_supported(method, rest) || chunk_val_method_supported(method, rest) {
            return true;
        }
    }
    match (module, method) {
        ("math", "floor" | "ceil" | "abs" | "sqrt") => argc == 1,
        ("math", "min" | "max" | "pow") => argc == 2,
        // Rounding, sign, the transcendentals, and the two predicates. None can
        // fail, so each calls the Rust symbol directly with no error drain.
        (
            "math",
            "round" | "trunc" | "sign" | "ln" | "log2" | "log10" | "exp" | "sin" | "cos" | "tan"
            | "asin" | "acos" | "atan" | "is_nan" | "is_inf",
        ) => argc == 1,
        ("math", "atan2" | "hypot") => argc == 2,
        ("math", "clamp") => argc == 3,
        // The constants, spelled as calls — see `mathf::pi`.
        ("math", "pi" | "e" | "tau" | "inf" | "nan") => argc == 0,
        ("path", "basename" | "ext" | "dirname" | "stem" | "abs") => argc == 1,
        ("path", "is_abs") => argc == 1,
        ("path", "join") => argc >= 1,
        ("fs", "read") => argc == 1 || argc == 2, // read(path) or read(path, trust)
        ("fs", "exists" | "delete" | "mkdir") => argc == 1,
        ("fs", "is_file" | "is_dir" | "rmdir" | "size") => argc == 1,
        ("fs", "copy" | "rename") => argc == 2,
        ("fs", "write" | "append") => argc == 2,
        ("fs", "read_bytes") => argc == 1 || argc == 2,
        ("fs", "write_bytes" | "append_bytes") => argc == 2,
        ("fs", "read_stdin_bytes") => argc == 0,
        ("fs", "write_stdout_bytes") => argc == 1,
        ("fs", "list_dir") => argc == 1,
        ("sh", "exec" | "run" | "output") => argc == 1,
        ("random", "choice" | "shuffle") => argc == 1,
        ("env", "cwd") => argc == 0,
        ("env", "get") => argc == 1,
        ("env", "set") => argc == 2,
        ("env", "args") => argc == 0,
        ("time", "now" | "now_ms" | "monotonic") => argc == 0,
        ("time", "sleep") => argc == 1,
        ("time", "local" | "utc" | "parts") => argc == 1,
        // stamp(y, mo, d[, h[, mi[, s]]]) — time of day defaults to midnight.
        ("time", "stamp") => (3..=6).contains(&argc),
        ("http", "get" | "delete" | "head" | "get_bytes") => argc == 1 || argc == 2,
        ("http", "post" | "put" | "post_bytes") => argc == 2 || argc == 3,
        ("uhttp", "get" | "delete" | "head" | "get_bytes") => argc == 1 || argc == 2,
        ("uhttp", "post" | "put" | "post_bytes") => argc == 2 || argc == 3,
        ("uhttp", "stream") => argc == 2 || argc == 3, // url, handler[, headers]
        ("array", "map" | "filter") => argc == 2,
        // The functional style: a sorted/reversed copy, not the in-place method.
        ("array", "sort" | "reverse") => argc == 1,
        // Unlike sort/reverse, `join` is pure — the two spellings really are the
        // same function, so this lowers to the symbol the method uses. It gets
        // an explicit arm rather than going through `package_fn_is_the_method`
        // because that bridge deliberately excludes `std::array`, whose other
        // package functions are *not* their methods.
        ("array", "join") => argc == 2,
        ("random", "int") => argc == 2,
        ("random", "seed") => argc == 1,
        ("random", "float") => argc == 0,
        ("dict", "merge") => argc == 2,
        ("json", "parse" | "stringify" | "stringify_pretty") => argc == 1,
        ("Grammar", "new") => (1..=3).contains(&argc), // pattern[, anchor[, stop]]
        ("bytes", "zeros" | "from_ints") => argc == 1,
        ("bytes", "concat") => argc == 2,
        _ => false,
    }
}

/// Whether `method` is a string-only primitive method the Chunk path lowers via
/// the shared `jrt_str_*` symbol (the receiver is unambiguously a string). These
/// names don't belong to dict/array, so no runtime kind dispatch is needed.
/// `contains` (also a dict method) and `split` (returns a collection) are excluded.
pub(super) fn chunk_str_method_supported(method: &str, argc: usize) -> bool {
    match method {
        "trim" | "upper" | "lower" | "encode" => argc == 0,
        "trim_start" | "trim_end" | "capitalize" | "is_empty" => argc == 0,
        "lines" => argc == 0,
        "starts_with" | "ends_with" => argc == 1,
        "index_of" | "last_index_of" | "count" | "repeat" => argc == 1,
        "replace" => argc == 2,
        "pad_start" | "pad_end" => argc == 2,
        "split" => argc == 1,
        _ => false,
    }
}

/// Whether `method` is an array/dict primitive method unique to one collection
/// kind (so the receiver's kind is known by the method name — the frontend has
/// type-checked it). `contains` (str/array/dict) and `len` (all) are excluded.
pub(super) fn chunk_val_method_supported(method: &str, argc: usize) -> bool {
    match method {
        "push" => argc == 1,                     // array
        "pop" | "sort" | "reverse" => argc == 0, // array
        "map" | "filter" => argc == 1,           // array
        "join" => argc == 1,                     // array
        "keys" | "values" => argc == 0,          // dict
        "has" | "get" => argc == 1,              // dict
        "contains" => argc == 1,                 // str / array (runtime-dispatched)
        "len" => argc == 0,                      // str / array / dict / bytes (runtime-dispatched)
        "ready" => argc == 0,                    // future
        "decode" => argc == 0,                   // bytes
        "slice" => argc == 2,                    // bytes
        _ => false,
    }
}

/// Emit the path a method call takes when the call site's static guess about
/// the receiver did not hold.
///
/// Two ways it can miss, and they need different answers. The receiver may be
/// another *struct* — a different type, or one that inherits the method — which
/// dispatches on its runtime type. Or it may not be a struct at all, which
/// happens when a struct declares a method named like a built-in one:
/// resolution reaches the struct's `contains` first, and then somebody calls
/// `[1, 2].contains(x)`. That one wants the primitive method, which is lowered
/// inline and so has to be chosen here rather than in the runtime.
pub(super) fn emit_method_fallback<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
    prim: Option<PrimFallback>,
) -> Result<IntValue<'ctx>, String> {
    let Some(kind) = prim else {
        return emit_dynamic_method(low, recv, method, args);
    };
    let b = low.builder;
    let i64_ty = low.i64t();
    let i32_ty = low.ctx.i32_type();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    let isf = low.runtime_fn("jrt_is_struct", i32_ty.fn_type(&[i64_ty.into()], false));
    let flag = b
        .build_call(isf, &[low.load(recv).into()], "isstruct")
        .map_err(err)?
        .as_any_value_enum()
        .into_int_value();
    let cond = b
        .build_int_compare(inkwell::IntPredicate::NE, flag, i32_ty.const_zero(), "isstructb")
        .map_err(err)?;
    let cur = b.get_insert_block().unwrap().get_parent().unwrap();
    let struct_bb = low.ctx.append_basic_block(cur, "mf_struct");
    let prim_bb = low.ctx.append_basic_block(cur, "mf_prim");
    let merge_bb = low.ctx.append_basic_block(cur, "mf_merge");
    b.build_conditional_branch(cond, struct_bb, prim_bb).map_err(err)?;

    b.position_at_end(struct_bb);
    let dyn_ret = emit_dynamic_method(low, recv, method, args)?;
    b.build_unconditional_branch(merge_bb).map_err(err)?;
    let struct_end = b.get_insert_block().unwrap();

    b.position_at_end(prim_bb);
    let prim_ret = match kind {
        PrimFallback::Str => crate::codegen::strings::emit_str_method(low, recv, method, args)?,
        PrimFallback::Val => emit_val_method(low, recv, method, args)?,
    };
    b.build_unconditional_branch(merge_bb).map_err(err)?;
    let prim_end = b.get_insert_block().unwrap();

    b.position_at_end(merge_bb);
    let phi = b.build_phi(i64_ty, "mfret").map_err(err)?;
    phi.add_incoming(&[(&dyn_ret, struct_end), (&prim_ret, prim_end)]);
    Ok(phi.as_basic_value().into_int_value())
}

/// Emit a runtime-dispatched method call `recv.method(args)`.
///
/// Used for a genuinely-ambiguous method name (the same name and arity on more
/// than one type), and as the fallback when a devirtualized call site's type
/// guard fails.
///
/// The whole resolution is one runtime call. It used to be built here — a fresh
/// tagged type-name string (allocated per call, never freed), a registry lookup,
/// then a jump through whatever came back, including null. That jump was an
/// uncatchable segfault whenever the receiver's type had no such method, and
/// going to the registry at all meant a *data field* holding a function lost to
/// another type's method of the same name, which the interpreter resolves the
/// other way round.
pub(super) fn emit_dynamic_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // Entry-block buffer, not an `alloca` here: this can sit inside a loop.
    let argv = if args.is_empty() {
        ptrt.const_null()
    } else {
        let buf = low.entry_buf("dmargv", args.len())?;
        for (i, a) in args.iter().enumerate() {
            let slot = unsafe {
                b.build_in_bounds_gep(i64_ty, buf, &[i64_ty.const_int(i as u64, false)], "dma")
                    .map_err(err)?
            };
            b.build_store(slot, low.load(*a)).map_err(err)?;
        }
        buf
    };

    let f = low.runtime_fn(
        "jrt_method_fallback",
        i64_ty.fn_type(&[i64_ty.into(), ptrt.into(), i64_ty.into(), ptrt.into()], false),
    );
    Ok(b.build_call(
        f,
        &[
            low.load(recv).into(),
            low.cstr(method).into(),
            i64_ty.const_int(args.len() as u64, false).into(),
            argv.into(),
        ],
        "dmcall",
    )
    .map_err(err)?
    .as_any_value_enum()
    .into_int_value())
}

/// Load a native package's `dlopen` handle from its `native_pkg$<pkgid>` global.
/// `compile()` creates + fills this in `main`'s prologue for the real module; it
/// is created lazily (nil) if missing so the throwaway probe module also lowers.
pub(super) fn native_pkg_global<'ctx>(low: &Lowerer<'_, 'ctx>, pkgid: u32) -> GlobalValue<'ctx> {
    let gname = format!("native_pkg${pkgid}");
    low.module.get_global(&gname).unwrap_or_else(|| {
        let g = low.module.add_global(low.ptrt(), None, &gname);
        g.set_initializer(&low.ptrt().const_null());
        g.set_linkage(inkwell::module::Linkage::Internal);
        g
    })
}

/// Load a package's handle out of that cell, at the point of use.
pub(super) fn native_pkg_handle<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    pkgid: u32,
) -> Result<PointerValue<'ctx>, String> {
    low.builder
        .build_load(low.ptrt(), native_pkg_global(low, pkgid).as_pointer_value(), "nhandle")
        .map_err(|e| e.to_string())
        .map(|v| v.into_pointer_value())
}

/// Emit a direct native (C-ABI) call `__native$<pkgid>$<fname>(args)`: marshal the
/// (already-tagged) args into a stack array and dispatch through `jrt_native_call`.
/// The result is a tagged word (native output strings are TAINTED); it is used
/// directly (no reinterpret — the Chunk path is uniformly tagged). Can raise.
pub(super) fn emit_native_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    pkgid: u32,
    fname: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    let handle = native_pkg_handle(low, pkgid)?;
    let argv = if args.is_empty() {
        ptrt.const_null()
    } else {
        // Entry-block buffer, not an alloca here: this call can sit inside a
        // loop. See `Lowerer::entry_buf`.
        let arr = low.entry_buf("nargv", args.len())?;
        for (i, r) in args.iter().enumerate() {
            let slot = unsafe {
                b.build_in_bounds_gep(i64_ty, arr, &[i64_ty.const_int(i as u64, false)], "na")
                    .map_err(err)?
            };
            b.build_store(slot, low.load(*r)).map_err(err)?;
        }
        arr
    };
    let name_ptr = b.build_global_string_ptr(fname, "nfname").map_err(err)?.as_pointer_value();
    let call_fn = low.runtime_fn(
        "jrt_native_call",
        i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
    );
    Ok(b.build_call(
        call_fn,
        &[
            handle.into(),
            name_ptr.into(),
            argv.into(),
            i64_ty.const_int(args.len() as u64, false).into(),
        ],
        "ncall",
    )
    .map_err(err)?
    .as_any_value_enum()
    .into_int_value())
}

/// Materialize a first-class native function value: `{ fn_ptr@0, kind@8, env@16 }`
/// where `fn_ptr` is the `jrt_native_call` address used purely as a sentinel (a
/// real `jf_<uid>` box never holds this symbol). `indirect_call` recognizes the
/// sentinel and routes through `jrt_native_call`. Returned as a `TAG_PTR` word.
///
/// The `kind` word at offset 8 holds `ObjKind::Fn`, aligned with
/// `ObjHeader.kind`, exactly as `fn_box_word` does for ordinary functions. It is
/// load-bearing, not decorative: without it, offset 8 held the `env` *pointer*,
/// so `jrt_decref` reading an `ObjKind` there would interpret a heap address's
/// low byte as a kind — and a byte that happened to be 2/3/4 would send
/// `free_obj` off to `Box::from_raw` the value as an Array/Dict/Struct. That
/// hazard is the reason native refs used to veto refcounting for the whole
/// program.
///
/// The third slot used to hold `name`, which nothing ever read (`env` already
/// carries it), so the kind word costs no extra space.
///
/// ## Both objects are link-time constants, not allocations
///
/// This used to `malloc` the box and its env on every evaluation, on the stated
/// assumption that a reference immediately called would be dead-code-eliminated.
/// It is not: `GetGlobal` stores the tagged word into the register-file alloca,
/// which is dead to Jade but live to LLVM, so both allocations stayed in the
/// loop body next to the call. Nothing freed them either — the `ObjKind::Fn` at
/// offset 8 is exactly what makes `is_collection` false and `jrt_decref` return
/// early — so a compiled binary leaked 48 bytes per FFI call, growing without
/// bound, while `jade run` did not.
///
/// The value has no identity and no mutable state: it is a pure function of
/// `(pkgid, fname)`. So there is one of each per binding, emitted once as an
/// internal constant and shared by every evaluation. `set_constant` is not
/// decoration — nothing may write to these, and a linker-enforced fault is a
/// better outcome than silent corruption of an object the whole program shares.
/// 8-byte alignment is required, because `TAG_PTR` lives in the low three bits
/// and `untag_ptr` masks them off.
///
/// **`env` holds the *address of* the package's handle cell, not the handle.**
/// The handle is a `dlopen` result that only exists once `jade_mod_init` has
/// run, so it cannot appear in a static initializer. Storing it into the global
/// on each evaluation would be a data race between concurrent tasks — benign in
/// practice, undefined in the abstract machine, and a real report under TSan —
/// and it would create an initialization-order rule for someone to break later.
/// Pointing at `@native_pkg$<pkgid>` instead leaves nothing to initialize: the
/// env is correct before `main` starts, and `indirect_call` pays one extra load
/// to reach through it.
pub(super) fn emit_native_fn_value<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    pkgid: u32,
    fname: &str,
) -> Result<IntValue<'ctx>, String> {
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let boxname = format!("native_fnval${pkgid}${fname}");
    if let Some(g) = low.module.get_global(&boxname) {
        return Ok(low.tag_ptr(g.as_pointer_value()));
    }

    // env = { &@native_pkg$<pkgid>, "<fname>" }
    let name_ptr = low
        .builder
        .build_global_string_ptr(fname, &format!("nfname${pkgid}${fname}"))
        .map_err(|e| e.to_string())?
        .as_pointer_value();
    let env_ty = low.ctx.struct_type(&[ptrt.into(), ptrt.into()], false);
    let env = low.module.add_global(env_ty, None, &format!("native_env${pkgid}${fname}"));
    env.set_initializer(&env_ty.const_named_struct(&[
        native_pkg_global(low, pkgid).as_pointer_value().into(),
        name_ptr.into(),
    ]));
    env.set_linkage(inkwell::module::Linkage::Internal);
    env.set_constant(true);
    env.set_alignment(8);

    // fn value = { sentinel, kind, env }
    let sentinel = low
        .runtime_fn(
            "jrt_native_call",
            i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into(), i64_ty.into()], false),
        )
        .as_global_value()
        .as_pointer_value();
    let box_ty = low.ctx.struct_type(&[ptrt.into(), i64_ty.into(), ptrt.into()], false);
    let fnval = low.module.add_global(box_ty, None, &boxname);
    fnval.set_initializer(&box_ty.const_named_struct(&[
        sentinel.into(),
        i64_ty.const_int(OBJKIND_FN | (OBJ_FN_NATIVE << 8), false).into(),
        env.as_pointer_value().into(),
    ]));
    fnval.set_linkage(inkwell::module::Linkage::Internal);
    fnval.set_constant(true);
    fnval.set_alignment(8);
    Ok(low.tag_ptr(fnval.as_pointer_value()))
}

/// Emit an array/dict primitive method `recv.method(args)` via the ObjHeader-aware
/// `jrt_karr_*`/`jrt_coll_*` helpers. The receiver kind is implied by the method
/// name (`chunk_val_method_supported`). Array push/pop/sort/reverse mutate in
/// place (push/sort/reverse → nil, pop → element); dict keys/values build a new
/// array, has → bool, get → value-or-nil.
pub(super) fn emit_val_method<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    recv: Reg,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let void_ty = low.ctx.void_type();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // Guard the receiver before anything dereferences it. `len` and `contains`
    // are absent on purpose: they hand the whole tagged word to `jrt_len_chunk`
    // / `jrt_in_any`, which dispatch on the tag themselves and are already safe
    // on a scalar. Every other arm untags to a pointer and trusts the kind.
    match method {
        "push" | "pop" | "sort" | "reverse" | "map" | "filter" | "join" => {
            low.require_kind(low.load(recv), WANT_ARRAY, method)?
        }
        "keys" | "values" | "has" | "get" => low.require_kind(low.load(recv), WANT_DICT, method)?,
        // `decode` is a blob method only; `slice` belongs to a blob *and* a
        // string. Both hand the runtime the whole tagged word, so without a
        // guard here `{"a": 1}.slice(0, 1)` read a `DictObj` as a `BytesObj`
        // and took the process down, and `[1].decode()` returned a blob from
        // an array's bytes. The VM raises "<kind> has no method" for both.
        "decode" => low.require_kind(low.load(recv), WANT_BYTES, method)?,
        "slice" => low.require_kind(low.load(recv), WANT_STR | WANT_BYTES, method)?,
        _ => {}
    }
    // `map`/`filter` hand their argument to the runtime, which calls it. A word
    // that is not a function value gets dereferenced there, so it is checked
    // here for the same reason an indirect call checks its callee.
    match method {
        // Deliberately not checked here. `jrt_call_value` checks each time it
        // enters the callback, so an eager check only changes what happens on an
        // *empty* array — where the interpreter never looks at the callback at
        // all and answers `[]`. Checking here made `[].map(5)` raise compiled
        // and return `[]` interpreted.
        "map" | "filter" => {}
        _ => {}
    }

    let recv_p = low.untag_ptr(low.load(recv));
    let nil = i64_ty.const_int(NIL, false);

    match method {
        // ── array, taking a function ──────────────────────────────────────
        // `a.map(f)` is `array.map(a, f)`, and both reach the same symbol.
        // Only the package spelling worked until v1.3.21, which made these the
        // one pair of array functions with a single spelling.
        "map" | "filter" => {
            let cname =
                if method == "map" { "jrt_coll_array_map" } else { "jrt_coll_array_filter" };
            let f = low.runtime_fn(cname, i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(recv).into(), low.load(args[0]).into()], "mapf")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        // (arr, sep) -> a tagged string. Trust is the union of the parts', so
        // joining a tainted element into a literal separator does not launder
        // it — the runtime does that fold, not this arm.
        "join" => {
            let f = low.runtime_fn(
                "jrt_coll_array_join",
                ptrt.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let sep = low.untag_ptr(low.load(args[0]));
            let r = b
                .build_call(f, &[recv_p.into(), sep.into()], "join")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        // ── bytes ─────────────────────────────────────────────────────────
        // Both untag the receiver to a BytesObj pointer. `decode` goes through
        // the raising C wrapper rather than the Rust entry point directly: a
        // Jade raise is a longjmp and must not unwind through a Rust frame.
        "decode" => {
            let f = low.runtime_fn("jk_bytes_decode", i64_ty.fn_type(&[i64_ty.into()], false));
            let r = b
                .build_call(f, &[low.load(recv).into()], "dec")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(r)
        }
        // The one method name that does not settle its receiver's kind: a blob
        // and a string both have `slice`. So the whole tagged word goes to the
        // runtime, which reads the tag and picks — the same treatment `len` and
        // `contains` already get, and for the same reason. Picking here from
        // the method name would read a tagged string as a `BytesObj` pointer.
        "slice" => {
            let f = low.runtime_fn(
                "jrt_slice_any",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into(), i64_ty.into()], false),
            );
            let s = low.untag_int(low.load(args[0]));
            let e = low.untag_int(low.load(args[1]));
            Ok(b.build_call(f, &[low.load(recv).into(), s.into(), e.into()], "slice")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        "push" => {
            let f = low
                .runtime_fn("jrt_karr_push", void_ty.fn_type(&[ptrt.into(), i64_ty.into()], false));
            b.build_call(f, &[recv_p.into(), low.load(args[0]).into()], "").map_err(err)?;
            Ok(nil)
        }
        "pop" => {
            let f = low.runtime_fn("jrt_coll_array_pop", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[recv_p.into()], "pop")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        "sort" | "reverse" => {
            let cname =
                if method == "sort" { "jrt_coll_array_sort" } else { "jrt_coll_array_reverse" };
            let f = low.runtime_fn(cname, void_ty.fn_type(&[ptrt.into()], false));
            b.build_call(f, &[recv_p.into()], "").map_err(err)?;
            Ok(nil)
        }
        "keys" | "values" => {
            let cname =
                if method == "keys" { "jrt_coll_dict_keys" } else { "jrt_coll_dict_values" };
            let f = low.runtime_fn(cname, ptrt.fn_type(&[ptrt.into()], false));
            let p = b
                .build_call(f, &[recv_p.into()], "kv")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        "ready" => {
            // `f.ready()` — has the task finished, without waiting for it. The
            // guard first, so a receiver that is not a future raises the
            // interpreter's wording rather than reaching the forwarder.
            low.require_kind(low.load(recv), WANT_FUTURE, "ready")?;
            let f = low.runtime_fn("jade_future_ready", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(recv).into()], "ready")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        "len" => {
            // `recv.len()` == `len(recv)`: jrt_len_chunk tag-dispatches str
            // (byte length) / collection (ObjHeader.len) at runtime → tagged int.
            let f = low.runtime_fn("jrt_len_chunk", i64_ty.fn_type(&[i64_ty.into()], false));
            let n = b
                .build_call(f, &[low.load(recv).into()], "len")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(low.tag_int(n))
        }
        "contains" => {
            // `haystack.contains(needle)` == `needle in haystack`: jrt_in_any
            // dispatches str (substring) / array (element eq) at runtime.
            let f = low
                .runtime_fn("jrt_in_any", i32_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            let r = b
                .build_call(f, &[low.load(args[0]).into(), low.load(recv).into()], "cont")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "c")
                .map_err(err)?;
            Ok(low.bool_word(bit))
        }
        "has" | "get" => {
            // jrt_coll_dict_get(dict, key_cstr, *out) -> found?  (key arg is a
            // tagged string → its data pointer).
            let f = low.runtime_fn(
                "jrt_coll_dict_get",
                i32_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let out = b.build_alloca(i64_ty, "dget").map_err(err)?;
            let key_p = low.untag_ptr(low.load(args[0]));
            let found = b
                .build_call(f, &[recv_p.into(), key_p.into(), out.into()], "has")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            let bit = b
                .build_int_compare(inkwell::IntPredicate::NE, found, i32_ty.const_zero(), "f")
                .map_err(err)?;
            if method == "has" {
                Ok(low.bool_word(bit))
            } else {
                // get: found ? *out : nil.
                let val = b.build_load(i64_ty, out, "getval").map_err(err)?.into_int_value();
                let res = b.build_select(bit, val, nil, "getornil").map_err(err)?.into_int_value();
                // `jrt_coll_dict_get` writes the value word out **borrowed** —
                // the dict still owns it — so the destination slot becomes a
                // second owner and the count must rise. `GetIndex` retains for
                // exactly this reason; `get` did not, so each call decremented
                // the entry on scope exit and a nested `TABLE.get(k)["f"]` read
                // correct values twice, then double-freed the inner collection
                // and raised "key not found" off the freed memory. Retaining
                // nil is a no-op, so the not-found arm needs no special case.
                low.retain(res);
                Ok(res)
            }
        }
        _ => Err(format!("codegen: emit_val_method: unhandled {method}")),
    }
}

/// Emit a stdlib `module.method(args)` call, returning the tagged result word.
/// Only calls `chunk_module_supported` accepts reach here; each reuses the same
/// runtime `jrt_*` symbol the interpreter's implementation calls. String returns
/// are null-checked → tagged nil (covers `path.ext`/`env.get`); scalar returns
/// tag directly.
pub(super) fn emit_module_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    module: &str,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
    // `string.upper(s)` is `s.upper()`, so it lowers as one. Before the two
    // spellings anything else, because a collection method reached this far only
    // by being written the other way round.
    if let Some((recv, rest)) = as_receiver_first(module, method, args) {
        return if chunk_str_method_supported(method, rest.len()) {
            emit_str_method(low, recv, method, rest)
        } else {
            emit_val_method(low, recv, method, rest)
        };
    }

    let b = low.builder;
    let i64_ty = low.i64t();
    let ptrt = low.ptrt();
    let i32_ty = low.ctx.i32_type();
    let void_ty = low.ctx.void_type();
    let f64_ty = low.f64t();
    let err = |e: inkwell::builder::BuilderError| e.to_string();

    // Untag arg `k` as a data pointer (string/collection char*/void*).
    let strp = |k: usize| low.untag_ptr(low.load(args[k]));
    let nil = i64_ty.const_int(NIL, false);

    // A returned char* → tagged str word, or nil when NULL.
    let tag_str_or_nil = |p: PointerValue<'ctx>| -> Result<IntValue<'ctx>, String> {
        let asint = b.build_ptr_to_int(p, i64_ty, "sp2i").map_err(err)?;
        let is_null = b
            .build_int_compare(inkwell::IntPredicate::EQ, asint, i64_ty.const_zero(), "isnull")
            .map_err(err)?;
        let tagged = b.build_or(asint, i64_ty.const_int(TAG_STR, false), "tagstr").map_err(err)?;
        Ok(b.build_select(is_null, nil, tagged, "strornil").map_err(err)?.into_int_value())
    };

    // Call a `(ptr, ptr, …) -> ptr` string function over `strp(0..n)`.
    let str_fn = |name: &str, n: usize| -> Result<PointerValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n];
        let f = low.runtime_fn(name, ptrt.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| strp(k).into()).collect();
        Ok(b.build_call(f, &argv, "modcall").map_err(err)?.as_any_value_enum().into_pointer_value())
    };
    // A `(ptr, …) -> void` sink → nil word.
    let void_ptr_fn = |name: &str, n: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n];
        let f = low.runtime_fn(name, void_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| strp(k).into()).collect();
        b.build_call(f, &argv, "").map_err(err)?;
        Ok(nil)
    };
    // A `(ptr) -> i32` predicate → bool word.
    let bool_ptr_fn = |name: &str| -> Result<IntValue<'ctx>, String> {
        let f = low.runtime_fn(name, i32_ty.fn_type(&[ptrt.into()], false));
        let r = b
            .build_call(f, &[strp(0).into()], "")
            .map_err(err)?
            .as_any_value_enum()
            .into_int_value();
        let bit = b
            .build_int_compare(inkwell::IntPredicate::NE, r, i32_ty.const_zero(), "b")
            .map_err(err)?;
        Ok(low.bool_word(bit))
    };
    // A `() / (ptr) -> i64` scalar → tagged int.
    let int_fn = |name: &str, n_ptr: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![ptrt.into(); n_ptr];
        let f = low.runtime_fn(name, i64_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n_ptr).map(|k| strp(k).into()).collect();
        let r = b.build_call(f, &argv, "").map_err(err)?.as_any_value_enum().into_int_value();
        Ok(low.tag_int(r))
    };

    // math.* take/return tagged value words directly (int-or-float dispatch is in
    // the runtime helper), so no arg/return coercion is needed.
    let math_fn = |name: &str, n: usize| -> Result<IntValue<'ctx>, String> {
        let params: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![i64_ty.into(); n];
        let f = low.runtime_fn(name, i64_ty.fn_type(&params, false));
        let argv: Vec<BasicMetadataValueEnum> = (0..n).map(|k| low.load(args[k]).into()).collect();
        Ok(b.build_call(f, &argv, "math").map_err(err)?.as_any_value_enum().into_int_value())
    };

    match (module, method) {
        // abs/pow are overflow-checked, and so are the four float -> int
        // conversions: `math.floor(math.inf())` saturates to a whole number no
        // tagged word can hold. All six go through C forwarders that raise
        // "integer overflow"; the rest cannot fail and call Rust directly.
        ("math", "abs") => math_fn("jade_math_abs", 1),
        ("math", "pow") => math_fn("jade_math_pow", 2),
        ("math", "floor" | "ceil" | "round" | "trunc") => {
            math_fn(&format!("jade_math_{method}"), 1)
        }
        ("math", "sqrt") => math_fn("jrt_math_sqrt", 1),
        ("math", "min" | "max") => math_fn(&format!("jrt_math_{method}"), 2),
        // The predicates answer a tagged bool word, which `math_fn` returns
        // unchanged — nothing here has to know the difference.
        (
            "math",
            "sign" | "ln" | "log2" | "log10" | "exp" | "sin" | "cos" | "tan" | "asin" | "acos"
            | "atan" | "is_nan" | "is_inf",
        ) => math_fn(&format!("jrt_math_{method}"), 1),
        ("math", "atan2" | "hypot") => math_fn(&format!("jrt_math_{method}"), 2),
        ("math", "clamp") => math_fn("jrt_math_clamp", 3),
        ("math", "pi" | "e" | "tau" | "inf" | "nan") => math_fn(&format!("jrt_math_{method}"), 0),
        ("path", "basename" | "ext" | "dirname" | "stem" | "abs") => {
            tag_str_or_nil(str_fn(&format!("jrt_path_{method}"), 1)?)
        }
        ("path", "is_abs") => bool_ptr_fn("jrt_path_is_abs"),
        ("path", "join") => {
            // Variadic left-fold through the 2-arg jrt_path_join primitive.
            let f =
                low.runtime_fn("jrt_path_join", ptrt.fn_type(&[ptrt.into(), ptrt.into()], false));
            let mut acc = strp(0);
            for k in 1..args.len() {
                acc = b
                    .build_call(f, &[acc.into(), strp(k).into()], "join")
                    .map_err(err)?
                    .as_any_value_enum()
                    .into_pointer_value();
            }
            Ok(low.tag_str(acc))
        }
        ("fs", "read") => {
            // jrt_fs_read(path, i32 trust) -> str (TAINTED unless trust, raises on
            // error). `fs.read(path, trust=<bool>)` passes the bool's bit4 as trust.
            let f =
                low.runtime_fn("jrt_fs_read", ptrt.fn_type(&[ptrt.into(), i32_ty.into()], false));
            let trust = if args.len() == 2 {
                let w = low.load(args[1]);
                let sh = b
                    .build_right_shift(w, i64_ty.const_int(4, false), false, "tsh")
                    .map_err(err)?;
                let bit = b.build_and(sh, i64_ty.const_int(1, false), "tbit").map_err(err)?;
                b.build_int_truncate(bit, i32_ty, "t32").map_err(err)?
            } else {
                i32_ty.const_zero()
            };
            let r = b
                .build_call(f, &[strp(0).into(), trust.into()], "read")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        ("fs", "read_bytes") => {
            // Same shape as fs.read: a `trust=<bool>` second argument passes the
            // bool's bit4 through, and the content is TAINTED without it.
            let f = low.runtime_fn(
                "jk_fs_read_bytes",
                i64_ty.fn_type(&[ptrt.into(), i32_ty.into()], false),
            );
            let trust = if args.len() == 2 {
                let w = low.load(args[1]);
                let sh = b
                    .build_right_shift(w, i64_ty.const_int(4, false), false, "tsh")
                    .map_err(err)?;
                let bit = b.build_and(sh, i64_ty.const_int(1, false), "tbit").map_err(err)?;
                b.build_int_truncate(bit, i32_ty, "t32").map_err(err)?
            } else {
                i32_ty.const_zero()
            };
            let r = b
                .build_call(f, &[strp(0).into(), trust.into()], "readb")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(r)
        }
        ("fs", "write_bytes" | "append_bytes") => {
            let fname =
                if method == "write_bytes" { "jk_fs_write_bytes" } else { "jk_fs_append_bytes" };
            let f = low.runtime_fn(fname, void_ty.fn_type(&[ptrt.into(), i64_ty.into()], false));
            b.build_call(f, &[strp(0).into(), low.load(args[1]).into()], "").map_err(err)?;
            Ok(i64_ty.const_int(NIL, false))
        }
        ("fs", "read_stdin_bytes") => {
            let f = low.runtime_fn("jk_fs_read_stdin_bytes", i64_ty.fn_type(&[], false));
            let r =
                b.build_call(f, &[], "stdinb").map_err(err)?.as_any_value_enum().into_int_value();
            Ok(r)
        }
        ("fs", "write_stdout_bytes") => {
            let f = low
                .runtime_fn("jk_fs_write_stdout_bytes", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[low.load(args[0]).into()], "").map_err(err)?;
            Ok(i64_ty.const_int(NIL, false))
        }
        ("fs", "exists") => bool_ptr_fn("jrt_fs_exists"),
        ("fs", "is_file") => bool_ptr_fn("jrt_fs_is_file"),
        ("fs", "is_dir") => bool_ptr_fn("jrt_fs_is_dir"),
        // Each of these goes through the C forwarder, not the Rust `_impl`:
        // the forwarder is what drains the pending error and raises. Calling
        // the impl directly would answer success in a compiled program where
        // the interpreter raises.
        ("fs", "copy") => void_ptr_fn("jrt_fs_copy", 2),
        ("fs", "rename") => void_ptr_fn("jrt_fs_rename", 2),
        ("fs", "rmdir") => void_ptr_fn("jrt_fs_rmdir", 1),
        ("fs", "size") => int_fn("jrt_fs_size", 1),
        ("fs", "write") => void_ptr_fn("jrt_fs_write", 2),
        ("fs", "append") => void_ptr_fn("jrt_fs_append", 2),
        ("fs", "delete") => void_ptr_fn("jrt_fs_delete", 1),
        ("fs", "mkdir") => void_ptr_fn("jrt_fs_mkdir", 1),
        ("sh", "exec") => Ok(low.tag_str(str_fn("jrt_sh_exec", 1)?)),
        ("sh", "run") => int_fn("jrt_sh_run", 1),
        ("sh", "output") => {
            // `jrt_sh_output`, not `jrt_coll_sh_output` — the forwarder is where
            // the tainted-command refusal lives, matching exec and run.
            let f = low.runtime_fn("jrt_sh_output", ptrt.fn_type(&[ptrt.into()], false));
            let p = b
                .build_call(f, &[strp(0).into()], "shout")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        ("fs", "list_dir") => {
            // raises on I/O error; returns an already-tagged array pointer word.
            let f = low.runtime_fn("jrt_fs_list_dir_chunk", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[strp(0).into()], "ld")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("random", "choice") => {
            // (arr word) -> element word (already tagged).
            let f =
                low.runtime_fn("jrt_random_choice_chunk", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(args[0]).into()], "choice")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("random", "shuffle") => {
            let f = low
                .runtime_fn("jrt_random_shuffle_chunk", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[low.load(args[0]).into()], "").map_err(err)?;
            Ok(nil)
        }
        ("env", "cwd") => Ok(low.tag_str(str_fn("jrt_env_cwd", 0)?)),
        ("env", "get") => tag_str_or_nil(str_fn("jrt_env_get", 1)?),
        ("env", "set") => void_ptr_fn("jrt_env_set", 2),
        ("time", "now") => int_fn("jrt_time_now", 0),
        ("time", "now_ms") => int_fn("jrt_time_now_ms", 0),
        ("time", "sleep") => {
            // (float seconds) -> nil. Unbox the boxed-float arg to a native f64.
            let unbox = low.runtime_fn("jrt_unbox_float", f64_ty.fn_type(&[i64_ty.into()], false));
            let d = b
                .build_call(unbox, &[low.load(args[0]).into()], "sec")
                .map_err(err)?
                .as_any_value_enum()
                .into_float_value();
            let f = low.runtime_fn("jrt_time_sleep", void_ty.fn_type(&[f64_ty.into()], false));
            b.build_call(f, &[d.into()], "").map_err(err)?;
            Ok(nil)
        }
        ("time", "monotonic") => {
            // () -> raw f64, boxed into a float word (same shape as random.float).
            let f = low.runtime_fn("jrt_time_monotonic", f64_ty.fn_type(&[], false));
            let d =
                b.build_call(f, &[], "mono").map_err(err)?.as_any_value_enum().into_float_value();
            let boxf = low.runtime_fn("jrt_box_float", i64_ty.fn_type(&[f64_ty.into()], false));
            Ok(b.build_call(boxf, &[d.into()], "boxf")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("time", "utc") => {
            // (raw i64 seconds) -> char*. Not `str_fn`: the argument is an int
            // word to untag, not a data pointer.
            let f = low.runtime_fn("jrt_time_utc", ptrt.fn_type(&[i64_ty.into()], false));
            let p = b
                .build_call(f, &[low.untag_int(low.load(args[0])).into()], "utc")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(p))
        }
        ("time", "parts") => {
            // (raw i64 seconds) -> already-tagged dict word.
            let f = low.runtime_fn("jrt_time_parts", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.untag_int(low.load(args[0])).into()], "parts")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("time", "stamp") => {
            // Six raw i64 fields in, raw i64 out. The trailing time-of-day
            // arguments are optional in the source, so pad the call with zeros
            // rather than giving the runtime a second signature to match.
            let f = low.runtime_fn("jrt_time_stamp", i64_ty.fn_type(&[i64_ty.into(); 6], false));
            let argv: Vec<BasicMetadataValueEnum> = (0..6)
                .map(|k| match args.get(k) {
                    Some(a) => low.untag_int(low.load(*a)).into(),
                    None => i64_ty.const_zero().into(),
                })
                .collect();
            let r =
                b.build_call(f, &argv, "stamp").map_err(err)?.as_any_value_enum().into_int_value();
            Ok(low.tag_int(r))
        }
        ("array", "map" | "filter") => {
            // (arr word, fn word) -> new array word. Both args are tagged words.
            let cname =
                if method == "map" { "jrt_coll_array_map" } else { "jrt_coll_array_filter" };
            let f = low.runtime_fn(cname, i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(args[0]).into(), low.load(args[1]).into()], "mapf")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("array", "join") => {
            let f = low.runtime_fn(
                "jrt_coll_array_join",
                ptrt.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let r = b
                .build_call(f, &[strp(0).into(), strp(1).into()], "join")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(r))
        }
        ("array", "sort" | "reverse") => {
            // (arr) -> a NEW array word. Deliberately not the in-place symbol
            // `a.sort()` uses: the package spelling answers with a copy and
            // leaves its argument alone. See `jrt_coll_array_sorted`.
            let cname =
                if method == "sort" { "jrt_coll_array_sorted" } else { "jrt_coll_array_reversed" };
            let f = low.runtime_fn(cname, ptrt.fn_type(&[ptrt.into()], false));
            let p = b
                .build_call(f, &[strp(0).into()], "arrcopy")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        ("random", "int") => {
            // Raw (untagged) i64 bounds; raises if lo > hi.
            let f = low.runtime_fn(
                "jrt_random_int",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            let lo = low.untag_int(low.load(args[0]));
            let hi = low.untag_int(low.load(args[1]));
            let r = b
                .build_call(f, &[lo.into(), hi.into()], "")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(low.tag_int(r))
        }
        ("random", "seed") => {
            let f = low.runtime_fn("jrt_random_seed", void_ty.fn_type(&[i64_ty.into()], false));
            b.build_call(f, &[low.untag_int(low.load(args[0])).into()], "").map_err(err)?;
            Ok(nil)
        }
        ("random", "float") => {
            let f = low.runtime_fn("jrt_random_float", f64_ty.fn_type(&[], false));
            let d = b.build_call(f, &[], "").map_err(err)?.as_any_value_enum().into_float_value();
            let boxf = low.runtime_fn("jrt_box_float", i64_ty.fn_type(&[f64_ty.into()], false));
            Ok(b.build_call(boxf, &[d.into()], "boxf")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        // These return already-tagged ObjHeader value words (dict/array or nil).
        ("env", "args") => {
            // () -> already-tagged array word (TRUSTED strings).
            let f = low.runtime_fn("jrt_env_args", i64_ty.fn_type(&[], false));
            Ok(b.build_call(f, &[], "args").map_err(err)?.as_any_value_enum().into_int_value())
        }
        ("time", "local") => Ok(low.tag_str(str_fn("jrt_time_local", 1)?)),
        // `get_bytes` rides the same arm as `get`: the C signature is identical
        // (url, headers) -> tagged dict word, and only the dict's `body` differs
        // in type. `post_bytes` cannot, because its body is a whole tagged word
        // rather than a data pointer — see the arm below.
        ("http", "get" | "delete" | "head" | "get_bytes") => {
            // (url, [headers]) -> already-tagged { status, body } dict word.
            let f = low.runtime_fn(
                &format!("jrt_http_{method}"),
                i64_ty.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 2 { strp(1) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), headers.into()], "http")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("http", "post" | "put") => {
            // (url, body, [headers]) -> tagged { status, body } dict word.
            let f = low.runtime_fn(
                &format!("jrt_http_{method}"),
                i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), strp(1).into(), headers.into()], "http")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("http" | "uhttp", "post_bytes") => {
            // (url, body word, [headers]) -> tagged { status, body } dict word.
            // The body goes over as its tagged word, not `strp(1)`: the runtime
            // checks the tag and reports a non-bytes argument, which it could
            // not do if all it received were a bare pointer.
            let f = low.runtime_fn(
                &format!("jrt_{module}_post_bytes"),
                i64_ty.fn_type(&[ptrt.into(), i64_ty.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            Ok(b.build_call(
                f,
                &[strp(0).into(), low.load(args[1]).into(), headers.into()],
                "httpb",
            )
            .map_err(err)?
            .as_any_value_enum()
            .into_int_value())
        }
        ("uhttp", "get" | "delete" | "head" | "get_bytes") => {
            // (unix-url, [headers]) -> already-tagged { status, body } dict word.
            let f = low.runtime_fn(
                &format!("jrt_uhttp_{method}"),
                i64_ty.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 2 { strp(1) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), headers.into()], "uhttp")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("uhttp", "post" | "put") => {
            // (unix-url, body, [headers]) -> tagged { status, body } dict word.
            let f = low.runtime_fn(
                &format!("jrt_uhttp_{method}"),
                i64_ty.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            Ok(b.build_call(f, &[strp(0).into(), strp(1).into(), headers.into()], "uhttp")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("uhttp", "stream") => {
            // (unix-url, handler fn word, [headers]) -> status as a tagged int.
            // The handler is passed as its whole tagged word, not a data pointer:
            // the C driver reads the raw function pointer out of the box itself,
            // the way `array.map` does.
            let f = low.runtime_fn(
                "jrt_uhttp_stream",
                i64_ty.fn_type(&[ptrt.into(), i64_ty.into(), ptrt.into()], false),
            );
            let headers = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            let status = b
                .build_call(
                    f,
                    &[strp(0).into(), low.load(args[1]).into(), headers.into()],
                    "ustream",
                )
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value();
            Ok(low.tag_int(status))
        }
        ("dict", "merge") => {
            // (d1, d2) -> new dict word (tagged ptr).
            let f = low.runtime_fn(
                "jrt_coll_dict_merge",
                ptrt.fn_type(&[ptrt.into(), ptrt.into()], false),
            );
            let p = b
                .build_call(f, &[strp(0).into(), strp(1).into()], "merge")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
        }
        ("json", "parse") => {
            // (str) -> value word (already tagged: dict/array/scalar).
            let f = low.runtime_fn("jrt_json_parse", i64_ty.fn_type(&[ptrt.into()], false));
            Ok(b.build_call(f, &[strp(0).into()], "jparse")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("json", "stringify" | "stringify_pretty") => {
            // (value word, i32 pretty) -> tagged string.
            let f = low.runtime_fn(
                "jrt_json_stringify_chunk",
                ptrt.fn_type(&[i64_ty.into(), i32_ty.into()], false),
            );
            let pretty = i32_ty.const_int(u64::from(method == "stringify_pretty"), false);
            let p = b
                .build_call(f, &[low.load(args[0]).into(), pretty.into()], "jstr")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_str(p))
        }
        ("Grammar", "new") => {
            // Grammar.new(pattern[, anchor[, stop]]) -> a Grammar object word.
            // Omitted optional args pass NULL (→ None in the runtime).
            let f = low.runtime_fn(
                "jrt_grammar_new",
                ptrt.fn_type(&[ptrt.into(), ptrt.into(), ptrt.into()], false),
            );
            let anchor = if args.len() >= 2 { strp(1) } else { ptrt.const_null() };
            let stop = if args.len() >= 3 { strp(2) } else { ptrt.const_null() };
            let g = b
                .build_call(f, &[strp(0).into(), anchor.into(), stop.into()], "grammar")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(g))
        }
        // ── std::bytes ─────────────────────────────────────────────────────
        //
        // All three can raise — a bad length, an element that is not an octet,
        // an argument that is not a blob — so each goes through a `jk_*` C
        // forwarder rather than calling the Rust entry point directly: a Jade
        // raise is a longjmp and must not unwind through a Rust frame. Each
        // hands back an already-tagged word.
        ("bytes", "zeros") => {
            let f = low.runtime_fn("jk_bytes_zeros", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(args[0]).into()], "bzeros")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("bytes", "from_ints") => {
            // The whole tagged word, not `strp(0)`: nothing upstream proves the
            // argument is an array, so the forwarder reads the tag and reports
            // rather than reading a dict as an `ArrayObj`.
            let f = low.runtime_fn("jk_bytes_from_ints", i64_ty.fn_type(&[i64_ty.into()], false));
            Ok(b.build_call(f, &[low.load(args[0]).into()], "bfrom")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        ("bytes", "concat") => {
            let f = low.runtime_fn(
                "jk_bytes_concat",
                i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false),
            );
            Ok(b.build_call(f, &[low.load(args[0]).into(), low.load(args[1]).into()], "bcat")
                .map_err(err)?
                .as_any_value_enum()
                .into_int_value())
        }
        _ => Err(format!("codegen: emit_module_call: unhandled {module}.{method}")),
    }
}

impl<'a, 'ctx> Lowerer<'a, 'ctx> {
    /// Lower `print(val)`: the runtime's tag-dispatching `jrt_print_any(val,
    /// "\n")` (VM-faithful `value_to_display` + trailing newline). `print`
    /// evaluates to nil.
    pub(super) fn print_value(&self, val: IntValue<'ctx>, dest: Reg) -> Result<(), String> {
        let f = self.runtime_fn(
            "jrt_print_any",
            self.ctx.void_type().fn_type(&[self.i64t().into(), self.ptrt().into()], false),
        );
        let nl = self
            .builder
            .build_global_string_ptr("\n", "jprint_nl")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        self.builder.build_call(f, &[val.into(), nl.into()], "").map_err(|e| e.to_string())?;
        self.store(dest, self.i64t().const_int(NIL, false));
        Ok(())
    }

    /// `write(x)`: the same render as `print`, with no newline and a flush.
    pub(super) fn write_value(&self, val: IntValue<'ctx>, dest: Reg) -> Result<(), String> {
        let f = self.runtime_fn(
            "jrt_write_any",
            self.ctx.void_type().fn_type(&[self.i64t().into()], false),
        );
        self.builder.build_call(f, &[val.into()], "").map_err(|e| e.to_string())?;
        self.store(dest, self.i64t().const_int(NIL, false));
        Ok(())
    }

    /// Guard a primitive method call's receiver: emit `jrt_require_kind`, which
    /// raises unless `recv` is one of the `WANT_*` kinds `want` names.
    ///
    /// This has to be a runtime check. `chunk_val_method_supported` picks the
    /// arm from the method *name*, and a name does not prove a kind — the
    /// receiver of `fn f(v) { v.keys() }` is only known once `f` is called.
    /// Without the guard the arm untagged the word to a pointer regardless and
    /// dereferenced it as the kind it assumed, which is a segfault on a scalar
    /// receiver and a silent wrong answer on the wrong collection.
    ///
    /// `jrt_require_kind` returns normally on a match, so callers keep emitting
    /// into the same block — unlike `throw`, this needs no block surgery.
    pub(super) fn require_kind(
        &self,
        recv: IntValue<'ctx>,
        want: u64,
        method: &str,
    ) -> Result<(), String> {
        let i32_ty = self.ctx.i32_type();
        let f = self.runtime_fn(
            "jrt_require_kind",
            self.ctx
                .void_type()
                .fn_type(&[self.i64t().into(), i32_ty.into(), self.ptrt().into()], false),
        );
        self.builder
            .build_call(
                f,
                &[recv.into(), i32_ty.const_int(want, false).into(), self.cstr(method).into()],
                "",
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
