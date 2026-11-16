//! The cached, precompiled library: each std library instance compiled once
//! per machine, its object file kept, and linked into every program that
//! needs it by LLD. Part 4 of `design/v2-simplify.md`.
//!
//! *What is cached.* `plan.rs` decides: an instance whose types are all
//! built in, and which calls only instances like it, gets an object of its
//! own; a small one is copied into each module that calls it; the rest, such
//! as any instance that touches one of the program's classes, compile with
//! the program as before.
//!
//! *Where.* `~/.jade/lib/<version>/<triple>/<key>.o` ([`default_root`]). The
//! key is the SHA-256 of the compiler's version, the target triple, CPU,
//! features and code-generation options, the optimization pipeline, and the
//! instance's module as IR text before optimization. Since the key covers the
//! IR, a development build whose library or codegen changed never reads an
//! object built from older code, even at the same version. Nothing prunes the
//! directories of older versions; `rm -rf ~/.jade/lib` clears the cache.
//!
//! *Each build* makes every cached instance's module, which is quick, prints
//! its IR, and hashes it. A hit links the object as it is. A miss optimizes
//! the module, writes the object under a name of its own in the same
//! directory and renames it into place, so two builds at once never read a
//! half-written file. The program's module is built last, with what the
//! library modules declared of its shared state (`shared.rs`), hit or miss,
//! so it defines all of it.
//!
//! *The cache never fails a build.* If the directory cannot be made or
//! written, the object goes to the build's scratch place beside the output
//! and is deleted after linking. If anything else in this path fails before
//! linking, the program is built in one module, as `--emit ir` shows it. If
//! linking fails and some object came from the cache, the driver in `build/`
//! tries once more in one module, since a damaged object is the one thing a
//! cache adds; when that works, it says so and names the directory to remove.
//!
//! *Threads.* A cold cache compiles one module per instance. The modules are
//! independent, so they are built on up to [`THREADS`] threads, each with its
//! own LLVM context and target machine.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::targets::{FileType, TargetMachine};
use sha2::{Digest, Sha256};

use super::shared::{Role, Shared};
use super::{Cx, Objects, func, mir};
use plan::{Kind, LibPlan};

pub(super) mod plan;

/// The most threads a build compiles library modules on.
pub const THREADS: usize = 8;

/// Purpose: This function returns where `jade build` keeps compiled library
/// instances: `~/.jade/lib`.
pub fn default_root() -> PathBuf {
    crate::rustpkg::jade_home().join("lib")
}

/// Purpose: This struct holds what one build did with the library cache, for
/// tests and for measuring.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Each library object linked.
    pub objects: Vec<LibObject>,
    /// The directory objects were read from and written to, when it could be
    /// made.
    pub dir: Option<PathBuf>,
    /// Instances the plan cached whose modules turned out to depend on the
    /// program, which compiled with it instead. Always empty unless this
    /// backend has a bug.
    pub demoted: Vec<String>,
    /// Why the build compiled in one module instead, when something before
    /// linking failed.
    pub fallback: Option<String>,
    /// What the linker said when the cached objects did not link and the
    /// build without them did.
    pub relinked: Option<String>,
}

impl Report {
    /// Purpose: This function counts the objects taken from the cache.
    pub fn hits(&self) -> usize {
        self.objects.iter().filter(|o| o.hit).count()
    }
}

/// Purpose: This struct describes one library object a build linked.
#[derive(Debug, Clone)]
pub struct LibObject {
    /// The instance's label, such as `join$4294966271(str, str)`.
    pub label: String,
    /// The file linked.
    pub path: PathBuf,
    /// Whether it came from the cache.
    pub hit: bool,
    /// Whether it was written beside the output, since the cache could not
    /// take it, and deleted after linking.
    pub scratch: bool,
}

/// Purpose: This struct holds one library module's object and what the
/// program's module must define for it.
struct Built {
    id: usize,
    obj: LibObject,
    /// Whether the module asked for something only the program knows.
    dependent: bool,
    refs: BTreeMap<String, Shared>,
}

/// Purpose: This function compiles a program into object files, with its
/// library instances taken from, or added to, the cache.
///
/// Inputs:
/// - `prog`: the program.
/// - `out`: where the binary will go; objects the cache cannot take are
///   written beside it.
/// - `root`: the cache's root, `~/.jade/lib` for `jade build`; tests pass a
///   directory of their own.
///
/// Outputs: the objects, the program's first, with what the build did with
/// the cache; or the error compiling in one module gives.
pub fn objects(prog: &mir::Program, out: &Path, root: &Path) -> Result<Objects, String> {
    let plan = LibPlan::new(prog);
    if plan.objects().is_empty() {
        return super::whole(prog, out);
    }
    match split(prog, out, root, plan) {
        Ok((report, files)) => Ok(Objects { files, report }),
        Err(e) => {
            let mut whole = super::whole(prog, out)?;
            whole.report.fallback = Some(e);
            Ok(whole)
        }
    }
}

impl Report {
    /// Purpose: This function says on standard error what went wrong with
    /// the cache in a build that succeeded anyway: each case is a damaged
    /// cache or a bug in this backend, which someone should hear about.
    ///
    /// Inputs:
    /// - `root`: the cache's root, named when no directory was made.
    ///
    /// Outputs: none.
    pub fn warn(&self, root: &Path) {
        let dir = self.dir.as_deref().unwrap_or(root);
        if self.relinked.is_some() {
            eprintln!(
                "jade: warning: linking with the cached std library failed, and linking without \
                 it worked, so a cached object may be damaged; remove {} to rebuild it",
                dir.display()
            );
        }
        if let Some(e) = &self.fallback {
            eprintln!(
                "jade: warning: internal compiler error: the std library cache was skipped for \
                 this build: {e}"
            );
        }
        if !self.demoted.is_empty() {
            eprintln!(
                "jade: warning: internal compiler error: {} compiled with the program, since \
                 their cached code depended on it",
                self.demoted.join(", ")
            );
        }
    }
}

/// Purpose: This function builds every library object and the program's
/// object, short of linking.
///
/// Inputs:
/// - `prog`, `out`, `root`: as for [`objects`].
/// - `plan`: how each function is compiled.
///
/// Outputs: the report and the objects to link, the program's first; or why
/// this path failed, after which the build compiles in one module.
fn split(
    prog: &mir::Program,
    out: &Path,
    root: &Path,
    mut plan: LibPlan,
) -> Result<(Report, Vec<PathBuf>), String> {
    let machine = super::target_machine()?;
    let triple = machine.get_triple().as_str().to_string_lossy().into_owned();
    let dir = cache_dir(root, &triple);
    let mut built = build_objects(prog, &plan, dir.as_deref(), out, &triple)?;

    // A module that asked for the program's classes or its model is compiled
    // with the program after all, and so is every cached instance calling it.
    let dependent: Vec<usize> = built.iter().filter(|b| b.dependent).map(|b| b.id).collect();
    let mut demoted = Vec::new();
    if !dependent.is_empty() {
        plan.demote(prog, &dependent);
        built.retain(|b| {
            let keep = plan.kind(b.id) == Kind::Object;
            if !keep {
                demoted.push(b.obj.label.clone());
                if b.obj.scratch {
                    std::fs::remove_file(&b.obj.path).ok();
                }
            }
            keep
        });
    }

    let mut exports = BTreeMap::new();
    for b in &built {
        exports.extend(b.refs.iter().map(|(k, v)| (k.clone(), *v)));
    }
    let obj = out.with_extension("o");
    let ctx = Context::create();
    let program = super::build_program_module(&ctx, prog, &plan, Role::Program { exports })
        .and_then(|module| super::write_object(&module, &machine, &obj));
    if let Err(e) = program {
        remove_scratch_objects(built.iter().map(|b| &b.obj));
        return Err(e);
    }
    let mut objs = vec![obj];
    objs.extend(built.iter().map(|b| b.obj.path.clone()));
    let report = Report {
        objects: built.into_iter().map(|b| b.obj).collect(),
        dir,
        demoted,
        fallback: None,
        relinked: None,
    };
    Ok((report, objs))
}

/// Purpose: This function deletes the library objects written beside the
/// output, leaving the cache's own.
pub(super) fn remove_scratch_objects<'a>(objects: impl Iterator<Item = &'a LibObject>) {
    for o in objects.filter(|o| o.scratch) {
        std::fs::remove_file(&o.path).ok();
    }
}

/// Purpose: This function returns the directory this compiler's objects for
/// this target live in, made if it is not there.
///
/// Inputs:
/// - `root`: the cache's root.
/// - `triple`: the target triple.
///
/// Outputs: the directory, or `None` when it cannot be made, after which
/// every object goes to the build's scratch place.
fn cache_dir(root: &Path, triple: &str) -> Option<PathBuf> {
    let safe: String = triple
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect();
    let dir = root.join(env!("CARGO_PKG_VERSION")).join(safe);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Purpose: This function builds every library object the plan names, on
/// threads when there are several.
///
/// Inputs:
/// - `prog`, `plan`: the program and how each function is compiled.
/// - `dir`: the cache directory, if there is one.
/// - `out`: the output, beside which scratch objects go.
/// - `triple`: the target triple, for the key.
///
/// Outputs: each object, in the plan's order, or the first error.
fn build_objects(
    prog: &mir::Program,
    plan: &LibPlan,
    dir: Option<&Path>,
    out: &Path,
    triple: &str,
) -> Result<Vec<Built>, String> {
    let ids = plan.objects();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, THREADS)
        .min(ids.len().div_ceil(2).max(1));
    let run = |chunk: &[usize]| -> Result<Vec<Built>, String> {
        let machine = super::target_machine()?;
        let mut done = Vec::with_capacity(chunk.len());
        for id in chunk {
            match build_object(prog, plan, *id, dir, out, triple, &machine) {
                Ok(b) => done.push(b),
                Err(e) => {
                    remove_scratch_objects(done.iter().map(|b: &Built| &b.obj));
                    return Err(e);
                }
            }
        }
        Ok(done)
    };
    if threads <= 1 {
        return run(&ids);
    }
    // Round-robin, so the large instances, which sort together by name,
    // spread across the threads.
    let chunks: Vec<Vec<usize>> =
        (0..threads).map(|t| ids.iter().skip(t).step_by(threads).copied().collect()).collect();
    let results: Vec<Result<Vec<Built>, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks.iter().map(|c| s.spawn(|| run(c))).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err("a library build thread panicked".into())))
            .collect()
    });
    let mut all = Vec::with_capacity(ids.len());
    let mut failed = None;
    for r in results {
        match r {
            Ok(b) => all.extend(b),
            Err(e) => failed = failed.or(Some(e)),
        }
    }
    if let Some(e) = failed {
        remove_scratch_objects(all.iter().map(|b| &b.obj));
        return Err(e);
    }
    all.sort_by_key(|b| b.id);
    Ok(all)
}

/// Purpose: This function builds one library instance's module and returns
/// its object: the cached one when the key matches, else a new one.
///
/// Inputs:
/// - `prog`, `plan`: the program and how each function is compiled.
/// - `id`: the instance.
/// - `dir`, `out`, `triple`: as for [`build_objects`].
/// - `machine`: this thread's target machine.
///
/// Outputs: the object and what the program must define for it, or an
/// error naming the instance.
fn build_object(
    prog: &mir::Program,
    plan: &LibPlan,
    id: usize,
    dir: Option<&Path>,
    out: &Path,
    triple: &str,
    machine: &TargetMachine,
) -> Result<Built, String> {
    let ctx = Context::create();
    let (cx, dependent) = library_module(&ctx, prog, plan, id)?;
    let f = &prog.funcs[id];
    super::set_target(&cx.module, machine);
    let ir = cx.module.print_to_string().to_string();
    let name = format!("{}.o", object_key(&ir, triple));
    let refs = cx.take_refs();
    let made = |path: PathBuf, hit: bool, scratch: bool| Built {
        id,
        obj: LibObject { label: f.name.clone(), path, hit, scratch },
        dependent,
        refs: refs.clone(),
    };
    if let Some(d) = dir
        && is_object(&d.join(&name))
    {
        return Ok(made(d.join(&name), true, false));
    }
    super::optimize(&cx.module, machine)?;
    let buf = machine
        .write_to_memory_buffer(&cx.module, FileType::Object)
        .map_err(|e| format!("{}: {e}", f.name))?;
    if let Some(d) = dir
        && let Ok(path) = write_atomic(d, &name, buf.as_slice())
    {
        return Ok(made(path, false, false));
    }
    let path = out.with_extension(format!("lib{id}.o"));
    std::fs::write(&path, buf.as_slice()).map_err(|e| {
        format!(
            "cannot write {}: {e}; check that the output's directory is writable",
            path.display()
        )
    })?;
    Ok(made(path, false, true))
}

/// Purpose: This function builds one library instance's module: the
/// instance itself, external and hidden under its symbol; a private copy of
/// each small instance it calls; and a declaration of each cached one.
///
/// Inputs:
/// - `ctx`: the module's own LLVM context, so its IR text names nothing
///   another module made first.
/// - `prog`, `plan`: the program and how each function is compiled.
/// - `id`: the instance.
///
/// Outputs: the module's context, and whether it asked for something only
/// the program knows; or an error naming the instance.
pub(super) fn library_module<'ctx>(
    ctx: &'ctx Context,
    prog: &mir::Program,
    plan: &LibPlan,
    id: usize,
) -> Result<(Cx<'ctx>, bool), String> {
    let f = &prog.funcs[id];
    let missing = |name: &str| format!("internal compiler error: {name} has no library symbol");
    let symbol = plan.symbol(id).ok_or_else(|| missing(&f.name))?;
    let cx = Cx::library(ctx, "jade_lib");
    let mut funcs: Vec<Option<inkwell::values::FunctionValue>> = vec![None; prog.funcs.len()];
    let me = cx.declare_lib(symbol, cx.fn_type(f));
    funcs[id] = Some(me);
    // Each copied callee brings the functions it calls, copied or declared.
    let mut copies = Vec::new();
    let mut todo = plan::callees(f);
    while let Some(c) = todo.pop() {
        if funcs[c].is_some() {
            continue;
        }
        let callee = &prog.funcs[c];
        let sym = plan.symbol(c).ok_or_else(|| missing(&callee.name))?;
        let ty = cx.fn_type(callee);
        funcs[c] = Some(match plan.kind(c) {
            Kind::Copied => {
                copies.push(c);
                todo.extend(plan::callees(callee));
                cx.module.add_function(sym, ty, Some(Linkage::Internal))
            }
            _ => cx.declare_lib(sym, ty),
        });
    }
    let env = func::Env { prog, funcs: &funcs, globals: &[] };
    func::lower(&cx, &env, f, me);
    for c in copies {
        if let Some(fv) = funcs[c] {
            func::lower(&cx, &env, &prog.funcs[c], fv);
        }
    }
    cx.module.verify().map_err(|e| format!("internal compiler error in {}: {e}", f.name))?;
    let dependent = cx.dependent.get();
    Ok((cx, dependent))
}

/// Purpose: This function computes an object's key.
///
/// Inputs:
/// - `ir`: the module's IR text, before optimization.
/// - `triple`: the target triple.
///
/// Outputs: 64 hex digits of the SHA-256 of the compiler's version, the
/// target, the code-generation options, the pipeline and the IR, each
/// prefixed by its length so no two inputs run together.
pub fn object_key(ir: &str, triple: &str) -> String {
    let mut h = Sha256::new();
    for part in [
        env!("CARGO_PKG_VERSION"),
        triple,
        super::CPU,
        super::FEATURES,
        super::CODEGEN,
        super::PIPELINE,
        ir,
    ] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Purpose: This function says whether a file is an object file this
/// backend wrote: Mach-O's 64-bit magic number, or ELF's.
fn is_object(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic));
    read.is_ok() && (magic == [0xcf, 0xfa, 0xed, 0xfe] || magic == *b"\x7fELF")
}

/// Purpose: This function writes a file into a directory so that no reader
/// ever sees part of it: to a name no other writer uses, then renamed.
///
/// Inputs:
/// - `dir`: the directory.
/// - `name`: the file's name.
/// - `bytes`: its contents.
///
/// Outputs: the file's path, or the error; a failed write leaves nothing
/// behind.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let path = dir.join(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    written.map(|()| path)
}

#[cfg(test)]
mod tests;
