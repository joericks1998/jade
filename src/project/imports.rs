//! Resolving a `use` path, and walking the import graph ahead of execution.
//!
//! [`resolve_import`] is the single answer to "what does this `use` name?" —
//! a stdlib package, a native shared library, or a Jade source file. The VM's
//! `Instr::ImportFile` handler reaches it through `vm::chunk::resolve_user_import`,
//! and [`walk_imports`] uses it to answer the same question for every import in
//! a program without running any of them.
//!
//! [`walk_imports`] exists because `jade check` could not previously tell you
//! whether a program's imports resolve. Import resolution was not a compile
//! stage at all: the VM resolved a `use` when the `Import` opcode executed, and
//! the AOT backend when it flattened imports, so `jade check` — which stops at
//! bytecode emission — never looked. A file naming a module that does not exist
//! reported `ok` and then failed at run time with "cannot find import". That
//! made `check` unusable as a gate, which is the one job it has.
//!
//! The walk deliberately does **not** load anything. A native module is checked
//! for existence only, never `dlopen`ed, because opening a shared library runs
//! its initializer and `check` must not execute a program's code. For the same
//! reason a Jade module is lexed and parsed to find its own imports, but not
//! type-checked here — `check` type-checks the entry file, and a module's own
//! errors surface when it is checked in its own right.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use super::{
    ImportKind, LibraryEntry, ambiguous_bare_import, resolve_library_import,
    resolve_relative_import,
};
use crate::frontend::{
    ast::{Program, Stmt},
    error::{JadeError, Span},
};
use std::collections::HashMap;

/// What a `use` path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// A built-in `std::*` package. Compiled into the binary; nothing on disk.
    Builtin,
    /// A native C-ABI shared library, loaded over the FFI.
    Native(PathBuf),
    /// A Jade source module.
    Jade(PathBuf),
}

/// The context an import resolves against: the registered `[lib]` entries, the
/// project root they are anchored at (if the file belongs to a project), and the
/// directory of the file doing the importing.
///
/// Bundled into one struct because all three always travel together, and because
/// `source_dir` changes as the walk descends into an imported module while the
/// other two do not — a distinction that is easy to get wrong when they are
/// three loose arguments.
pub struct ImportContext<'a> {
    pub libraries: &'a HashMap<String, LibraryEntry>,
    pub project_root: Option<&'a Path>,
    pub source_dir: PathBuf,
}

/// Resolve one `use` path to what it names, without loading it.
///
/// The order matters and matches what the VM has always done: a stdlib package
/// wins outright, then a registered `[lib]` module anchored at the project root,
/// then a file sitting beside the importer. Neither of the latter two touches
/// disk beyond `exists()`.
///
/// A returned [`ImportTarget::Native`] or [`ImportTarget::Jade`] path is a
/// *candidate*: the underlying resolvers hand back a `.jde` candidate when
/// nothing is on disk, so the caller still has to confirm the file is there.
/// [`walk_imports`] does.
pub fn resolve_import(ctx: &ImportContext, path: &str) -> Result<ImportTarget, String> {
    // Stdlib packages are compiled in and always win — the VM binds them under
    // their own global name before it ever consults the filesystem.
    if crate::builtins::find_package(path).is_some() {
        return Ok(ImportTarget::Builtin);
    }

    if let Some(root) = ctx.project_root {
        if let Some(message) = ambiguous_bare_import(path, ctx.libraries, &ctx.source_dir) {
            return Err(message);
        }
        if let Some(r) = resolve_library_import(ctx.libraries, path, root)? {
            return Ok(match r.kind {
                ImportKind::Native => ImportTarget::Native(r.path),
                ImportKind::Jade => ImportTarget::Jade(r.path),
            });
        }
    }

    let r = resolve_relative_import(&ctx.source_dir, path);
    Ok(match r.kind {
        ImportKind::Native => ImportTarget::Native(r.path),
        ImportKind::Jade => ImportTarget::Jade(r.path),
    })
}

/// Every `use` / `from ... use` path in a program, with the span to blame.
///
/// Only top-level statements are inspected: an import is a top-level form, so a
/// nested one cannot exist to be missed.
pub fn program_import_paths(program: &Program) -> Vec<(String, Span)> {
    program
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Use { path, span, .. } | Stmt::FromUse { path, span, .. } => {
                Some((path.clone(), span.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Check that every import reachable from these paths resolves to something
/// that exists, following Jade modules transitively.
///
/// Takes the entry file's import paths rather than its AST so a caller holding
/// only typed IR can use it too — `jade check` reaches this from its TIR cache
/// hit, where no AST was ever built. Imported modules are parsed as it descends,
/// so their paths come from an AST regardless.
///
/// Returns the first unresolvable import as a [`JadeError`] carrying its span,
/// so `jade check` reports it the way `jade run` would. A module that fails to
/// lex or parse is reported as its own error rather than swallowed — but note a
/// module is only *parsed* here, to find its imports. Type errors inside it are
/// not this function's business.
///
/// A cycle is guarded against, not reported. A circular import raises
/// `CircularImport` in the VM at run time; whether check should reject it up
/// front is a separate question from whether the files exist, so the walk simply
/// stops rather than recursing forever.
pub fn walk_imports(paths: &[(String, Span)], ctx: &ImportContext) -> Result<(), JadeError> {
    reachable_jade_modules(paths, ctx).map(|_| ())
}

/// The same walk, keeping the set it visits: every Jade module reachable from
/// `paths`, canonicalized.
///
/// The walk has always built this set to guard against cycles and then thrown it
/// away. `jade build --lib` needs it to check a package's declared `sources`
/// against the files actually pulled in, and asking the *frontend* rather than
/// the LLVM backend keeps that check off the compile path — the answer is a
/// property of the import graph, not of code generation.
///
/// The entry file itself is not included: it is the root of the walk, not
/// something the walk reached. Callers that want the whole package add it.
pub fn reachable_jade_modules(
    paths: &[(String, Span)],
    ctx: &ImportContext,
) -> Result<HashSet<PathBuf>, JadeError> {
    let mut seen = HashSet::new();
    walk(paths, ctx, &mut seen)?;
    Ok(seen)
}

fn walk(
    paths: &[(String, Span)],
    ctx: &ImportContext,
    seen: &mut HashSet<PathBuf>,
) -> Result<(), JadeError> {
    for (path, span) in paths.iter().cloned() {
        let target =
            resolve_import(ctx, &path).map_err(|message| JadeError::IoError { message, span })?;

        let file = match target {
            // Compiled in — nothing to look for.
            ImportTarget::Builtin => continue,
            // Existence only. Opening it would run the library's initializer,
            // and `check` does not execute the program it is checking.
            ImportTarget::Native(p) => {
                if !p.exists() {
                    return Err(JadeError::ImportNotFound { path, span });
                }
                continue;
            }
            ImportTarget::Jade(p) => p,
        };

        let Ok(canon) = file.canonicalize() else {
            return Err(JadeError::ImportNotFound { path, span });
        };
        if !seen.insert(canon.clone()) {
            continue;
        }

        // Descend. The module's own imports resolve relative to *its* directory,
        // not the importer's, which is what makes a chain of sibling imports in
        // different directories work.
        let Ok(source) = std::fs::read_to_string(&canon) else {
            return Err(JadeError::ImportNotFound { path, span });
        };
        let tokens = crate::frontend::lexer::tokenize(&source)?;
        let sub = crate::frontend::parser::parse(tokens)?;
        let sub_ctx = ImportContext {
            libraries: ctx.libraries,
            project_root: ctx.project_root,
            source_dir: canon.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
        };
        walk(&program_import_paths(&sub), &sub_ctx, seen)?;
    }
    Ok(())
}
