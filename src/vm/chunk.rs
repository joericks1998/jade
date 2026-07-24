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

pub(crate) fn resolve_user_import(state: &VmState, path: &str, span: Span) -> Result<ResolvedImport> {
    if let Some(root) = &state.project_root {
        if let Some(message) =
            crate::project::ambiguous_bare_import(path, &state.libraries, &state.source_dir)
        {
            return Err(JadeError::IoError { message, span });
        }
        match crate::project::resolve_library_import(&state.libraries, path, root) {
            Ok(Some(r)) => {
                return Ok(match r.kind {
                    crate::project::ImportKind::Native => ResolvedImport::Native(r.path),
                    crate::project::ImportKind::Jade => ResolvedImport::File(r.path),
                });
            }
            Ok(None) => {}
            Err(message) => return Err(JadeError::IoError { message, span }),
        }
    }
    // Not a registered library: resolve relative to the importing file. `path` is
    // a module stem (`utils`, `sub/helper`) — probe `<path>.jde`, then a native
    // library, mirroring an allowlist-free `[lib]` directory.
    let r = crate::project::resolve_relative_import(&state.source_dir, path);
    Ok(match r.kind {
        crate::project::ImportKind::Native => ResolvedImport::Native(r.path),
        crate::project::ImportKind::Jade => ResolvedImport::File(r.path),
    })
}

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
    for (k, v) in program.struct_decorators {
        state.struct_decorators.insert(k, v);
    }
    for (type_name, methods) in program.extend_methods {
        state.extend_methods.entry(type_name).or_default().extend(methods);
    }
    for (k, v) in program.route_configs {
        state.route_configs.insert(k, v);
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
