//! Import resolution + module namespacing for the LLVM backend.
//!
//! ## Why this exists
//!
//! Every imported file has its **own namespace**: `use "a.jde" as a` binds its
//! symbols under `a` (`a.greet`, struct defs as `a.Foo`, methods as `a.Foo`), so
//! two different modules can each define `greet` and stay distinct. Until v1.4.8
//! the bytecode VM ran each import in an isolated sub-state, and this pass was
//! written to match it.
//!
//! A compiled program has no runtime namespaces: everything collapses into one
//! LLVM module. The previous pass flattened every imported file into one global
//! stream and rewrote `a.greet` to bare `greet`, which **fused all namespaces
//! together**: two modules each defining `greet` collided (last definition won,
//! or LLVM rejected the duplicate symbol).
//!
//! ## What this pass does instead
//!
//! It inlines imported files (AOT still needs one module) but **mangles every
//! imported module's globals by a per-module id**, so distinct modules never
//! collide. `main` keeps bare names, since it is the root namespace.
//!
//! Mangling scheme: `name$<id>`. `$` is valid in LLVM symbol names but is **never
//! produced by the Jade lexer** (identifiers are `[A-Za-z0-9_]`), so a mangled name
//! can never collide with a user-written one. It is also untouched by codegen's
//! existing name-splitting conventions:
//!   * `rsplit_once('.')` — type / decorator namespace stripping
//!   * `split_once("__")` — `Type__method` extend-method keys
//!
//! so `Foo$2` stays intact and methods key as `Foo$2__method`.
//!
//! ## Coverage
//!
//! The mangle is scope-aware and crosses every name-bearing position:
//!   * **values** — `let` / `fn` / `async fn` / `prompt` names, identifier refs,
//!     `alias.foo` accesses, decorator targets, `from "x" use a` bindings.
//!   * **types**  — `struct` / `interface` names, `StructLiteral.type_name`,
//!     `extend` `type_name` / `interface_name`, `JadeType::Struct(_)` embedded in
//!     every `TExpr.ty` / `ret_ty`, `?p |> Type` output types, `catch Type`.
//!   * **AST defaults** — `StructFieldDef::Let/Prompt` defaults are raw AST `Expr`
//!     (inlined into literals later by `fill_struct_literal_defaults`), so they get
//!     their own walker.
//!
//! Scope-awareness: a bare identifier is only mangled when it resolves to a
//! *module-global* and is **not shadowed** by a function param, `let`, loop var,
//! closure param, or catch binding. Get this wrong and you clobber locals.
//!
//! Known gap (alpha, tune later): closures appearing *inside* a struct-field
//! default expression have their `Vec<Stmt>` body left un-mangled — vanishingly
//! rare and not worth an entire AST-statement walker yet.

use crate::typechecker::infer::native::NativeSig;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::frontend::ast::{Expr, FStrPart, StructFieldDef};
use crate::frontend::error::{JadeError, Span};
use crate::typechecker::tir::{JadeType, TExpr, TExprKind, TFStrPart, TStmt};

/// Mangle a module-global `name` into module `id`'s namespace.
fn mangle(name: &str, id: u32) -> String {
    format!("{name}${id}")
}

/// stdlib packages are handled by a separate codegen dispatch path — they never
/// participate in source-file flattening, and their `ns.foo` accesses stay
/// qualified for the stdlib path to resolve.
fn is_stdlib(path: &str) -> bool {
    crate::builtins::find_package(path).is_some()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve every `use "file.jde"` reachable from `source_path`, inline the
/// imported files (mangled into per-module namespaces), and return one flat,
/// fully-resolved statement stream for the LLVM backend. `main`'s own globals
/// keep bare names; imported modules' globals are mangled `name$<id>`.
/// Returns the inlined statement stream, and for a program that uses a Rust
/// package, the library to link and each function's signature. A reference to a
/// Rust function is rewritten to the canonical identifier
/// `__native$<pkgid>$<fnname>` (see [`Renamer`]), which the checker types from
/// the package's record and the backend lowers to a call of its wrapper.
pub fn resolve_and_namespace(
    stmts: Vec<TStmt>,
    source_path: &Path,
) -> Result<ResolvedImports, ResolveError> {
    let main_canon =
        source_path.canonicalize().map_err(|e| format!("{}: {e}", source_path.display()))?;
    let main_dir = main_canon.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

    let mut reg = Registry::new();
    // Load registered [lib] libraries from the project's jade.toml (walking up
    // from the main file), so `use "<lib>/<module>"` resolves the way `jade
    // check` resolves it. Locked dependencies join them as synthetic [lib]
    // entries, so `use <dep>` lowers exactly like a hand-written library.
    if let Some((root, manifest)) = crate::project::find_project_root_from(&main_dir)
        .and_then(|r| crate::project::load_project(&r).ok().map(|m| (r, m)))
    {
        reg.libraries = crate::pkg::resolved_libraries(&root, &manifest);
        reg.manifest = Some(manifest);
        reg.lib_root = Some(root);
    }

    // Mark main as loaded so a self-import is treated as a cycle (skipped), not
    // re-inlined under a fresh id.
    reg.loaded.insert(main_canon);

    // main carries self_id = None → its own names are never mangled (root namespace).
    let main_stmts =
        process_file(&mut reg, stmts, &main_dir, None, HashSet::new(), HashSet::new())?;

    // Imported modules were appended to `reg.out` in dependency order (deps before
    // dependents); main's statements come last.
    let rust = reg.rust.as_ref().filter(|_| !reg.rust_ids.is_empty()).map(|b| b.link());
    let mut out = reg.out;
    out.extend(main_stmts);
    Ok(ResolvedImports { stmts: out, rust, native_sigs: reg.native_sigs })
}

/// Purpose: This function namespaces a module given as source text rather
/// than a file, such as the inference prelude, so its names cannot meet a
/// program's.
///
/// Inputs:
/// - `source`: the module's source.
/// - `label`: what to call it in a message.
/// - `id`: the namespace id its globals are mangled with, as `name$<id>`.
///
/// Outputs: its statements, mangled, or why it does not compile. It may import
/// `std` packages only.
pub fn namespace_source(source: &str, label: &str, id: u32) -> Result<Vec<TStmt>, String> {
    let tokens = crate::frontend::lexer::tokenize(source).map_err(|e| format!("{label}: {e}"))?;
    let ast = crate::frontend::parser::parse(tokens).map_err(|e| format!("{label}: {e}"))?;
    let tp = crate::typechecker::type_infer::infer(ast).map_err(|e| format!("{label}: {e}"))?;
    let (values, types) = collect_globals(&tp.stmts);
    let mut reg = Registry::new();
    let body = process_file(&mut reg, tp.stmts, Path::new("."), Some(id), values, types)
        .map_err(|e| format!("{label}: {e}"))?;
    let mut out = reg.out;
    out.extend(body);
    Ok(out)
}

/// Why import resolution failed.
///
/// The two cases need telling apart because `aot::would_build` probes a program
/// and must stay quiet about the first: an import that does not resolve means a
/// dependency has not been installed yet, and `check_imports` already says so in
/// words that name the real problem.
///
/// A `Program` error is the opposite. Nothing else reports it, so swallowing it
/// is exactly how a mistyped FFI symbol used to reach run time — `jade check`
/// probed the build, threw the answer away, and reported `ok`.
#[derive(Debug)]
pub enum ResolveError {
    /// An import did not name anything on disk.
    Unresolved(String),
    /// The program is wrong in a way this pass can see.
    Program(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ResolveError::Unresolved(m) | ResolveError::Program(m) => write!(f, "{m}"),
        }
    }
}

/// Anything reported as a bare string is a resolution failure. The program
/// errors are raised explicitly, at the two places that can find one, so `?` on
/// the path-resolving helpers keeps meaning what it always did.
impl From<String> for ResolveError {
    fn from(m: String) -> Self {
        ResolveError::Unresolved(m)
    }
}

/// What one resolution pass produced.
pub struct ResolvedImports {
    pub stmts: Vec<TStmt>,
    /// The staticlib holding the project's Rust packages, when the program
    /// uses one, for the backend to link.
    pub rust: Option<crate::rustpkg::Link>,
    /// What each Rust function this program can call takes and returns, by
    /// `__native$<pkgid>$<fn>`.
    pub native_sigs: HashMap<String, NativeSig>,
}

// ── Module registry ───────────────────────────────────────────────────────────

/// Per-module bookkeeping, keyed by canonical file path. `main` is intentionally
/// absent (it has no id and is never mangled).
struct Registry {
    next_id: u32,
    id: HashMap<PathBuf, u32>,
    values: HashMap<PathBuf, HashSet<String>>,
    types: HashMap<PathBuf, HashSet<String>>,
    loaded: HashSet<PathBuf>,
    /// Inlined, mangled statements from imported files, in dependency order.
    out: Vec<TStmt>,
    /// Project root (anchor for `[lib]` imports) and the registered libraries
    /// from `jade.toml`. Empty when the build isn't inside a project.
    lib_root: Option<PathBuf>,
    libraries: HashMap<String, crate::project::LibraryEntry>,
    /// The project's manifest, whose Rust dependencies the build compiles.
    manifest: Option<crate::project::ProjectManifest>,
    /// The project's Rust packages, built the first time a `use` names one.
    rust: Option<crate::rustpkg::Build>,
    /// Each Rust package a `use` named, by its dependency name, and the id its
    /// functions' references carry. Its own id space, distinct from `.jde`
    /// module ids (native refs always carry the `__native$` prefix).
    rust_ids: HashMap<String, u32>,
    native_next_id: u32,
    /// The classes each Rust package defines, by pkgid, then the name the
    /// program writes after the package's alias.
    native_types: HashMap<u32, HashMap<String, String>>,
    /// Each Rust function's signature by `__native$<pkg>$<fn>`, for the
    /// packages this build uses.
    native_sigs: HashMap<String, NativeSig>,
    /// The names each Rust package exports, functions and structs, keyed by
    /// pkgid, which is the form the renamer needs to check `alias.name`.
    native_symbols: HashMap<u32, HashSet<String>>,
}

/// One decorator on a TIR item: its name, and its positional arguments.
///
/// The AST spells the same shape as `ast::DecoratorList`; this is the TIR's,
/// over `TExpr`.
type TDecorator = (String, Vec<(Option<String>, TExpr)>);

impl Registry {
    fn new() -> Self {
        Registry {
            next_id: 0,
            id: HashMap::new(),
            values: HashMap::new(),
            types: HashMap::new(),
            loaded: HashSet::new(),
            out: Vec::new(),
            lib_root: None,
            libraries: HashMap::new(),
            manifest: None,
            rust: None,
            rust_ids: HashMap::new(),
            native_next_id: 0,
            native_types: HashMap::new(),
            native_sigs: HashMap::new(),
            native_symbols: HashMap::new(),
        }
    }

    fn fresh_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Purpose: This function assigns (or reuses) the pkgid of a Rust package,
    /// building the project's Rust packages the first time one is named.
    ///
    /// Inputs:
    /// - `name`: the package's dependency name, which is what `use` wrote.
    ///
    /// Outputs: the pkgid, or why the packages could not be built or read.
    /// Building emits a class for every struct the library exports, once.
    fn rust_pkg_id(&mut self, name: &str) -> Result<u32, ResolveError> {
        if let Some(&id) = self.rust_ids.get(name) {
            return Ok(id);
        }
        if self.rust.is_none() {
            let (Some(root), Some(manifest)) = (&self.lib_root, &self.manifest) else {
                return Err(ResolveError::Program(format!(
                    "'{name}' names a Rust package, which only a project can depend on"
                )));
            };
            let built = crate::rustpkg::build(root, manifest).map_err(ResolveError::Program)?;
            let Some(built) = built else {
                return Err(ResolveError::Program(format!(
                    "'{name}' names no Rust package in jade.toml"
                )));
            };
            let classes = rust_classes(&built);
            for s in built.structs.values() {
                self.out.push(rust_class(s, &classes));
            }
            self.rust = Some(built);
        }
        let Some(built) = &self.rust else {
            return Err(ResolveError::Program(format!("the Rust package '{name}' was not built")));
        };
        let Some(pkg) = built.packages.get(name) else {
            return Err(ResolveError::Program(format!(
                "'{name}' is not a Rust dependency in jade.toml; add it under [dependencies] \
                 with abi = \"rust\""
            )));
        };
        let id = self.native_next_id;
        self.native_next_id += 1;
        self.rust_ids.insert(name.to_string(), id);
        let classes = rust_classes(built);
        for f in pkg.functions.values() {
            let sig = crate::typechecker::infer::native::rust_sig(
                &format!("{name}.{}", f.name),
                f,
                &classes,
            )
            .map_err(ResolveError::Program)?;
            self.native_sigs.insert(native_ref(id, &f.name), sig);
        }
        let types: HashMap<String, String> = pkg
            .structs
            .iter()
            .filter_map(|(bare, key)| Some((bare.clone(), classes.get(key)?.clone())))
            .collect();
        let mut symbols: HashSet<String> = pkg.functions.keys().cloned().collect();
        symbols.extend(types.keys().cloned());
        self.native_types.insert(id, types);
        self.native_symbols.insert(id, symbols);
        Ok(id)
    }
}

/// Parse + type-infer an imported file, returning its TIR statements.
fn parse_and_infer(canon: &Path) -> Result<Vec<TStmt>, ResolveError> {
    let src =
        std::fs::read_to_string(canon).map_err(|e| format!("import '{}': {e}", canon.display()))?;
    let tokens = crate::frontend::lexer::tokenize(&src).map_err(|e| e.to_string())?;
    let ast = crate::frontend::parser::parse(tokens).map_err(|e| e.to_string())?;
    let tp = crate::typechecker::type_infer::infer(ast).map_err(|e| e.to_string())?;
    Ok(tp.stmts)
}

/// Collect a file's top-level global names, split into the value namespace
/// (let / fn / async fn / prompt) and the type namespace (struct / interface).
fn collect_globals(stmts: &[TStmt]) -> (HashSet<String>, HashSet<String>) {
    let mut values = HashSet::new();
    let mut types = HashSet::new();
    for s in stmts {
        match s {
            TStmt::Let { name, .. }
            | TStmt::FnDef { name, .. }
            | TStmt::AsyncFnDef { name, .. }
            | TStmt::PromptDecl { name, .. } => {
                values.insert(name.clone());
            }
            TStmt::StructDef { name, .. } => {
                types.insert(name.clone());
            }
            _ => {}
        }
    }
    (values, types)
}

/// Ensure `dep_canon` is loaded: parse, assign an id, record its globals, mangle
/// its body, and append it to `reg.out`. Idempotent (diamond imports inline once).
fn load_dep(reg: &mut Registry, dep_canon: &Path) -> Result<(), ResolveError> {
    if reg.loaded.contains(dep_canon) {
        return Ok(());
    }
    reg.loaded.insert(dep_canon.to_path_buf());

    let stmts = parse_and_infer(dep_canon)?;
    let id = reg.fresh_id();
    reg.id.insert(dep_canon.to_path_buf(), id);

    let (values, types) = collect_globals(&stmts);
    reg.values.insert(dep_canon.to_path_buf(), values.clone());
    reg.types.insert(dep_canon.to_path_buf(), types.clone());

    let dep_dir = dep_canon.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

    // Recurse first (post-order): this module's own deps land in `reg.out` before
    // it does, so forward references resolve.
    let mangled = process_file(reg, stmts, &dep_dir, Some(id), values, types)?;
    reg.out.extend(mangled);
    Ok(())
}

/// Resolve the imports of a single file and mangle its body. Returns the mangled,
/// import-free statements (the caller decides where to splice them).
fn process_file(
    reg: &mut Registry,
    stmts: Vec<TStmt>,
    dir: &Path,
    self_id: Option<u32>,
    self_values: HashSet<String>,
    self_types: HashSet<String>,
) -> Result<Vec<TStmt>, ResolveError> {
    let mut aliases: HashMap<String, (u32, HashSet<String>, HashSet<String>)> = HashMap::new();
    let mut from_value: HashMap<String, String> = HashMap::new();
    let mut from_type: HashMap<String, String> = HashMap::new();
    // Rust package imports: `use pkg` → `pkg` -> pkgid; `from pkg use f` → `f`
    // -> pkgid. The renamer rewrites references through these to
    // `__native$<pkgid>$<fn>`.
    let mut native_aliases: HashMap<String, u32> = HashMap::new();
    let mut native_from: HashMap<String, u32> = HashMap::new();
    // Alias → the source package and the names it does not export.
    let mut hidden: HashMap<String, (String, HashSet<String>)> = HashMap::new();
    let mut kept: Vec<TStmt> = Vec::new();

    for s in stmts {
        match s {
            TStmt::Use { path, as_name, .. } => {
                // stdlib: drop the `use`; the `ns.foo` access stays qualified for
                // stdlib dispatch.
                if is_stdlib(&path) {
                    continue;
                }
                match resolve_use(reg, dir, &path)? {
                    Resolved::Rust(name) => {
                        let pkgid = reg.rust_pkg_id(&name)?;
                        let alias = as_name.unwrap_or_else(|| stem(&path));
                        native_aliases.insert(alias, pkgid);
                        // No body to inline — the package is linked in.
                    }
                    Resolved::Jade(dep, exports) => {
                        load_dep(reg, &dep)?;
                        let alias = as_name.unwrap_or_else(|| stem(&path));
                        let id = reg.id[&dep];
                        let (mut values, mut types) =
                            (reg.values[&dep].clone(), reg.types[&dep].clone());
                        if let Some(ex) = exports {
                            let kept = |set: &mut HashSet<String>| {
                                let gone: HashSet<String> = set
                                    .iter()
                                    .filter(|n| !ex.names.contains(*n))
                                    .cloned()
                                    .collect();
                                set.retain(|n| ex.names.contains(n));
                                gone
                            };
                            let mut gone = kept(&mut values);
                            gone.extend(kept(&mut types));
                            hidden.insert(alias.clone(), (ex.package.clone(), gone));
                        }
                        aliases.insert(alias, (id, values, types));
                        // `use` itself is dropped — the body is inlined+mangled already.
                    }
                }
            }
            TStmt::FromUse { path, names, path_is_string, span } => {
                // stdlib from-imports are preserved verbatim for the stdlib
                // dispatch path (parity with the prior pass).
                if is_stdlib(&path) {
                    kept.push(TStmt::FromUse { path, names, path_is_string, span });
                    continue;
                }
                match resolve_use(reg, dir, &path)? {
                    Resolved::Rust(name) => {
                        let pkgid = reg.rust_pkg_id(&name)?;
                        // A struct the package exports is a class, not a function.
                        let types = reg.native_types.get(&pkgid).cloned().unwrap_or_default();
                        let names: Vec<String> = names
                            .into_iter()
                            .filter(|n| match types.get(n) {
                                Some(class) => {
                                    from_type.insert(n.clone(), class.clone());
                                    false
                                }
                                None => true,
                            })
                            .collect();
                        // The name is right here, so an undeclared one is caught
                        // now rather than waiting for a reference to it.
                        if let Some(declared) = reg.native_symbols.get(&pkgid) {
                            for n in &names {
                                if !declared.contains(n) {
                                    return Err(ResolveError::Program(
                                        JadeError::UnknownFfiSymbol {
                                            module: path.clone(),
                                            symbol: n.clone(),
                                            suggestion: closest_symbol(n, declared.iter()),
                                            span,
                                        }
                                        .to_string(),
                                    ));
                                }
                            }
                        }
                        for n in &names {
                            native_from.insert(n.clone(), pkgid);
                        }
                        // `from ... use` is dropped — selected names map to native refs.
                    }
                    Resolved::Jade(dep, exports) => {
                        load_dep(reg, &dep)?;
                        let id = reg.id[&dep];
                        if let Some(ex) = &exports
                            && let Some(n) = names.iter().find(|n| !ex.names.contains(*n))
                        {
                            return Err(ResolveError::Program(
                                JadeError::NotExported {
                                    package: ex.package.clone(),
                                    name: n.clone(),
                                    span,
                                }
                                .to_string(),
                            ));
                        }
                        for n in &names {
                            if reg.values[&dep].contains(n) {
                                from_value.insert(n.clone(), mangle(n, id));
                            }
                            if reg.types[&dep].contains(n) {
                                from_type.insert(n.clone(), mangle(n, id));
                            }
                        }
                        // `from ... use` is dropped — selected names map to mangled targets.
                    }
                }
            }
            other => kept.push(other),
        }
    }

    let native_types = native_aliases
        .iter()
        .filter_map(|(alias, id)| Some((alias.clone(), reg.native_types.get(id)?.clone())))
        .collect();
    let mut renamer = Renamer {
        native_types,
        self_id,
        self_values,
        self_types,
        aliases,
        from_value,
        from_type,
        native_aliases,
        native_from,
        hidden,
        native_symbols: reg.native_symbols.clone(),
        errors: Vec::new(),
        locals: Vec::new(),
    };
    for s in kept.iter_mut() {
        renamer.rename_stmt(s, true);
    }
    // Report the first undeclared FFI symbol, in source order. One at a time
    // rather than all of them: they are usually the same typo, and the message
    // names the fix.
    if let Some(first) = renamer.errors.into_iter().next() {
        return Err(ResolveError::Program(first));
    }
    Ok(kept)
}

/// A resolved `use` target: a Rust package, by its dependency name, or a Jade
/// source module (inlined + mangled).
enum Resolved {
    Rust(String),
    /// A Jade module, and the names an importer may use when it is a source
    /// package's entry module that lists its exports.
    Jade(PathBuf, Option<Exports>),
}

/// What a source package lets other projects use from its entry module.
#[derive(Clone)]
struct Exports {
    /// The package's name, for messages.
    package: String,
    names: HashSet<String>,
}

/// Resolve a `use` path to a canonical file. Registered `[lib]` libraries take
/// precedence (anchored at the project root, enabling cross-directory imports);
/// everything else resolves relative to the importing file's `dir`. A Rust
/// package resolves through the `[lib]` entry its dependency became.
fn resolve_use(reg: &mut Registry, dir: &Path, path: &str) -> Result<Resolved, String> {
    // A file inside a source package resolves against the package's own
    // `[lib]` entries first, anchored at the package, then the build's.
    if let Some((pkg_root, manifest)) = package_context(reg, dir)
        && let Some(libs) = &manifest.lib
        && let Some(r) = crate::project::resolve_library_import(libs, path, &pkg_root)?
    {
        if r.kind == crate::project::ImportKind::Rust {
            return Ok(Resolved::Rust(r.path.to_string_lossy().into_owned()));
        }
        let canon = r.path.canonicalize().map_err(|e| format!("import '{}': {e}", path))?;
        return Ok(Resolved::Jade(canon, None));
    }
    if let Some(message) = crate::project::ambiguous_bare_import(path, &reg.libraries, dir) {
        return Err(message);
    }
    // Not a registered library: resolve relative to the importing file. `path` is
    // a module stem (`utils`, `sub/helper`), so probe `<path>.jde`.
    let relative = || crate::project::resolve_relative_import(dir, path).path;
    let mut exports = None;
    let target = match &reg.lib_root {
        Some(root) => match crate::project::resolve_library_import(&reg.libraries, path, root)? {
            Some(r) if r.kind == crate::project::ImportKind::Rust => {
                return Ok(Resolved::Rust(r.path.to_string_lossy().into_owned()));
            }
            Some(r) => {
                exports = r.exports.map(|names| Exports {
                    package: path.to_string(),
                    names: names.into_iter().collect(),
                });
                r.path
            }
            None => relative(),
        },
        None => relative(),
    };
    let canon = target.canonicalize().map_err(|e| format!("import '{}': {e}", path))?;
    Ok(Resolved::Jade(canon, exports))
}

/// Purpose: This function finds the source package a directory belongs to,
/// when it is not the project being built.
///
/// Inputs:
/// - `reg`: the registry, which knows the project's root.
/// - `dir`: the importing file's directory.
///
/// Outputs: the package's root and manifest, or `None` for a file of the
/// project itself or one outside any package.
fn package_context(
    reg: &Registry,
    dir: &Path,
) -> Option<(PathBuf, crate::project::ProjectManifest)> {
    let root = crate::project::find_project_root_from(dir)?;
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    if reg.lib_root.as_deref().is_some_and(|r| same(r, &root)) {
        return None;
    }
    let manifest = crate::project::load_project(&root).ok()?;
    manifest.package.is_some().then_some((root, manifest))
}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or(path).to_string()
}

/// Purpose: This function maps each struct a Rust library exports to its class.
///
/// Inputs:
/// - `b`: the build.
///
/// Outputs: each struct by the names a type is written with, `crate::Name` in a
/// record and the class's own name in a class's field, to the class's name.
fn rust_classes(b: &crate::rustpkg::Build) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, s) in &b.structs {
        let class = crate::rustpkg::class_name(&s.krate, &s.name);
        out.insert(key.clone(), class.clone());
        out.insert(class.clone(), class);
    }
    out
}

/// Purpose: This function spells a record's type with each struct written as its
/// class, the form a class field's type decorator takes.
///
/// Inputs:
/// - `t`: the type, such as `[geo::Point]?`.
/// - `classes`: as [`rust_classes`] gives them.
///
/// Outputs: the type, such as `[Point$rgeo]?`.
fn class_type_text(t: &str, classes: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        out.push_str(classes.get(word.as_str()).map_or(word.as_str(), String::as_str));
        word.clear();
    };
    for c in t.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '$' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Purpose: This function builds the class a Rust struct becomes.
///
/// Inputs:
/// - `s`: the struct's record.
/// - `classes`: as [`rust_classes`] gives them.
///
/// Outputs: a class whose fields are the struct's, in order, every one public,
/// typed, and required, since Rust gives no field a default.
fn rust_class(s: &crate::rustpkg::Struct, classes: &HashMap<String, String>) -> TStmt {
    use crate::frontend::ast::ClassMembers;
    let span = Span { line: 0, col: 0 };
    TStmt::StructDef {
        name: crate::rustpkg::class_name(&s.krate, &s.name),
        fields: s.fields.iter().map(|f| StructFieldDef::Required(f.name.clone())).collect(),
        parents: Vec::new(),
        members: ClassMembers {
            decorators: s
                .fields
                .iter()
                .map(|f| {
                    (f.name.clone(), vec!["public".to_string(), class_type_text(&f.ty, classes)])
                })
                .collect(),
            declared: Vec::new(),
        },
        span,
    }
}

/// The declared symbol closest to `wanted`, when one is close enough to name.
///
/// Bounded edit distance rather than a prefix match, because the mistakes this
/// catches are typos and stale names — `jade_gfx_key_press` for
/// `jade_gfx_key_pressed` — which share a prefix with half the table. The
/// threshold scales with the name's length so a short symbol cannot match
/// something unrelated, and a long one still tolerates a dropped word.
fn closest_symbol<'a>(wanted: &str, declared: impl Iterator<Item = &'a String>) -> Option<String> {
    let budget = (wanted.len() / 3).clamp(1, 8);
    declared
        .map(|s| (edit_distance(wanted, s), s))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, s)| (*d, s.len()))
        .map(|(_, s)| s.clone())
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ── Renamer ───────────────────────────────────────────────────────────────────

/// Walks one module's statements, mangling references to module-globals (its own,
/// via `self_*`; imported, via `aliases` / `from_*`) while leaving locals bare.
struct Renamer {
    /// `None` for `main` (root namespace — never mangle own names).
    self_id: Option<u32>,
    self_values: HashSet<String>,
    self_types: HashSet<String>,
    /// import alias → (module id, value names, type names) of the target module.
    aliases: HashMap<String, (u32, HashSet<String>, HashSet<String>)>,
    /// `from "x" use n` → mangled target name, by namespace.
    from_value: HashMap<String, String>,
    from_type: HashMap<String, String>,
    /// Rust package imports: `use pkg` alias → pkgid, and `from pkg use f` name
    /// → pkgid. References through these rewrite to `__native$<pkgid>$<fn>`.
    native_aliases: HashMap<String, u32>,
    native_from: HashMap<String, u32>,
    /// Import alias → the source package it names and the top-level names the
    /// package keeps to itself.
    hidden: HashMap<String, (String, HashSet<String>)>,
    /// Rust package alias → the classes its package exports, by the name
    /// written after the alias.
    native_types: HashMap<String, HashMap<String, String>>,
    /// pkgid → the names that Rust package exports.
    native_symbols: HashMap<u32, HashSet<String>>,
    /// Undeclared symbols found while renaming, reported after the walk.
    ///
    /// Collected rather than returned because renaming a tree cannot fail — the
    /// walk has no `Result` to thread an error back through, and giving it one
    /// would touch every arm to carry a case that only this check produces.
    errors: Vec<String>,
    /// Lexical local-binding stack; an identifier shadowed here is never mangled.
    locals: Vec<HashSet<String>>,
}

/// The canonical identifier a native function reference is rewritten to. `$` is
/// never produced by the lexer and `<fn>` is `[A-Za-z0-9_]`, so the form parses
/// unambiguously by splitting on `$`. Codegen recognizes the `__native$` prefix.
pub(crate) fn native_ref(pkgid: u32, f_name: &str) -> String {
    format!("__native${pkgid}${f_name}")
}

impl Renamer {
    fn push_scope(&mut self) {
        self.locals.push(HashSet::new());
    }
    fn pop_scope(&mut self) {
        self.locals.pop();
    }
    fn bind_local(&mut self, name: &str) {
        if let Some(top) = self.locals.last_mut() {
            top.insert(name.to_string());
        }
    }
    fn is_shadowed(&self, name: &str) -> bool {
        self.locals.iter().any(|s| s.contains(name))
    }

    /// Resolve a bare value reference (scope-aware).
    fn ref_value(&self, name: &str) -> Option<String> {
        if self.is_shadowed(name) {
            return None;
        }
        if let Some(m) = self.from_value.get(name) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id
            && self.self_values.contains(name)
        {
            return Some(mangle(name, id));
        }
        None
    }

    /// Resolve `alias.field` where `alias` is a Rust package import → the
    /// canonical `__native$<pkgid>$<field>` ref, unless `alias` is shadowed.
    ///
    /// This is also where the name is checked against what the package exports,
    /// as its `.jde` sibling [`ref_value_qual`] asks whether a module exports the
    /// field, so a mistyped function is refused with the closest name.
    fn ref_native_qual(&mut self, alias: &str, field: &str, span: Span) -> Option<String> {
        if self.is_shadowed(alias) {
            return None;
        }
        let id = *self.native_aliases.get(alias)?;
        // A struct the package exports is a class, which `ref_type` names.
        let class = field.split_once("::").map_or(field, |(c, _)| c);
        if self.native_types.get(alias).is_some_and(|t| t.contains_key(class)) {
            return None;
        }
        if let Some(declared) = self.native_symbols.get(&id)
            && !declared.contains(field)
        {
            self.errors.push(
                JadeError::UnknownFfiSymbol {
                    module: alias.to_string(),
                    symbol: field.to_string(),
                    suggestion: closest_symbol(field, declared.iter()),
                    span,
                }
                .to_string(),
            );
        }
        Some(native_ref(id, field))
    }

    /// Resolve a bare `from lib use name` native binding → `__native$<pkgid>$<name>`,
    /// unless shadowed by a local.
    fn ref_native_value(&self, name: &str) -> Option<String> {
        if self.is_shadowed(name) {
            return None;
        }
        self.native_from.get(name).map(|id| native_ref(*id, name))
    }

    /// Resolve `alias.field` to a mangled value, unless `alias` is a shadowing local.
    fn ref_value_qual(&self, alias: &str, field: &str) -> Option<String> {
        if self.is_shadowed(alias) {
            return None;
        }
        let (id, values, _) = self.aliases.get(alias)?;
        if values.contains(field) { Some(mangle(field, *id)) } else { None }
    }

    /// Purpose: This function records an error when `alias.field` names a
    /// top-level name its source package does not export.
    ///
    /// Inputs:
    /// - `alias`: the import's name.
    /// - `field`: the name after the dot, or a class before its `::`.
    /// - `span`: where the reference is, for the message.
    ///
    /// Outputs: none.
    fn check_exported(&mut self, alias: &str, field: &str, span: Span) {
        if self.is_shadowed(alias) {
            return;
        }
        let name = field.split_once("::").map_or(field, |(class, _)| class);
        if let Some((package, gone)) = self.hidden.get(alias)
            && gone.contains(name)
        {
            self.errors.push(
                JadeError::NotExported { package: package.clone(), name: name.to_string(), span }
                    .to_string(),
            );
        }
    }

    /// Resolve a type name (possibly `alias.Type`). Types share no namespace with
    /// values and are not shadowed by locals.
    fn ref_type(&self, tn: &str) -> Option<String> {
        if let Some((alias, field)) = tn.split_once('.') {
            if let Some(class) = self.native_types.get(alias).and_then(|t| t.get(field)) {
                return Some(class.clone());
            }
            let (id, _, types) = self.aliases.get(alias)?;
            return types.contains(field).then(|| mangle(field, *id));
        }
        if let Some(m) = self.from_type.get(tn) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id
            && self.self_types.contains(tn)
        {
            return Some(mangle(tn, id));
        }
        // Fallback: a *bare* imported type whose `alias.` qualifier was stripped by
        // type inference. `let G = tools.ToolGroup {}` reaches codegen as a bare
        // `StructLiteral.type_name = "ToolGroup"` / `JadeType::Struct("ToolGroup")`
        // (see type_infer's `has_imports` StructLiteral branch), so it matches none
        // of the cases above. Resolve it against the unique imported module that
        // exports this type name, mangling to that module's id so it agrees with the
        // module's own (mangled) `StructDef`. If two imported modules export the
        // same struct name it's ambiguous — leave bare rather than guess (the
        // distinct definitions must not collapse onto one another).
        let mut resolved: Option<String> = None;
        for (id, _, types) in self.aliases.values() {
            if types.contains(tn) {
                if resolved.is_some() {
                    return None; // ambiguous across modules — leave bare
                }
                resolved = Some(mangle(tn, *id));
            }
        }
        for types in self.native_types.values() {
            if let Some(class) = types.get(tn) {
                if resolved.as_ref().is_some_and(|r| r != class) {
                    return None;
                }
                resolved = Some(class.clone());
            }
        }
        resolved
    }

    /// Resolve a decorator target (`alias.dec` or bare `dec`) — a value, applied at
    /// the (top-level) definition site so no local shadowing applies.
    fn ref_decorator(&self, name: &str) -> Option<String> {
        if let Some((alias, field)) = name.split_once('.') {
            let (id, values, _) = self.aliases.get(alias)?;
            return values.contains(field).then(|| mangle(field, *id));
        }
        if let Some(m) = self.from_value.get(name) {
            return Some(m.clone());
        }
        if let Some(id) = self.self_id
            && self.self_values.contains(name)
        {
            return Some(mangle(name, id));
        }
        None
    }

    /// Mangle a top-level value *definition* name (no-op for `main`).
    fn def_value(&self, name: &str) -> Option<String> {
        self.self_id.filter(|_| self.self_values.contains(name)).map(|id| mangle(name, id))
    }
    /// Mangle a top-level type *definition* name (no-op for `main`).
    fn def_type(&self, name: &str) -> Option<String> {
        self.self_id.filter(|_| self.self_types.contains(name)).map(|id| mangle(name, id))
    }

    // ── Statements ────────────────────────────────────────────────────────────

    /// `top` marks module-top-level statements, whose definition names are
    /// globals (mangled). Inside bodies, definitions are locals (bound, not mangled).
    fn rename_stmt(&mut self, s: &mut TStmt, top: bool) {
        match s {
            TStmt::Let { name, value, .. } => {
                self.rename_expr(value);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
            }
            TStmt::Assign { name, value, .. } => {
                self.rename_expr(value);
                if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            TStmt::FnDef { name, params, body, ret_ty, decorators, .. }
            | TStmt::AsyncFnDef { name, params, body, ret_ty, decorators, .. } => {
                self.rename_decorators(decorators);
                self.rename_type(ret_ty);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
                self.push_scope();
                for (pname, default) in params.iter_mut() {
                    if let Some(d) = default {
                        self.rename_expr(d);
                    }
                    self.bind_local(pname);
                }
                for st in body.iter_mut() {
                    self.rename_stmt(st, false);
                }
                self.pop_scope();
            }
            TStmt::StructDef { name, fields, parents, members, .. } => {
                // A parent is a type *reference*, so it renames the way an
                // `extend` target does. Left bare when `ref_type` cannot place
                // it, which `resolve_inheritance` then reports by name.
                for parent in parents.iter_mut() {
                    if let Some(m) = self.ref_type(parent) {
                        *parent = m;
                    }
                }
                // A member's type decorator names a class, which renames too.
                for (_, decs) in members.decorators.iter_mut() {
                    for d in decs.iter_mut() {
                        if let Some(m) = self.ref_type(d) {
                            *d = m;
                        }
                    }
                }
                for f in fields.iter_mut() {
                    match f {
                        StructFieldDef::Let { default, .. }
                        | StructFieldDef::Prompt { default, .. } => self.rename_ast_expr(default),
                        StructFieldDef::Required(_) => {}
                    }
                }
                if top && let Some(m) = self.def_type(name) {
                    *name = m;
                }
            }
            TStmt::ExtendBlock { type_name, methods, decorators, .. } => {
                self.rename_decorators(decorators);
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                // Methods are FnDefs; their names are NOT globals (they key as
                // `Type$id__method` off the already-mangled type_name).
                for meth in methods.iter_mut() {
                    if let TStmt::FnDef { params, body, ret_ty, decorators, .. }
                    | TStmt::AsyncFnDef { params, body, ret_ty, decorators, .. } = meth
                    {
                        self.rename_decorators(decorators);
                        self.rename_type(ret_ty);
                        self.push_scope();
                        for (pname, default) in params.iter_mut() {
                            if let Some(d) = default {
                                self.rename_expr(d);
                            }
                            self.bind_local(pname);
                        }
                        for st in body.iter_mut() {
                            self.rename_stmt(st, false);
                        }
                        self.pop_scope();
                    } else {
                        self.rename_stmt(meth, false);
                    }
                }
            }
            TStmt::FieldAssign { object, value, .. } => {
                self.rename_expr(value);
                if let Some(m) = self.ref_value(object) {
                    *object = m;
                }
            }
            TStmt::IndexAssign { name, index, value, .. } => {
                self.rename_expr(index);
                self.rename_expr(value);
                if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            TStmt::PromptDecl { name, body, .. } => {
                self.rename_expr(body);
                if top {
                    if let Some(m) = self.def_value(name) {
                        *name = m;
                    }
                } else {
                    self.bind_local(name);
                }
            }
            TStmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.rename_expr(e);
                }
            }
            TStmt::Yield { value, .. } => self.rename_expr(value),
            TStmt::If { condition, then_body, else_body, .. } => {
                self.rename_expr(condition);
                self.block(then_body);
                if let Some(eb) = else_body {
                    self.block(eb);
                }
            }
            TStmt::While { condition, body, .. } => {
                self.rename_expr(condition);
                self.block(body);
            }
            TStmt::For { var, iterable, body, .. } => {
                self.rename_expr(iterable);
                self.push_scope();
                self.bind_local(var);
                for st in body.iter_mut() {
                    self.rename_stmt(st, false);
                }
                self.pop_scope();
            }
            TStmt::TryCatch { body, arms, .. } => {
                self.block(body);
                for arm in arms.iter_mut() {
                    if let Some(ct) = arm.catch_type.as_mut()
                        && let Some(m) = self.ref_type(ct)
                    {
                        *ct = m;
                    }
                    self.push_scope();
                    self.bind_local(&arm.binding);
                    for st in arm.body.iter_mut() {
                        self.rename_stmt(st, false);
                    }
                    self.pop_scope();
                }
            }
            TStmt::Raise { value, .. } => self.rename_expr(value),
            TStmt::Expr(e) => self.rename_expr(e),
            // Control flow, naming nothing to rename.
            TStmt::Break { .. } | TStmt::Continue { .. } => {}
            // Imports were stripped in process_file (except preserved stdlib
            // from-uses, which carry nothing to rename).
            TStmt::Use { .. } | TStmt::FromUse { .. } => {}
        }
    }

    fn block(&mut self, stmts: &mut [TStmt]) {
        self.push_scope();
        for s in stmts.iter_mut() {
            self.rename_stmt(s, false);
        }
        self.pop_scope();
    }

    fn rename_decorators(&mut self, decorators: &mut [TDecorator]) {
        for (dname, dargs) in decorators.iter_mut() {
            if let Some(m) = self.ref_decorator(dname) {
                *dname = m;
            }
            for (_, e) in dargs.iter_mut() {
                self.rename_expr(e);
            }
        }
    }

    // ── TIR expressions ───────────────────────────────────────────────────────

    fn rename_expr(&mut self, e: &mut TExpr) {
        self.rename_type(&mut e.ty);

        // `alias.foo` → `foo$<id>` (.jde) or `__native$<pkgid>$foo` (native),
        // before walking children so the bare alias isn't independently rewritten.
        // Cloned rather than borrowed: the native arm reports an undeclared
        // symbol, so it needs `&mut self` while `e.kind` is still in hand.
        let qualified = match &e.kind {
            TExprKind::FieldAccess { object, field } => match &object.kind {
                TExprKind::Identifier(alias) => Some((alias.clone(), field.clone())),
                _ => None,
            },
            _ => None,
        };
        let collapse = qualified.and_then(|(alias, field)| {
            self.check_exported(&alias, &field, e.span);
            self.ref_native_qual(&alias, &field, e.span)
                .or_else(|| self.ref_value_qual(&alias, &field))
                // `shapes.Animal` as a value is a class view, `shapes.Animal(x)`,
                // and `shapes.Animal::speak` a parent call: both name a class.
                .or_else(|| self.ref_type(&format!("{alias}.{field}")))
                .or_else(|| {
                    let (class, member) = field.split_once("::")?;
                    self.ref_type(&format!("{alias}.{class}")).map(|c| format!("{c}::{member}"))
                })
        });
        if let Some(m) = collapse {
            e.kind = TExprKind::Identifier(m);
            return;
        }

        match &mut e.kind {
            TExprKind::Is { value, class } => {
                self.rename_expr(value);
                if let Some(m) = self.ref_type(class) {
                    *class = m;
                }
            }
            TExprKind::Identifier(n) => {
                if let Some(m) = self.ref_native_value(n) {
                    *n = m;
                } else if let Some(m) = self.ref_value(n) {
                    *n = m;
                } else if let Some((class, member)) = n.split_once("::")
                    && let Some(c) = self.ref_type(class)
                {
                    // `Animal::speak`: the class part is a type reference.
                    *n = format!("{c}::{member}");
                } else if let Some(m) = self.ref_type(n) {
                    // `Animal(x)`, a class view of a value.
                    *n = m;
                }
            }
            TExprKind::Call { callee, args, kwargs } => {
                self.rename_expr(callee);
                for a in args.iter_mut() {
                    self.rename_expr(a);
                }
                for (_, v) in kwargs.iter_mut() {
                    self.rename_expr(v);
                }
            }
            TExprKind::BinOp { left, right, .. } => {
                self.rename_expr(left);
                self.rename_expr(right);
            }
            TExprKind::UnaryOp { operand, .. } => self.rename_expr(operand),
            TExprKind::StructLiteral { type_name, base, fields, .. } => {
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                if let Some(b) = base.as_mut() {
                    self.rename_expr(b);
                }
                for (_, v, _) in fields.iter_mut() {
                    self.rename_expr(v);
                }
            }
            TExprKind::FieldAccess { object, .. } => self.rename_expr(object),
            TExprKind::Index { object, index } => {
                self.rename_expr(object);
                self.rename_expr(index);
            }
            TExprKind::Array { elements } => {
                for el in elements.iter_mut() {
                    self.rename_expr(el);
                }
            }
            TExprKind::FStr { parts } => {
                for p in parts.iter_mut() {
                    if let TFStrPart::Expr(inner) = p {
                        self.rename_expr(inner);
                    }
                }
            }
            TExprKind::PromptDeref { expr, output_type, grammar_expr } => {
                self.rename_expr(expr);
                if let Some(ot) = output_type.as_mut()
                    && let Some(m) = self.ref_type(ot)
                {
                    *ot = m;
                }
                if let Some(g) = grammar_expr {
                    self.rename_expr(g);
                }
            }
            TExprKind::Dict { entries } => {
                for (k, v) in entries.iter_mut() {
                    self.rename_expr(k);
                    self.rename_expr(v);
                }
            }
            TExprKind::Await { expr } => self.rename_expr(expr),
            TExprKind::PromptLiteral { body } => self.rename_expr(body),
            // The piece and the hoisted arguments are names the pipe binds, so
            // they shadow any top-level name inside its stages.
            TExprKind::Pipe { source, hoisted, piece, body } => {
                self.rename_expr(source);
                for (_, v) in hoisted.iter_mut() {
                    self.rename_expr(v);
                }
                self.push_scope();
                self.bind_local(piece);
                for (name, _) in hoisted.iter() {
                    self.bind_local(name);
                }
                self.rename_expr(body);
                self.pop_scope();
            }
            TExprKind::Integer(_)
            | TExprKind::Float(_)
            | TExprKind::Bool(_)
            | TExprKind::Str(_) => {}
        }
    }

    fn rename_type(&mut self, t: &mut JadeType) {
        match t {
            JadeType::Struct(n) => {
                if let Some(m) = self.ref_type(n) {
                    *n = m;
                }
            }
            JadeType::Array(inner) | JadeType::Future(inner) => self.rename_type(inner),
            JadeType::Fn { params, ret } | JadeType::AsyncFn { params, ret } => {
                for p in params.iter_mut() {
                    self.rename_type(p);
                }
                self.rename_type(ret);
            }
            _ => {}
        }
    }

    // ── AST expressions (struct-field defaults) ───────────────────────────────

    fn rename_ast_expr(&mut self, e: &mut Expr) {
        // `alias.foo` collapse (native + .jde), same as the TIR path.
        // Cloned for the same reason as the TIR path: the native arm needs
        // `&mut self` to report an undeclared symbol.
        let qualified = match &*e {
            Expr::FieldAccess { object, field, span } => match object.as_ref() {
                Expr::Identifier { name: alias, .. } => Some((alias.clone(), field.clone(), *span)),
                _ => None,
            },
            _ => None,
        };
        let repl = qualified.and_then(|(alias, field, span)| {
            self.check_exported(&alias, &field, span);
            self.ref_native_qual(&alias, &field, span)
                .or_else(|| self.ref_value_qual(&alias, &field))
                .map(|m| (m, span))
        });
        if let Some((m, span)) = repl {
            *e = Expr::Identifier { name: m, span };
            return;
        }

        match e {
            Expr::Is { value, class, .. } => {
                self.rename_ast_expr(value);
                if let Some(m) = self.ref_type(class) {
                    *class = m;
                }
            }
            Expr::Identifier { name, .. } => {
                if let Some(m) = self.ref_native_value(name) {
                    *name = m;
                } else if let Some(m) = self.ref_value(name) {
                    *name = m;
                }
            }
            Expr::Call { callee, args, kwargs, .. } => {
                self.rename_ast_expr(callee);
                for a in args.iter_mut() {
                    self.rename_ast_expr(a);
                }
                for (_, v) in kwargs.iter_mut() {
                    self.rename_ast_expr(v);
                }
            }
            Expr::BinOp { left, right, .. } => {
                self.rename_ast_expr(left);
                self.rename_ast_expr(right);
            }
            Expr::UnaryOp { operand, .. } => self.rename_ast_expr(operand),
            Expr::StructLiteral { type_name, fields, .. } => {
                if let Some(m) = self.ref_type(type_name) {
                    *type_name = m;
                }
                for (_, v) in fields.iter_mut() {
                    self.rename_ast_expr(v);
                }
            }
            Expr::FieldAccess { object, .. } => self.rename_ast_expr(object),
            Expr::Index { object, index, .. } => {
                self.rename_ast_expr(object);
                self.rename_ast_expr(index);
            }
            Expr::Array { elements, .. } => {
                for el in elements.iter_mut() {
                    self.rename_ast_expr(el);
                }
            }
            Expr::FStr { parts, .. } => {
                for p in parts.iter_mut() {
                    if let FStrPart::Expr(inner) = p {
                        self.rename_ast_expr(inner);
                    }
                }
            }
            Expr::PromptLiteral { body, .. } => self.rename_ast_expr(body),
            Expr::PromptDeref { expr, .. } => self.rename_ast_expr(expr),
            Expr::Dict { entries, .. } => {
                for (k, v) in entries.iter_mut() {
                    self.rename_ast_expr(k);
                    self.rename_ast_expr(v);
                }
            }
            Expr::Await { expr, .. } => self.rename_ast_expr(expr),
            // A pipe stage is an ordinary expression — an imported function is a
            // legitimate stage (`x |> mathlib.double`), so it needs namespacing
            // exactly as the piped value does.
            Expr::Pipe { value, stage, .. } => {
                self.rename_ast_expr(value);
                self.rename_ast_expr(stage);
            }
            Expr::Integer { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::Str { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Renamer` for an importing module (`self_id`) that imports one or
    /// more modules under aliases, each exporting the given type names.
    fn renamer(self_id: Option<u32>, imports: &[(&str, u32, &[&str])]) -> Renamer {
        let mut aliases = HashMap::new();
        for (alias, id, types) in imports {
            let tset: HashSet<String> = types.iter().map(|s| s.to_string()).collect();
            aliases.insert(alias.to_string(), (*id, HashSet::new(), tset));
        }
        Renamer {
            self_id,
            self_values: HashSet::new(),
            self_types: HashSet::new(),
            aliases,
            from_value: HashMap::new(),
            from_type: HashMap::new(),
            native_aliases: HashMap::new(),
            native_from: HashMap::new(),
            hidden: HashMap::new(),
            native_types: HashMap::new(),
            native_symbols: HashMap::new(),
            errors: Vec::new(),
            locals: Vec::new(),
        }
    }

    // A bare imported type (the `tools.` qualifier already stripped by type
    // inference) resolves to the importing-from module's mangled name.
    #[test]
    fn bare_imported_type_resolves_to_module_id() {
        let r = renamer(None, &[("tools", 3, &["ToolGroup"])]);
        assert_eq!(r.ref_type("ToolGroup").as_deref(), Some("ToolGroup$3"));
    }

    // Qualified `alias.Type` still mangles through the existing path.
    #[test]
    fn qualified_imported_type_still_resolves() {
        let r = renamer(None, &[("tools", 3, &["ToolGroup"])]);
        assert_eq!(r.ref_type("tools.ToolGroup").as_deref(), Some("ToolGroup$3"));
    }

    // Two imported modules exporting the same struct name is ambiguous — leave it
    // bare so the distinct definitions never collapse onto one id.
    #[test]
    fn same_name_across_modules_is_ambiguous() {
        let r = renamer(None, &[("a", 1, &["Foo"]), ("b", 2, &["Foo"])]);
        assert_eq!(r.ref_type("Foo"), None);
    }

    // A type defined in `main` (no alias exports it) stays bare.
    #[test]
    fn unknown_or_main_type_stays_bare() {
        let r = renamer(None, &[("tools", 3, &["ToolGroup"])]);
        assert_eq!(r.ref_type("MainOnly"), None);
    }

    // A native `alias.fn` rewrites to the canonical `__native$<pkgid>$fn` ref.
    #[test]
    fn native_alias_qual_rewrites() {
        let mut r = renamer(None, &[]);
        r.native_aliases.insert("m".to_string(), 0);
        assert_eq!(
            r.ref_native_qual("m", "add", Span { line: 1, col: 1 }).as_deref(),
            Some("__native$0$add")
        );
        // a non-native alias is left to the .jde path (None here)
        assert_eq!(r.ref_native_qual("other", "add", Span { line: 1, col: 1 }), None);
    }

    // A `from lib use add` native binding rewrites a bare `add` ref.
    #[test]
    fn native_from_rewrites_bare_ref() {
        let mut r = renamer(None, &[]);
        r.native_from.insert("add".to_string(), 2);
        assert_eq!(r.ref_native_value("add").as_deref(), Some("__native$2$add"));
    }

    // A local shadowing a native alias/binding leaves the reference bare.
    #[test]
    fn native_ref_respects_shadowing() {
        let mut r = renamer(None, &[]);
        r.native_aliases.insert("m".to_string(), 0);
        r.native_from.insert("add".to_string(), 0);
        r.push_scope();
        r.bind_local("m");
        r.bind_local("add");
        assert_eq!(r.ref_native_qual("m", "add", Span { line: 1, col: 1 }), None);
        assert_eq!(r.ref_native_value("add"), None);
    }

    // A non-main module's own type still mangles via `self_types`, and the alias
    // fallback doesn't shadow that.
    #[test]
    fn self_type_takes_precedence() {
        let mut r = renamer(Some(7), &[("tools", 3, &["ToolGroup"])]);
        r.self_types.insert("Local".to_string());
        assert_eq!(r.ref_type("Local").as_deref(), Some("Local$7"));
        // and the imported bare name still resolves to the import, not self
        assert_eq!(r.ref_type("ToolGroup").as_deref(), Some("ToolGroup$3"));
    }
}

#[cfg(test)]
mod rust_symbol_tests {
    use super::*;

    const SPAN: Span = Span { line: 4, col: 7 };

    /// A renamer holding one Rust package under `gfx` (pkgid 0) that exports
    /// exactly `names`, and the struct `Point`.
    fn native_renamer(names: &[&str]) -> Renamer {
        let mut native_aliases = HashMap::new();
        native_aliases.insert("gfx".to_string(), 0u32);
        let mut native_symbols = HashMap::new();
        native_symbols.insert(0u32, names.iter().map(|s| s.to_string()).collect::<HashSet<_>>());
        let mut native_types = HashMap::new();
        native_types.insert(
            "gfx".to_string(),
            [("Point".to_string(), "Point$rgfx".to_string())].into_iter().collect(),
        );
        Renamer {
            self_id: None,
            self_values: HashSet::new(),
            self_types: HashSet::new(),
            aliases: HashMap::new(),
            from_value: HashMap::new(),
            from_type: HashMap::new(),
            native_aliases,
            native_from: HashMap::new(),
            hidden: HashMap::new(),
            native_types,
            native_symbols,
            errors: Vec::new(),
            locals: Vec::new(),
        }
    }

    #[test]
    fn an_exported_function_rewrites_and_reports_nothing() {
        let mut r = native_renamer(&["init", "key_pressed"]);
        assert_eq!(r.ref_native_qual("gfx", "init", SPAN).as_deref(), Some("__native$0$init"));
        assert!(r.errors.is_empty());
    }

    /// A name the package does not export is refused when the program is
    /// checked, not the first time the line runs.
    #[test]
    fn a_name_the_package_does_not_export_is_reported() {
        let mut r = native_renamer(&["init", "key_pressed"]);
        r.ref_native_qual("gfx", "nope", SPAN);
        assert_eq!(r.errors.len(), 1);
        let msg = &r.errors[0];
        assert!(msg.contains("[4:7]"), "no span in: {msg}");
        assert!(msg.contains("nope"), "no name in: {msg}");
        assert!(msg.contains("gfx"), "no package name in: {msg}");
    }

    #[test]
    fn a_near_miss_suggests_the_real_name() {
        let mut r = native_renamer(&["init", "key_pressed"]);
        r.ref_native_qual("gfx", "key_press", SPAN);
        assert!(r.errors[0].contains("did you mean 'key_pressed'?"), "{}", r.errors[0]);
    }

    /// A name nothing resembles names the fix in the package instead of a
    /// suggestion — proposing an unrelated name would be worse than silence.
    #[test]
    fn a_wild_miss_names_the_attribute_instead_of_guessing() {
        let mut r = native_renamer(&["init"]);
        r.ref_native_qual("gfx", "totally_unrelated_thing", SPAN);
        assert!(!r.errors[0].contains("did you mean"), "guessed: {}", r.errors[0]);
        assert!(r.errors[0].contains("#[jade::export]"), "{}", r.errors[0]);
    }

    /// A local of the same name is not the import, so nothing about it is
    /// checked — the same rule the rewrite itself has always followed.
    #[test]
    fn a_shadowed_alias_is_left_alone() {
        let mut r = native_renamer(&["init"]);
        r.push_scope();
        r.bind_local("gfx");
        assert_eq!(r.ref_native_qual("gfx", "anything_at_all", SPAN), None);
        assert!(r.errors.is_empty());
    }

    /// `gfx.Point` names the struct's class, which `ref_type` resolves, so the
    /// function path neither claims it nor reports it.
    #[test]
    fn a_struct_name_is_left_to_the_type_path() {
        let mut r = native_renamer(&["init", "Point"]);
        assert_eq!(r.ref_native_qual("gfx", "Point", SPAN), None);
        assert!(r.errors.is_empty());
        assert_eq!(r.ref_type("gfx.Point").as_deref(), Some("Point$rgfx"));
    }

    // ── Rust structs as classes ───────────────────────────────────────────────

    fn point() -> crate::rustpkg::Struct {
        crate::rustpkg::Struct {
            krate: "geo".into(),
            name: "Point".into(),
            size: 40,
            align: 8,
            fields: vec![
                crate::rustpkg::meta::Field { name: "x".into(), ty: "float".into(), offset: 0 },
                crate::rustpkg::meta::Field {
                    name: "near".into(),
                    ty: "[geo::Point]?".into(),
                    offset: 8,
                },
            ],
        }
    }

    #[test]
    fn a_rust_struct_becomes_a_class_with_public_required_fields() {
        let classes: HashMap<String, String> = [
            ("geo::Point".to_string(), "Point$rgeo".to_string()),
            ("Point$rgeo".to_string(), "Point$rgeo".to_string()),
        ]
        .into_iter()
        .collect();
        let TStmt::StructDef { name, fields, members, .. } = rust_class(&point(), &classes) else {
            panic!("a struct becomes a class");
        };
        assert_eq!(name, "Point$rgeo");
        assert!(fields.iter().all(|f| matches!(f, StructFieldDef::Required(_))));
        assert!(members.is_public("x") && members.is_public("near"));
        let near = members.decorators.iter().find(|(f, _)| f == "near").map(|(_, d)| d.clone());
        assert_eq!(near, Some(vec!["public".to_string(), "[Point$rgeo]?".to_string()]));
    }

    #[test]
    fn struct_names_inside_a_type_are_written_as_classes() {
        let classes: HashMap<String, String> =
            [("geo::Point".to_string(), "Point$rgeo".to_string())].into_iter().collect();
        assert_eq!(class_type_text("[geo::Point]?", &classes), "[Point$rgeo]?");
        assert_eq!(class_type_text("geo::Pointy", &classes), "geo::Pointy");
        assert_eq!(class_type_text("str", &classes), "str");
    }

    // ── closest_symbol ────────────────────────────────────────────────────────

    #[test]
    fn a_one_character_slip_is_matched() {
        let declared = ["jade_gfx_key_pressed".to_string()];
        assert_eq!(
            closest_symbol("jade_gfx_key_presed", declared.iter()).as_deref(),
            Some("jade_gfx_key_pressed")
        );
    }

    /// The budget scales with length, so a short name cannot drag in an
    /// unrelated one that merely happens to be a few edits away.
    #[test]
    fn a_short_name_does_not_match_something_unrelated() {
        let declared = ["quit".to_string()];
        assert_eq!(closest_symbol("draw", declared.iter()), None);
    }

    #[test]
    fn the_nearest_of_several_wins() {
        let declared: Vec<String> = ["gfx_draw_rect", "gfx_draw_line", "gfx_draw_text"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            closest_symbol("gfx_draw_rec", declared.iter()).as_deref(),
            Some("gfx_draw_rect")
        );
    }

    #[test]
    fn nothing_declared_suggests_nothing() {
        assert_eq!(closest_symbol("anything", std::iter::empty()), None);
    }

    #[test]
    fn edit_distance_is_symmetric_and_zero_on_equality() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
