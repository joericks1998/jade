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
    "path", "random",
];

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
            | "Grammar"
    )
}

/// Whether the Chunk backend can lower `module.method` with `argc` explicit args.
/// Restricted to **layout-safe** methods — string/scalar I/O whose runtime symbols
/// don't produce or consume the legacy `JrtArrayHdr`/`JadeDict` collection layouts
/// (those need ObjHeader-aware runtime helpers, a later brick). Everything else
/// declines to the legacy path. Keep in lockstep with `emit_module_call`.
pub(super) fn chunk_module_supported(module: &str, method: &str, argc: usize) -> bool {
    match (module, method) {
        ("math", "floor" | "ceil" | "abs" | "sqrt") => argc == 1,
        ("math", "min" | "max" | "pow") => argc == 2,
        ("path", "basename" | "ext" | "dirname" | "stem" | "abs") => argc == 1,
        ("path", "is_abs") => argc == 1,
        ("path", "join") => argc >= 1,
        ("fs", "read") => argc == 1 || argc == 2, // read(path) or read(path, trust)
        ("fs", "exists" | "delete" | "mkdir") => argc == 1,
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
        ("time", "now" | "now_ms") => argc == 0,
        ("time", "sleep") => argc == 1,
        ("time", "local") => argc == 1,
        ("http", "get" | "delete" | "head" | "get_bytes") => argc == 1 || argc == 2,
        ("http", "post" | "put" | "post_bytes") => argc == 2 || argc == 3,
        ("uhttp", "get" | "delete" | "head" | "get_bytes") => argc == 1 || argc == 2,
        ("uhttp", "post" | "put" | "post_bytes") => argc == 2 || argc == 3,
        ("uhttp", "stream") => argc == 2 || argc == 3, // url, handler[, headers]
        ("array", "map" | "filter") => argc == 2,
        ("random", "int") => argc == 2,
        ("random", "seed") => argc == 1,
        ("random", "float") => argc == 0,
        ("dict", "merge") => argc == 2,
        ("json", "parse" | "stringify" | "stringify_pretty") => argc == 1,
        ("Grammar", "new") => (1..=3).contains(&argc), // pattern[, anchor[, stop]]
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
        "starts_with" | "ends_with" => argc == 1,
        "replace" => argc == 2,
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
        "keys" | "values" => argc == 0,          // dict
        "has" | "get" => argc == 1,              // dict
        "contains" => argc == 1,                 // str / array (runtime-dispatched)
        "len" => argc == 0,                      // str / array / dict / bytes (runtime-dispatched)
        "decode" => argc == 0,                   // bytes
        "slice" => argc == 2,                    // bytes
        _ => false,
    }
}

/// Emit a runtime-dispatched struct method `recv.method(args)`: look up the
/// implementation by the receiver's runtime type name (`jrt_get_type_name` →
/// `jrt_method_lookup`), then indirect-call it with `self` (the receiver)
/// prepended. Used only for genuinely-ambiguous method names (same name+arity on
/// >1 type); exact-arity, so no default filling.
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

    // type_word = tag_str(jrt_get_type_name(recv))
    let gtn = low.runtime_fn("jrt_get_type_name", ptrt.fn_type(&[i64_ty.into()], false));
    let tn = b
        .build_call(gtn, &[low.load(recv).into()], "tname")
        .map_err(err)?
        .as_any_value_enum()
        .into_pointer_value();
    let type_word = low.tag_str(tn);
    // fnptr = jrt_method_lookup(type_word, "method")
    let lookup =
        low.runtime_fn("jrt_method_lookup", ptrt.fn_type(&[i64_ty.into(), ptrt.into()], false));
    let fnptr = b
        .build_call(lookup, &[type_word.into(), low.cstr(method).into()], "mfn")
        .map_err(err)?
        .as_any_value_enum()
        .into_pointer_value();
    // Indirect call: fnptr(self, args...) — one i64 param per (self + args).
    let arity = args.len() + 1;
    let arg_tys = vec![i64_ty.into(); arity];
    let fn_ty = i64_ty.fn_type(&arg_tys, false);
    let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(arity);
    argv.push(low.load(recv).into());
    for a in args {
        argv.push(low.load(*a).into());
    }
    Ok(b.build_indirect_call(fn_ty, fnptr, &argv, "dmcall")
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
        i64_ty.const_int(OBJKIND_FN, false).into(),
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
        "push" | "pop" | "sort" | "reverse" => {
            low.require_kind(low.load(recv), WANT_ARRAY, method)?
        }
        "keys" | "values" | "has" | "get" => low.require_kind(low.load(recv), WANT_DICT, method)?,
        _ => {}
    }

    let recv_p = low.untag_ptr(low.load(recv));
    let nil = i64_ty.const_int(NIL, false);

    match method {
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
        "slice" => {
            let f = low.runtime_fn(
                "jrt_bytes_slice",
                ptrt.fn_type(&[ptrt.into(), i64_ty.into(), i64_ty.into()], false),
            );
            let s = low.untag_int(low.load(args[0]));
            let e = low.untag_int(low.load(args[1]));
            let p = b
                .build_call(f, &[recv_p.into(), s.into(), e.into()], "slice")
                .map_err(err)?
                .as_any_value_enum()
                .into_pointer_value();
            Ok(low.tag_ptr(p))
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

/// Emit a layout-safe stdlib `module.method(args)` call, returning the tagged
/// result word. Only methods `chunk_module_supported` accepts reach here; each
/// reuses the same runtime `jrt_*` symbol the legacy path calls. String returns
/// are null-checked → tagged nil (covers `path.ext`/`env.get`); scalar returns
/// tag directly. Collection-producing/consuming methods are excluded (they use
/// the legacy `JrtArrayHdr`/`JadeDict` layout, not the Chunk path's ObjHeader).
pub(super) fn emit_module_call<'ctx>(
    low: &Lowerer<'_, 'ctx>,
    module: &str,
    method: &str,
    args: &[Reg],
) -> Result<IntValue<'ctx>, String> {
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
        // abs/pow are overflow-checked, so they go through C forwarders that
        // raise "integer overflow"; the rest cannot fail and call Rust directly.
        ("math", "abs") => math_fn("jade_math_abs", 1),
        ("math", "pow") => math_fn("jade_math_pow", 2),
        ("math", "floor" | "ceil" | "sqrt") => math_fn(&format!("jrt_math_{method}"), 1),
        ("math", "min" | "max") => math_fn(&format!("jrt_math_{method}"), 2),
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
