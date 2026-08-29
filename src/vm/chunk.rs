//! Program entry points and import resolution.
//!
//! [`run`] / [`run_incremental`] are the public ways to execute a compiled
//! program; `run_with_state` is the shared body they and the import machinery
//! use so an imported file shares globals/struct-defs/methods with its importer.
//! [`resolve_user_import`] maps a `use` path to a file on disk (a registered
//! `[lib]`, a sibling module, or a native library).

use super::*;

/// A `use` path resolved to either a native shared library (loaded over the FFI)
/// or a Jade source file (parsed + run in a sub-state).
pub(crate) enum ResolvedImport {
    Native(PathBuf),
    File(PathBuf),
}

/// Resolve a `use` path against the running VM's project context.
///
/// A thin adapter over [`crate::project::resolve_import`], which is also what
/// `jade check` walks the import graph with. Sharing the function is the point:
/// a `use` that check accepts and the VM then cannot find is precisely the bug
/// this indirection prevents.
///
/// The stdlib case never reaches here — `Instr::ImportFile` binds a built-in
/// package before calling this — so a `Builtin` target is unreachable in
/// practice and is mapped to a not-found rather than given a variant of its own.
pub(crate) fn resolve_user_import(
    state: &VmState,
    path: &str,
    span: Span,
) -> Result<ResolvedImport> {
    let ctx = crate::project::ImportContext {
        libraries: &state.libraries,
        project_root: state.project_root.as_deref(),
        source_dir: state.source_dir.clone(),
    };
    match crate::project::resolve_import(&ctx, path) {
        Ok(crate::project::ImportTarget::Native(p)) => Ok(ResolvedImport::Native(p)),
        Ok(crate::project::ImportTarget::Jade(p)) => Ok(ResolvedImport::File(p)),
        Ok(crate::project::ImportTarget::Builtin) => {
            Err(JadeError::ImportNotFound { path: path.to_string(), span })
        }
        Err(message) => Err(JadeError::IoError { message, span }),
    }
}

/// Native stack the interpreter needs, applied to the tokio worker threads
/// `main` builds its runtime with.
///
/// `call::MAX_CALL_DEPTH` (10,000) is meant to be what stops a runaway
/// recursion — never the OS underneath it — so the stack has to hold that many
/// frames with room to spare. Measured on this crate: a release build spends
/// ~12.4 KB of native stack per nested `call_fn`, a debug build ~137 KB, since
/// debug's async state machines carry unoptimized locals the optimizer
/// otherwise collapses. So the two builds need very different numbers, and
/// each is sized to double what its own rate demands.
///
/// Reserving address space costs nothing until it is touched: a 64-bit process
/// does not commit stack pages it never writes.
///
/// This is set on the *runtime's* threads rather than by spawning a thread of
/// our own here. Interpretation is async, and a thread that borrows the
/// caller's `Handle` and calls `block_on` deadlocks against a current-thread
/// runtime — the caller is parked in `join()` and so nothing is left driving
/// the reactor, which hangs every prompt-stream test outright.
#[cfg(debug_assertions)]
pub const VM_STACK_SIZE: usize = 3 * 1024 * 1024 * 1024;
#[cfg(not(debug_assertions))]
pub const VM_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Execute a compiled program and return the populated global state.
pub async fn run(program: CompiledProgram, opts: VmOpts) -> Result<VmState> {
    let mut state = VmState::new();
    state.apply_opts(opts);
    run_with_state(program, &mut state).await?;
    Ok(state)
}

/// Execute a compiled program against an existing `VmState`.
///
/// This is the public entry point for the REPL — it lets each snippet share
/// globals, struct definitions, and extend-block methods with prior snippets.
pub async fn run_incremental(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    run_with_state(program, state).await
}

/// Execute a compiled program against an existing `VmState`.
/// Used internally for imports so they share globals/struct_defs/extend_methods.
pub(crate) async fn run_with_state(program: CompiledProgram, state: &mut VmState) -> Result<()> {
    // Merge compile-time metadata into the shared state.
    for (k, v) in program.struct_defs {
        state.globals.entry(k.clone()).or_insert_with(|| VmValue::TypeRef(k.clone()));
        state.struct_defs.insert(k, v);
    }
    for (type_name, methods) in program.extend_methods {
        state.extend_methods.entry(type_name).or_default().extend(methods);
    }
    for (k, v) in program.struct_ancestors {
        state.struct_ancestors.insert(k, v);
    }

    let mut slots: Vec<VmValue> = vec![VmValue::Nil; program.top_n_slots as usize];
    execute_chunk(&program.top, &mut slots, state).await?;
    Ok(())
}

/// Recursively stamp `source_file` onto every `CompiledFn` reachable from
/// `chunk`. Called on freshly-compiled import modules so that runtime errors
/// inside those functions can be attributed to the correct file.
pub(crate) fn stamp_source_file(chunk: &mut Chunk, file: &str) {
    for fn_arc in &mut chunk.fn_defs {
        let cf = Arc::make_mut(fn_arc);
        if cf.source_file.is_empty() {
            cf.source_file = file.to_string();
        }
        stamp_source_file(&mut cf.chunk, file);
    }
}
