// Chunk→LLVM path — the sole AOT lowering. `cfg` reconstructs basic blocks from
// the bytecode; `lower` translates each opcode into LLVM IR. `compile()` inlines
// imports, emits a thin `main()`, and lowers the bytecode `Chunk` (the same one
// the VM runs); an opcode it can't lower is a hard build error.
pub mod cfg;
pub mod imports;
pub mod lower;

use std::path::{Path, PathBuf};

use inkwell::{
    AddressSpace, OptimizationLevel,
    context::Context,
    module::Module,
    passes::PassBuilderOptions,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine},
    values::AnyValue,
};

use crate::compiler::tir::TProgram;

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
) -> Result<lower::LoweredProgram<'ctx>, String> {
    let cp = crate::compiler::emit::emit(program.clone()).map_err(|e| e.to_string())?;
    {
        let probe_ctx = Context::create();
        let probe_mod = probe_ctx.create_module("probe");
        lower::lower_program(
            &probe_ctx,
            &probe_mod,
            &cp.top,
            cp.top_n_slots,
            &cp.struct_defs,
            &cp.extend_methods,
        )?;
    }
    lower::lower_program(
        context,
        module,
        &cp.top,
        cp.top_n_slots,
        &cp.struct_defs,
        &cp.extend_methods,
    )
}

/// What `compile` produces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompileMode {
    /// An executable with a `main` entry point.
    #[default]
    Binary,
    /// A shared library exporting `jade_pkg_init` — a Jade-authored package
    /// other Jade projects can depend on. `exports` narrows which top-level
    /// functions are bound; empty means every named function, since Jade has no
    /// `pub` keyword and everything is public.
    SharedLib { exports: Vec<String> },
}

/// Emit the C-ABI surface of a Jade package: a `JadeNativeFnPtr`-shaped wrapper
/// per exported function, a `JadeBinding[]` table, and `jade_pkg_init`.
///
/// Lowered functions already take and return the tagged 64-bit word, so each
/// wrapper only has to marshal `JadeVal` in and out — `jrt_ffi_to_tagged` /
/// `jrt_ffi_from_tagged` in runtime_aot/native.c, the same conversions
/// `jrt_native_call` uses in the other direction.
#[allow(clippy::too_many_arguments)]
fn emit_pkg_init<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    init_fn: inkwell::values::FunctionValue<'ctx>,
    lowered: &lower::LoweredProgram<'ctx>,
    exports: &[String],
    output_path: &Path,
    deps_toml: Option<&str>,
) -> Result<(), String> {
    let i8_ty = context.i8_type();
    let i32_ty = context.i32_type();
    let i64_ty = context.i64_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let void_ty = context.void_type();

    // No `exports` list means export everything: Jade has no `pub`, so every
    // top-level function is public by construction.
    let mut names: Vec<String> = if exports.is_empty() {
        lowered.global_fns.keys().cloned().collect()
    } else {
        for want in exports {
            if !lowered.global_fns.contains_key(want) {
                return Err(format!(
                    "cannot export '{want}': no top-level function by that name in this file"
                ));
            }
        }
        exports.to_vec()
    };
    // Sorted so the binding table is deterministic rather than HashMap-ordered.
    names.sort();
    if names.is_empty() {
        return Err("nothing to export: this file defines no top-level functions".to_string());
    }

    let to_tagged = module.get_function("jrt_ffi_to_tagged").unwrap_or_else(|| {
        module.add_function("jrt_ffi_to_tagged", i64_ty.fn_type(&[ptr_ty.into()], false), None)
    });
    let from_tagged = module.get_function("jrt_ffi_from_tagged").unwrap_or_else(|| {
        module.add_function(
            "jrt_ffi_from_tagged",
            void_ty.fn_type(&[i64_ty.into(), ptr_ty.into()], false),
            None,
        )
    });

    // JadeVal is 16 bytes (tag + 7 pad + an 8-byte union), so argv[i] is at
    // byte offset i*16. Must match src/native/mod.rs and runtime.h.
    const JADE_VAL_SIZE: u64 = 16;

    let mut wrappers = Vec::new();
    for name in &names {
        let target = lowered.global_fns[name];
        let arity = target.count_params();

        let wrapper = module.add_function(
            &format!("jade_export${name}"),
            i32_ty.fn_type(&[i64_ty.into(), ptr_ty.into(), ptr_ty.into()], false),
            None,
        );
        let entry = context.append_basic_block(wrapper, "entry");
        let bad_arity = context.append_basic_block(wrapper, "bad_arity");
        let body = context.append_basic_block(wrapper, "body");
        builder.position_at_end(entry);

        // A host calling with the wrong number of arguments gets a non-zero
        // status, which the loader turns into a Jade error — rather than us
        // reading past the end of argv.
        let argc = wrapper.get_nth_param(0).unwrap().into_int_value();
        let ok = builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                argc,
                i64_ty.const_int(arity as u64, false),
                "arity_ok",
            )
            .map_err(|e| e.to_string())?;
        builder.build_conditional_branch(ok, body, bad_arity).map_err(|e| e.to_string())?;

        builder.position_at_end(bad_arity);
        builder.build_return(Some(&i32_ty.const_int(1, false))).map_err(|e| e.to_string())?;

        builder.position_at_end(body);
        let argv = wrapper.get_nth_param(1).unwrap().into_pointer_value();
        let out = wrapper.get_nth_param(2).unwrap().into_pointer_value();

        let mut args: Vec<inkwell::values::BasicMetadataValueEnum> = Vec::new();
        for i in 0..arity {
            let slot = unsafe {
                builder
                    .build_in_bounds_gep(
                        i8_ty,
                        argv,
                        &[i64_ty.const_int(i as u64 * JADE_VAL_SIZE, false)],
                        "argslot",
                    )
                    .map_err(|e| e.to_string())?
            };
            let tagged = builder
                .build_call(to_tagged, &[slot.into()], "arg")
                .map_err(|e| e.to_string())?
                .as_any_value_enum()
                .into_int_value();
            args.push(tagged.into());
        }

        let result = builder
            .build_call(target, &args, "ret")
            .map_err(|e| e.to_string())?
            .as_any_value_enum()
            .into_int_value();
        builder
            .build_call(from_tagged, &[result.into(), out.into()], "")
            .map_err(|e| e.to_string())?;
        builder.build_return(Some(&i32_ty.const_int(0, false))).map_err(|e| e.to_string())?;

        wrappers.push((name.clone(), wrapper));
    }

    // ── JadeBinding BINDINGS[] ────────────────────────────────────────────
    let binding_ty = context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false);
    let entries: Vec<inkwell::values::StructValue> = wrappers
        .iter()
        .map(|(name, f)| {
            let s = builder
                .build_global_string_ptr(name, "export_name")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            Ok(binding_ty
                .const_named_struct(&[s.into(), f.as_global_value().as_pointer_value().into()]))
        })
        .collect::<Result<_, String>>()?;

    let table_ty = binding_ty.array_type(entries.len() as u32);
    let table = module.add_global(table_ty, None, "jade_bindings");
    table.set_initializer(&binding_ty.const_array(&entries));
    table.set_constant(true);

    // ── uint32_t jade_pkg_abi_version(void) ───────────────────────────────
    //
    // The value ABI this package was built against, so a host can refuse one it
    // cannot talk to instead of failing somewhere inside the call.
    //
    // It is emitted rather than borrowed from the linked runtime because the
    // linker drops `jrt_abi_version` from a package that never calls it — the
    // packages published before this existed happen to keep the symbol, newer
    // ones do not, so it cannot be relied on either way. A function whose whole
    // body is `return <constant>` is always there.
    //
    // Why it matters: v1.1.31 started sending the inference request as a struct
    // (`JADE_TAG_STRUCT`). A provider built before that cannot read the tag, and
    // the failure surfaced as "native function returned an unknown value tag"
    // from deep inside the call, with nothing naming the real problem.
    {
        let abi_fn = module.add_function("jade_pkg_abi_version", i32_ty.fn_type(&[], false), None);
        let abi_entry = context.append_basic_block(abi_fn, "entry");
        builder.position_at_end(abi_entry);
        builder
            .build_return(Some(&i32_ty.const_int(jade_runtime::RUNTIME_ABI_VERSION as u64, false)))
            .map_err(|e| e.to_string())?;
    }

    // ── const char* jade_pkg_deps(void) ───────────────────────────────────
    //
    // What this package needs installed alongside it, as the `[[package]]`
    // tables of the lockfile it was built from. `jade pkg add` reads it and
    // merges the entries into the consuming project's lock, so adding a package
    // brings what it depends on with it.
    //
    // Carried *in the artifact* because that is the only place it can be. A
    // dependency names where a library lives, not an entry in a registry, so
    // there is nothing to ask about a `.so` except the `.so`. That is exactly
    // the claim this repo made for why transitive resolution was impossible; a
    // package can answer for itself, and now does.
    //
    // Emitted the same way `jade_pkg_abi_version` is, for the same reason: a
    // function whose whole body returns a constant is always in the symbol
    // table, where a linked-in symbol can be dropped.
    if let Some(toml) = deps_toml {
        let deps_fn = module.add_function("jade_pkg_deps", ptr_ty.fn_type(&[], false), None);
        let deps_entry = context.append_basic_block(deps_fn, "entry");
        builder.position_at_end(deps_entry);
        let text = builder
            .build_global_string_ptr(toml, "jade_pkg_deps_text")
            .map_err(|e| e.to_string())?
            .as_pointer_value();
        builder.build_return(Some(&text)).map_err(|e| e.to_string())?;
    }

    // ── int jade_pkg_init(JadeNativePkg* out) ─────────────────────────────
    let pkg_init =
        module.add_function("jade_pkg_init", i32_ty.fn_type(&[ptr_ty.into()], false), None);
    let entry = context.append_basic_block(pkg_init, "entry");
    let do_init = context.append_basic_block(pkg_init, "do_init");
    let fill = context.append_basic_block(pkg_init, "fill");
    builder.position_at_end(entry);

    // A host may load the package once but call jade_pkg_init more than once;
    // running the module's top level twice would re-execute its side effects.
    let inited = module.add_global(i32_ty, None, "jade_pkg_inited");
    inited.set_initializer(&i32_ty.const_zero());
    let seen = builder
        .build_load(i32_ty, inited.as_pointer_value(), "seen")
        .map_err(|e| e.to_string())?
        .into_int_value();
    let first = builder
        .build_int_compare(inkwell::IntPredicate::EQ, seen, i32_ty.const_zero(), "first")
        .map_err(|e| e.to_string())?;
    builder.build_conditional_branch(first, do_init, fill).map_err(|e| e.to_string())?;

    builder.position_at_end(do_init);
    builder
        .build_store(inited.as_pointer_value(), i32_ty.const_int(1, false))
        .map_err(|e| e.to_string())?;
    builder.build_call(init_fn, &[], "").map_err(|e| e.to_string())?;
    builder.build_unconditional_branch(fill).map_err(|e| e.to_string())?;

    // JadeNativePkg { const char* name; const JadeBinding* bindings; size_t n; }
    builder.position_at_end(fill);
    let out = pkg_init.get_nth_param(0).unwrap().into_pointer_value();
    let pkg_ty = context.struct_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);

    let pkg_name =
        output_path.file_stem().and_then(|s| s.to_str()).unwrap_or("jade_package").to_string();
    let name_ptr = builder
        .build_global_string_ptr(&pkg_name, "pkg_name")
        .map_err(|e| e.to_string())?
        .as_pointer_value();

    for (i, value) in [
        inkwell::values::BasicValueEnum::from(name_ptr),
        table.as_pointer_value().into(),
        i64_ty.const_int(entries.len() as u64, false).into(),
    ]
    .into_iter()
    .enumerate()
    {
        let field =
            builder.build_struct_gep(pkg_ty, out, i as u32, "field").map_err(|e| e.to_string())?;
        builder.build_store(field, value).map_err(|e| e.to_string())?;
    }

    builder.build_return(Some(&i32_ty.const_zero())).map_err(|e| e.to_string())?;
    Ok(())
}

/// Directories holding the two runtime archives `jade build` links into every
/// binary it emits: `libJadeRuntime.a` (C) and `libjade_runtime.a` (Rust).
/// Returns `(c_dir, rust_dir)`.
///
/// Searched in order:
///
///  1. `JADE_RT_LIB` / `JADE_RUST_RT` — explicit override, wins outright.
///  2. `<dir of this executable>/../lib/jade` — the installed layout. A release
///     tarball ships `jade` plus `lib/*.a`, and `install.sh` puts the binary in
///     e.g. `/usr/local/bin` and the archives in `/usr/local/lib/jade`.
///  3. The build.rs-baked paths — correct in a dev tree, and absolute paths into
///     a `target/` directory that only exists on the machine that compiled the
///     toolchain.
///
/// Before (2) existed, an installed jade had only (3), so `jade run` worked
/// while `jade build` failed with `cannot find -lJadeRuntime` — the archives
/// were never shipped and the baked paths pointed at the release runner's
/// `target/`. Resolving relative to the executable keeps the binary and its
/// runtime together however the user relocates them.
fn runtime_archive_dirs() -> (String, String) {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("..").join("lib").join("jade")))
        .filter(|d| d.join("libJadeRuntime.a").is_file() && d.join("libjade_runtime.a").is_file());

    let pick = |over: &str, baked: &str| -> String {
        if let Ok(v) = std::env::var(over) {
            return v;
        }
        if let Some(ref d) = installed {
            return d.display().to_string();
        }
        baked.to_string()
    };

    (pick("JADE_RT_LIB", env!("JADE_RT_LIB_DIR")), pick("JADE_RUST_RT", env!("JADE_RUST_RT_DIR")))
}

/// What a compile produced, beyond the artifact on disk.
///
/// `ir` is the printed module when `emit_ir` asked for it. `dependencies` are
/// the native packages the artifact will look for at startup — the caller needs
/// them to copy exactly those beside it, rather than everything the project
/// happens to have locked.
#[derive(Debug)]
pub struct CompileOutcome {
    pub ir: Option<String>,
    pub dependencies: Vec<PathBuf>,
}

pub fn compile(
    program: TProgram,
    source_path: Option<&Path>,
    output_path: &Path,
    emit_ir: bool,
) -> Result<CompileOutcome, String> {
    compile_with_mode(program, source_path, output_path, emit_ir, CompileMode::Binary)
}

pub fn compile_with_mode(
    program: TProgram,
    source_path: Option<&Path>,
    output_path: &Path,
    emit_ir: bool,
    mode: CompileMode,
) -> Result<CompileOutcome, String> {
    // ── Import resolution + module namespacing ────────────────────────────
    // Inline every imported `.jde` file into one stream, mangling each imported
    // module's globals into its own `name$<id>` namespace so distinct modules
    // never collide — matching the bytecode VM (the source of truth), which keeps
    // imports namespaced. `main` keeps bare names. See `imports.rs`.
    let (mut program, native_pkgs, _libs_root) = if let Some(src) = source_path {
        let r = imports::resolve_and_namespace(program.stmts, src)?;
        (crate::compiler::tir::TProgram { stmts: r.stmts }, r.native_pkgs, r.libs_root)
    } else {
        (program, Vec::new(), None)
    };

    // ── Inline defaults into cross-file struct literals ───────────────────
    // The per-file inference pass can't see imported StructDefs, so literals
    // like `messages.Session { system: ..., tools: ... }` leave default-bearing
    // fields (e.g. `let _history = []`) missing.  Without this pass the emitter
    // would zero-initialise those slots — turning `[]` into a NULL pointer.
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut program)
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
    for pkg in &native_pkgs {
        let pkgid = pkg.id;
        let g = module.add_global(ptr_ty, None, &format!("native_pkg${pkgid}"));
        g.set_initializer(&ptr_ty.const_null());
    }

    // ── Module initializer ────────────────────────────────────────────────
    // `jade_mod_init()` holds everything that must run before any exported or
    // top-level user code: dlopen'ing native packages, then the lowered
    // top-level chunk. Both entry points below call it, so a binary and a
    // shared library initialize identically.
    let init_fn = module.add_function("jade_mod_init", void_ty.fn_type(&[], false), None);
    let init_bb = context.append_basic_block(init_fn, "entry");
    builder.position_at_end(init_bb);

    // Load every native package once, before any user code runs.
    //
    // Each is named twice: by a `libs/`-relative key, which is what a relocated
    // artifact resolves against the root its host published, and by the absolute
    // path it sat at when this was built, which is the answer for a library that
    // is not a dependency and so has no relative spelling. A null key means the
    // second one is all there is. See runtime_aot/native.c for why there is one
    // root and not a search order.
    //
    // jrt_native_load_rel raises on failure; with no handler installed here that
    // aborts the program, matching the VM's fatal-on-missing-package behavior.
    if !native_pkgs.is_empty() {
        let load_fn = module.get_function("jrt_native_load_rel").unwrap_or_else(|| {
            module.add_function(
                "jrt_native_load_rel",
                ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
                None,
            )
        });
        for pkg in &native_pkgs {
            let pkgid = pkg.id;
            let rel_ptr = match &pkg.rel {
                Some(rel) => builder
                    .build_global_string_ptr(rel, "native_pkg_rel")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value(),
                None => ptr_ty.const_null(),
            };
            let abs_ptr = builder
                .build_global_string_ptr(&pkg.abs, "native_pkg_path")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let handle = builder
                .build_call(load_fn, &[rel_ptr.into(), abs_ptr.into()], "native_load")
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
    let lowered = try_chunk_toplevel(&context, &module, &program)?;
    builder.build_call(lowered.toplevel, &[], "").map_err(|e| e.to_string())?;
    if builder.get_insert_block().and_then(|b| b.get_terminator()).is_none() {
        builder.build_return(None).map_err(|e| e.to_string())?;
    }

    match &mode {
        CompileMode::Binary => {
            // ── main(i32 argc, ptr argv) ──────────────────────────────────
            // Forward the args to the runtime via jrt_set_args so env.args() can
            // read them; a bare main() would leave env.args() empty.
            let main_fn = module.add_function(
                "main",
                i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false),
                None,
            );
            let entry_bb = context.append_basic_block(main_fn, "entry");
            builder.position_at_end(entry_bb);

            let argc = main_fn.get_nth_param(0).unwrap().into_int_value();
            let argv = main_fn.get_nth_param(1).unwrap().into_pointer_value();
            let set_args = module.get_function("jrt_set_args").unwrap_or_else(|| {
                module.add_function(
                    "jrt_set_args",
                    void_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false),
                    None,
                )
            });
            builder
                .build_call(set_args, &[argc.into(), argv.into()], "")
                .map_err(|e| e.to_string())?;

            // Say where this program's dependencies live, before anything loads
            // one. A binary is a host: it owns the process, so it is the thing
            // entitled to decide the single root every image will resolve
            // against. A package is not, and `SharedLib` below deliberately does
            // not do this — a second publisher would be a second root, and a
            // second root is a second copy of a dependency with its own state.
            //
            // The path is relative to the artifact, so moving the pair moves
            // both. `jrt_libs_root_publish` defers to a root the user already
            // set; see runtime_aot/native.c.
            if !native_pkgs.is_empty() {
                let publish = module.get_function("jrt_libs_root_publish").unwrap_or_else(|| {
                    module.add_function(
                        "jrt_libs_root_publish",
                        void_ty.fn_type(&[ptr_ty.into()], false),
                        None,
                    )
                });
                let root_ptr = builder
                    .build_global_string_ptr(crate::pkg::LIBS_DIR, "jade_libs_root")
                    .map_err(|e| e.to_string())?
                    .as_pointer_value();
                builder.build_call(publish, &[root_ptr.into()], "").map_err(|e| e.to_string())?;
            }

            builder.build_call(init_fn, &[], "").map_err(|e| e.to_string())?;

            // Just before returning, call jrt_heap_report() — a no-op unless
            // JADE_HEAP_REPORT is set, in which case it prints the live
            // heap-object count. This is the cycle collector's instrument:
            // difftest can't observe a leak (or, once refcounting lands, a
            // premature free), so the count at normal exit is how those bugs are
            // made visible. See runtime gc.rs.
            let report = module.get_function("jrt_heap_report").unwrap_or_else(|| {
                module.add_function("jrt_heap_report", void_ty.fn_type(&[], false), None)
            });
            builder.build_call(report, &[], "").map_err(|e| e.to_string())?;
            builder.build_return(Some(&i32_ty.const_int(0, false))).map_err(|e| e.to_string())?;
        }
        CompileMode::SharedLib { exports } => {
            // What this package will need installed beside it, taken from the
            // lock it was built against. A package built outside a project, or
            // one with no dependencies, carries nothing and the symbol is
            // simply absent — which is also what every package published before
            // this looks like.
            let deps_toml = source_path
                .and_then(|p| p.parent())
                .and_then(crate::project::find_project_root_from)
                .and_then(|root| crate::pkg::lock::read(&root).ok().flatten())
                .filter(|l| !l.packages.is_empty())
                .and_then(|l| toml::to_string_pretty(&l).ok());
            emit_pkg_init(
                &context,
                &module,
                &builder,
                init_fn,
                &lowered,
                exports,
                output_path,
                deps_toml.as_deref(),
            )?;
        }
    }

    module.verify().map_err(|e| e.to_string())?;

    // Only the ones that came from the project's `libs/`. A hand-written `[lib]`
    // pointing elsewhere is not a dependency and is not the caller's to copy.
    let dependencies: Vec<PathBuf> =
        native_pkgs.iter().filter(|p| p.rel.is_some()).map(|p| PathBuf::from(&p.abs)).collect();

    if emit_ir {
        return Ok(CompileOutcome { ir: Some(module.print_to_string().to_string()), dependencies });
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

    // Run the mid-level optimization pipeline before codegen. `write_to_file`
    // only runs the target machine's *codegen* passes — it does NOT run the
    // target-independent IR passes (mem2reg, SROA, instcombine, inlining, GVN).
    // Without this the lowered IR keeps every register as a stack `alloca` with a
    // load/store on each access, so a tight recursive function like `fib` is
    // memory-bound rather than register-bound. `default<O2>` promotes those to
    // SSA and cleans up the redundancy the chunk→IR translation emits.
    module
        .run_passes("default<O2>", &machine, PassBuilderOptions::create())
        .map_err(|e| e.to_string())?;

    let obj_path = output_path.with_extension("o");
    machine.write_to_file(&module, FileType::Object, &obj_path).map_err(|e| e.to_string())?;

    let mut cc = std::process::Command::new("cc");
    cc.arg(&obj_path).arg("-o").arg(output_path);
    // A package is loaded with dlopen, not exec'd, so it links as a shared
    // object with no entry point. -fPIC matters here in a way it does not for
    // an executable: the code is mapped at an arbitrary address.
    if matches!(mode, CompileMode::SharedLib { .. }) {
        if cfg!(target_os = "macos") {
            cc.arg("-dynamiclib");
        } else {
            cc.arg("-shared");
        }
        cc.arg("-fPIC");
    }
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
    // Where the two runtime archives live. Both are searched in the same order
    // (env override → installed layout → dev tree); see `runtime_archive_dirs`.
    let (rt_lib, rust_rt) = runtime_archive_dirs();
    cc.arg(format!("-L{rt_lib}"));
    cc.arg("-lJadeRuntime");
    // The shared Rust runtime staticlib supplies symbols moved out of the C
    // runtime (float boxing, ipow, …). It must come *after* -lJadeRuntime, whose
    // members reference these symbols (static-archive left-to-right resolution).
    cc.arg(format!("-L{rust_rt}"));
    cc.arg("-ljade_runtime");
    // The link is now *bidirectional*: the C runtime references Rust symbols
    // (jrt_coll_*/jrt_core_*), and — since grammar-constrained prompts — the Rust
    // runtime references a C symbol (jrt_prompt_grammar_obj → C jrt_prompt_grammar_ex).
    // Static-archive resolution is single-pass left-to-right on Linux, so repeat
    // the C archive after the Rust one to close the cycle. (macOS's ld resolves
    // all archives together and doesn't need it, but the repeat is harmless.)
    cc.arg("-lJadeRuntime");
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
    let result = cc.output().map_err(|e| format!("linker not found: {e}"))?;

    std::fs::remove_file(&obj_path).ok();
    if !result.status.success() {
        // Include the linker's own output: "linking failed" alone gives the user
        // nothing to act on, and undefined-symbol lists are the whole diagnosis.
        return Err(format!("linking failed\n{}", String::from_utf8_lossy(&result.stderr).trim()));
    }

    Ok(CompileOutcome { ir: None, dependencies })
}

#[cfg(test)]
mod tests;
