// Chunk→LLVM path — the sole AOT lowering. `cfg` reconstructs basic blocks from
// the bytecode; `lower` translates each opcode into LLVM IR. `compile()` inlines
// imports, emits a thin `main()`, and lowers the bytecode `Chunk` (the same one
// the VM runs); an opcode it can't lower is a hard build error.
pub mod cfg;
pub mod lower;
pub mod imports;

use std::path::Path;

use inkwell::{
    context::Context,
    module::Module,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine},
    values::AnyValue,
    AddressSpace, OptimizationLevel,
};

use jade::compiler::tir::TProgram;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower the whole program via the Chunk→LLVM path. On success, emits
/// `jade_toplevel() -> i64` (plus its `fn_defs`) into `module` and returns it,
/// for `main` to call after its prologue. Returns `Err` on any opcode the
/// backend can't lower — a hard build error (there is no legacy fallback).
///
/// A **probe** runs the identical lowering into a throwaway module first: if it
/// fails partway (an unsupported opcode after some functions/globals were
/// already emitted), only the throwaway module is polluted — the real module is
/// touched only once the whole program is known to lower cleanly.
fn try_chunk_toplevel<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &TProgram,
) -> Result<inkwell::values::FunctionValue<'ctx>, String> {
    let cp = jade::compiler::emit::emit(program.clone()).map_err(|e| e.to_string())?;
    {
        let probe_ctx = Context::create();
        let probe_mod = probe_ctx.create_module("probe");
        lower::lower_program(&probe_ctx, &probe_mod, &cp.top, cp.top_n_slots, &cp.struct_defs, &cp.extend_methods)?;
    }
    lower::lower_program(context, module, &cp.top, cp.top_n_slots, &cp.struct_defs, &cp.extend_methods)
}

pub fn compile(program: TProgram, source_path: Option<&Path>, output_path: &Path, emit_ir: bool) -> Result<Option<String>, String> {
    // ── Import resolution + module namespacing ────────────────────────────
    // Inline every imported `.jde` file into one stream, mangling each imported
    // module's globals into its own `name$<id>` namespace so distinct modules
    // never collide — matching the bytecode VM (the source of truth), which keeps
    // imports namespaced. `main` keeps bare names. See `imports.rs`.
    let (mut program, native_pkgs) = if let Some(src) = source_path {
        let (stmts, native_pkgs) = imports::resolve_and_namespace(program.stmts, src)?;
        (jade::compiler::tir::TProgram { stmts }, native_pkgs)
    } else {
        (program, Vec::new())
    };

    // ── Inline defaults into cross-file struct literals ───────────────────
    // The per-file inference pass can't see imported StructDefs, so literals
    // like `messages.Session { system: ..., tools: ... }` leave default-bearing
    // fields (e.g. `let _history = []`) missing.  Without this pass the emitter
    // would zero-initialise those slots — turning `[]` into a NULL pointer.
    jade::compiler::type_infer::fill_struct_literal_defaults(&mut program)
        .map_err(|e| e.to_string())?;

    let context = Context::create();
    let module = context.create_module("jade_program");
    let builder = context.create_builder();

    let i32_ty = context.i32_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let void_ty = context.void_type();

    // ── Native package handle globals ─────────────────────────────────────
    // One `@native_pkg$<id>: ptr = null` per dlopen'd native library, filled in
    // main's prologue below. A `__native$<id>$<fn>` reference (lowered in
    // lower.rs) loads its handle from this by-name global.
    for (pkgid, _) in &native_pkgs {
        let g = module.add_global(ptr_ty, None, &format!("native_pkg${pkgid}"));
        g.set_initializer(&ptr_ty.const_null());
    }

    // ── main(i32 argc, ptr argv) entry point ──────────────────────────────
    // Forward the args to the runtime via jrt_set_args so env.args() can read
    // them; a bare main() would leave env.args() empty.
    let main_fn = module.add_function(
        "main", i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false), None);
    let entry_bb = context.append_basic_block(main_fn, "entry");
    builder.position_at_end(entry_bb);

    {
        let argc = main_fn.get_nth_param(0).unwrap().into_int_value();
        let argv = main_fn.get_nth_param(1).unwrap().into_pointer_value();
        let set_args = module.get_function("jrt_set_args").unwrap_or_else(|| {
            module.add_function(
                "jrt_set_args", void_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false), None)
        });
        builder.build_call(set_args, &[argc.into(), argv.into()], "")
            .map_err(|e| e.to_string())?;
    }

    // Load every native package once, before any user code runs. The path is a
    // build-time-resolved absolute path embedded as a plain C string (dlopen'd at
    // runtime — see imports.rs / runtime_lib/native.c). jrt_native_load raises on
    // a failed dlopen; with no handler installed here that aborts the program,
    // matching the VM's fatal-on-missing-package behavior.
    if !native_pkgs.is_empty() {
        let load_fn = module.get_function("jrt_native_load").unwrap_or_else(|| {
            module.add_function("jrt_native_load", ptr_ty.fn_type(&[ptr_ty.into()], false), None)
        });
        for (pkgid, path) in &native_pkgs {
            let path_ptr = builder
                .build_global_string_ptr(path, "native_pkg_path")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let handle = builder
                .build_call(load_fn, &[path_ptr.into()], "native_load")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_pointer_value();
            let g = module.get_global(&format!("native_pkg${pkgid}")).unwrap();
            builder.build_store(g.as_pointer_value(), handle).map_err(|e| e.to_string())?;
        }
    }

    // ── Body: lower the bytecode Chunk → LLVM ─────────────────────────────
    // The AOT backend runs the same `Chunk` the VM interprets (Track A). This is
    // the only lowering path — the legacy `expr.rs`/`stmt.rs` re-lowering of the
    // typed AST is gone — so any opcode the Chunk backend can't handle is a hard
    // build error rather than a silent fallback.
    let top_fn = try_chunk_toplevel(&context, &module, &program)?;
    builder.build_call(top_fn, &[], "").map_err(|e| e.to_string())?;

    // Close main() with `return 0` if the body didn't already terminate. Just
    // before returning, call jrt_heap_report() — a no-op unless JADE_HEAP_REPORT
    // is set in the environment, in which case it prints the live heap-object
    // count. This is the cycle collector's instrument: difftest can't observe a
    // leak (or, once refcounting lands, a premature free), so the count at normal
    // exit is how those bugs are made visible. See runtime gc.rs.
    if builder.get_insert_block().and_then(|b| b.get_terminator()).is_none() {
        let report = module.get_function("jrt_heap_report").unwrap_or_else(|| {
            module.add_function("jrt_heap_report", void_ty.fn_type(&[], false), None)
        });
        builder.build_call(report, &[], "").map_err(|e| e.to_string())?;
        builder
            .build_return(Some(&i32_ty.const_int(0, false)))
            .map_err(|e| e.to_string())?;
    }

    module.verify().map_err(|e| e.to_string())?;

    if emit_ir {
        return Ok(Some(module.print_to_string().to_string()));
    }

    // ── AOT compilation ───────────────────────────────────────────────────
    Target::initialize_all(&InitializationConfig::default());
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let machine = target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("failed to create LLVM target machine")?;

    let obj_path = output_path.with_extension("o");
    machine
        .write_to_file(&module, FileType::Object, &obj_path)
        .map_err(|e| e.to_string())?;

    let mut cc = std::process::Command::new("cc");
    cc.arg(&obj_path).arg("-o").arg(output_path);
    if cfg!(target_os = "macos") {
        if let Ok(ver) = std::process::Command::new("sw_vers").args(["-productVersion"]).output() {
            if let Ok(s) = std::str::from_utf8(&ver.stdout) {
                let short = s.trim().splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
                cc.arg(format!("-mmacosx-version-min={short}"));
            }
        }
    }
    // Always link the runtime archive. Previously this was gated on a hand-kept
    // set of feature flags (uses_runtime/uses_async/uses_dicts/uses_exceptions/
    // uses_prompts), but that enumeration was incomplete: programs that only call
    // string methods (jrt_str_trim/split/replace/contains) or array methods
    // (jrt_array_push/pop) set none of those flags, so `-lJadeRuntime` was never
    // passed and the link failed with undefined `_jrt_*` symbols. The runtime is
    // a static archive, so unreferenced members aren't pulled into trivial
    // binaries — always linking is harmless and removes the whole bug class.
    // The C runtime archive dir: a caller-set `JADE_RT_LIB` (installed daemon)
    // wins, else this crate's own build.rs-baked OUT_DIR (dev builds — codegen
    // now owns the C runtime, so it needs no daemon to hand it the path). Same
    // for the shared Rust runtime staticlib below.
    let rt_lib = std::env::var("JADE_RT_LIB").unwrap_or_else(|_| env!("JADE_RT_LIB_DIR").to_string());
    cc.arg(format!("-L{rt_lib}"));
    cc.arg("-lJadeRuntime");
    // The shared Rust runtime staticlib supplies symbols moved out of the C
    // runtime (float boxing, ipow, …). It must come *after* -lJadeRuntime, whose
    // members reference these symbols (static-archive left-to-right resolution).
    let rust_rt = std::env::var("JADE_RUST_RT").unwrap_or_else(|_| env!("JADE_RUST_RT_DIR").to_string());
    cc.arg(format!("-L{rust_rt}"));
    cc.arg("-ljade_runtime");
    #[cfg(target_os = "linux")]
    {
        cc.arg("-lpthread");
        // dlopen/dlsym for native (C-ABI) packages (libdl). macOS has these in
        // libSystem, so no flag is needed there.
        cc.arg("-ldl");
        // libm for the runtime's float math (jrt_pow_any/jrt_mod_any call
        // pow/fmod). macOS folds libm into libSystem, so no flag is needed
        // there; on Linux it's a separate archive and must come *after*
        // -lJadeRuntime, which references these symbols.
        cc.arg("-lm");
    }
    let status = cc.status().map_err(|e| format!("linker not found: {e}"))?;

    std::fs::remove_file(&obj_path).ok();
    if !status.success() { return Err("linking failed".into()); }

    Ok(None)
}
