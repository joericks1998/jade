//! `jade pkg add` / `remove` / `install` / `update` / `list`.
//!
//! The manifest is the source of truth and `jade.lock` is derived from it.
//! There is no registry to query, so "update" means *reconcile the lock with
//! the manifest*, not *discover a newer version* — bumping a version is an edit
//! to `jade.toml` (by hand, or via `jade pkg add <name>` again).

use std::path::PathBuf;

use crate::pkg::{self, fetch::HttpFetcher, lock, manifest};
use crate::project::{self, Abi, ProjectManifest};

/// Resolve the project root, or exit — every command here is project-scoped.
fn root_or_exit() -> PathBuf {
    match project::find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: not inside a Jade project (no jade.toml with a [project] section)");
            eprintln!("       run `jade init` to create one");
            std::process::exit(1);
        }
    }
}

fn load_or_exit(root: &std::path::Path) -> ProjectManifest {
    match project::load_project(root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    std::process::exit(1);
}

/// Fail out of `jade pkg add`, taking the manifest edit with it.
///
/// `add` has to write the entry before it can validate it — binding a C library
/// reads the dependency back out of `jade.toml`, and resolving needs it there to
/// resolve. So a failure lands *after* the write. Leaving the entry behind was
/// worse than it sounds: every other `pkg` command re-validates the whole
/// manifest, so one `add` that failed on a missing file made `install`, `list`
/// and even a later successful `add` fail on an orphan the user never managed to
/// add, with nothing naming it as the cause.
///
/// Only a newly-created entry is removed. `add` replaces an existing dependency
/// outright, and rolling that back would delete a working entry to clean up
/// after a failed attempt to change it — so an existing one is left as it is,
/// and the message says the file was touched.
fn fail_new_dependency(
    root: &std::path::Path,
    name: &str,
    existed: bool,
    e: impl std::fmt::Display,
) -> ! {
    eprintln!("error: {e}");
    if existed {
        eprintln!(
            "note: [dependencies.{name}] in jade.toml was already replaced, and is left as it is"
        );
    } else if matches!(manifest::remove_dependency(root, name), Ok(true)) {
        eprintln!("note: {name} was not added — jade.toml is unchanged");
    }
    std::process::exit(1);
}

/// Whether `[dependencies.<name>]` is already in the manifest.
///
/// Read before `add` writes, so a rollback can tell "undo what I just created"
/// from "leave what was already there".
fn dependency_exists(root: &std::path::Path, name: &str) -> bool {
    project::load_project(root)
        .ok()
        .and_then(|m| m.dependencies)
        .is_some_and(|d| d.contains_key(name))
}

/// Re-resolve every dependency and write `jade.lock`, then install.
fn relock_and_install(root: &std::path::Path, manifest: &ProjectManifest) {
    try_relock_and_install(root, manifest).unwrap_or_else(|e| fail(e));
}

/// The same work, handing the error back instead of exiting.
///
/// `jade pkg add` needs this so it can undo its manifest edit before it dies;
/// every other caller has nothing to undo and uses the exiting form above.
fn try_relock_and_install(
    root: &std::path::Path,
    manifest: &ProjectManifest,
) -> Result<(), String> {
    let resolved = relock_and_fetch(root, manifest)?;
    pkg::build_c_shims(root, &resolved, manifest)
}

/// Everything up to the binding shims: resolve, write `jade.lock`, copy the
/// artifacts into `libs/`.
///
/// Split out for the one case that legitimately cannot build a shim yet — a C
/// dependency added with placeholder prototypes. Stopping short of the lock
/// entirely would be worse: `jade run` refuses to resolve on its own, so the
/// user would fill in the blanks, run their program, and be sent to
/// `jade pkg install` for a step that had nothing to do with what they just
/// fixed. Locking now leaves the shim as the only thing missing, which is
/// exactly what filling in the blanks completes.
fn relock_and_fetch(
    root: &std::path::Path,
    manifest: &ProjectManifest,
) -> Result<lock::Lockfile, String> {
    let fetcher = HttpFetcher::new();
    let resolved = pkg::resolve(root, manifest, &fetcher)?;
    lock::write(root, &resolved)?;
    pkg::materialize(root, &resolved, &fetcher)?;
    Ok(resolved)
}

// ── reading a header ──────────────────────────────────────────────────────────

/// Everything that says *how* to read a header, rather than which one.
///
/// One value instead of three parameters because `add`, `bind` and `install`
/// each take the same three and pass them straight through to the same place.
/// They travel together, so a fourth one arrives in one signature rather than
/// four.
#[derive(Clone, Copy, Default)]
pub struct HeaderOptions<'a> {
    /// Extra `-I` directories, in the order the user gave them.
    pub include: &'a [String],
    /// `-D` macros, defined before the header is read. Some headers require
    /// one: `pcre2.h` raises `#error` unless `PCRE2_CODE_UNIT_WIDTH` is set,
    /// and `fuse.h` unless `FUSE_USE_VERSION` is.
    pub defines: &'a [String],
    /// Bind only the symbols whose name contains this. A large header can then
    /// be bound a piece at a time, or a single symbol taken out of it.
    pub only: Option<&'a str>,
}

/// Refuse a `-D` value that is not a macro definition.
///
/// The string reaches two compilers — clang, when the header is read, and `cc`,
/// when the shim is built — so it is checked once here, where the user typed
/// it, rather than being discovered as a compiler diagnostic much later. The
/// accepted shape is C's own: a name, optionally `=` and a value.
pub(crate) fn check_defines(defines: &[String]) -> Result<(), String> {
    for d in defines {
        let name = d.split('=').next().unwrap_or(d);
        let ok = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!(
                "'{d}' is not a macro definition. -D takes a name, and optionally a value:\n  \
                 -D PCRE2_CODE_UNIT_WIDTH=8      the header's own configuration macro\n  \
                 -D NDEBUG                       a name on its own"
            ));
        }
    }
    Ok(())
}

// ── add ───────────────────────────────────────────────────────────────────────

/// `jade pkg add <name> --path <p> | --url <u> [--version <v>] [--header <h.h>]`
///
/// A `--header` says the artifact is a plain C library *and* how to bind it, so
/// adding one is a single step: the symbol table is generated and the shim is
/// built before the command returns. Requiring a separate `jade pkg bind` was
/// asking the user to know about a stage that has no decision in it.
pub fn run_add(
    name: &str,
    path: Option<&str>,
    url: Option<&str>,
    version: Option<&str>,
    c_abi: bool,
    header: Option<&str>,
    opts: HeaderOptions<'_>,
) {
    let root = root_or_exit();

    // Before the manifest is touched, since a bad -D is a typo to fix rather
    // than a state to roll back.
    check_defines(opts.defines).unwrap_or_else(|e| fail(e));

    let source = match (path, url) {
        (Some(p), None) => manifest::Source::Path(p),
        (None, Some(u)) => manifest::Source::Url(u),
        (Some(_), Some(_)) => {
            eprintln!("error: --path and --url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("error: `jade pkg add {name}` needs a source: --path <file> or --url <url>");
            eprintln!("       there is no package registry, so a dependency names where it lives");
            std::process::exit(1);
        }
    };

    // Catch a missing file before touching jade.toml. Resolution would catch it
    // anyway, but only after the entry was written, and "no such file" is a much
    // better answer here than the same fact reported as a resolution failure.
    if let Some(p) = path {
        let full = root.join(p);
        if !full.exists() {
            eprintln!("error: {p} does not exist");
            eprintln!(
                "       a --path dependency names a file to copy, relative to the project root"
            );
            std::process::exit(1);
        }
        // A dependency is a *loadable* shared library, and nothing downstream
        // checks that. Without this, a file that merely has the right name is
        // copied into libs/, resolved, linked and built, and first refused by
        // the dynamic loader when the finished program runs.
        if !pkg::bindgen::is_loadable_object(&full) {
            // A `.tbd` is not a mistake, so it does not get the mistake's
            // message. On a modern macOS the SDK ships only these for system
            // libraries — the real ones are inside the dyld shared cache and
            // have no file on disk — so there is nothing here for Jade to copy
            // into `libs/`, and saying "this is not a shared library" sends the
            // reader looking for a corrupt file instead of a different library.
            if let Some(install) =
                std::fs::read(&full).ok().and_then(|b| pkg::bindgen::tbd_install_name(&b))
            {
                // Accepted. A stub is not Mach-O and never will be, but it is
                // exactly what a linker wants: linking the shim against it
                // records `install` as the shim's own dependency, and dyld
                // resolves that from the shared cache at load time. Nothing has
                // to be copied and nothing has to be opened by hand — which is
                // the whole reason a system library can be bound at all now.
                println!("  stub for {install}, resolved from the dyld shared cache");
            } else {
                eprintln!("error: {p} is not a shared library");
                eprintln!(
                    "       A dependency is a prebuilt .dylib or .so. This file does not start \
                     with\n       a Mach-O or ELF header, so nothing could load it.\n\n       \
                     The usual cause is compiling the header instead of the source:\n         \
                     clang -o lib{0}.dylib {0}.h      # makes a precompiled header, not a \
                     library\n         \
                     clang -dynamiclib -o lib{0}.dylib {0}.c",
                    name
                );
                std::process::exit(1);
            }
        }
    }

    let existed = dependency_exists(&root, name);

    // A local artifact can be read for its export table — which says both what
    // kind of library it is and, later, whether a candidate header describes it.
    let lib_path = path.map(|p| root.join(p)).filter(|p| p.exists());

    // What the user said, then what the artifact says, then the default. A
    // header is only meaningful for a plain C library, so passing one is itself
    // a statement of the ABI.
    let abi = if c_abi || header.is_some() {
        Abi::C
    } else {
        match lib_path.as_deref().and_then(detect_abi) {
            Some(Abi::C) => {
                println!("{name} exports no jade_pkg_init, so it is a plain C library");
                Abi::C
            }
            Some(Abi::Jade) => Abi::Jade,
            // A URL dependency has nothing to read yet, and an unreadable table
            // proves nothing. Assume a Jade package, which is what `--c-abi` is
            // there to correct.
            None => Abi::Jade,
        }
    };

    manifest::add_dependency(&root, name, source, version, abi, None).unwrap_or_else(|e| fail(e));

    // Given, or found. A .so has no headers in it — names only, and C does not
    // mangle them — so one has to come from the filesystem; but the user should
    // not have to know where. `libsqlite3.dylib` implies `sqlite3.h`, and the
    // export table says whether the one we found is really this library's.
    let found = header.map(std::path::PathBuf::from).or_else(|| {
        if abi != Abi::C {
            return None;
        }
        let lib = lib_path.as_deref()?;
        let h = pkg::bindgen::discover_header(lib, &root, name)?;
        println!("found header {}", h.display());
        Some(h)
    });

    if let Some(h) = found {
        match bind_header(&root, name, &h.to_string_lossy(), opts, false, lib_path.as_deref()) {
            Ok(()) => {}
            // Nothing was written, so there is nothing here that a later command
            // could use — the entry goes with it.
            Err(e @ BindFailure::Unwritten(_)) => fail_new_dependency(&root, name, existed, e),
            // The header was read and is now recorded on the dependency, and
            // that is precisely why the entry stays. Rolling it back would
            // delete the header the skip report just said was recorded, and
            // send the user off to write a table with no header behind it —
            // where `int` is Jade's width standing in for the library's.
            //
            // Nothing is locked or installed, because a C dependency with no
            // symbols does not resolve: the table the user is being asked to
            // write is the missing half, and `jade pkg install` is what
            // completes the rest once it is there. Said out loud rather than
            // left to be discovered, since until then every other `pkg` command
            // reports the same gap.
            Err(e @ BindFailure::NothingBound(_)) => {
                eprintln!("{e}");
                println!("added {name} to jade.toml, with its header and no symbols");
                eprintln!(
                    "note: nothing bound, so {name} has no binding yet. Write the table\n  \
                     [dependencies.{name}.symbols] by hand — the reasons above name the spelling \
                     each\n  symbol needs, and the header is recorded, so what you write is \
                     checked against\n  the library's own prototypes — then run\n    \
                     jade pkg install"
                );
                return;
            }
        }
    } else if abi == Abi::C {
        // `--only` narrows a header, and there is no header here. Said out
        // loud, because a filter that was quietly dropped reads afterwards as
        // one that matched nothing.
        if opts.only.is_some() {
            eprintln!(
                "note: --only had nothing to narrow — it selects declarations out of a header, \
                 and none was read."
            );
        }

        // No header, but the library still says what it exports. Write those
        // names with `"?"` for the prototype rather than nothing at all: the
        // user fills in blanks in a file that already lists every function,
        // instead of going to look for a header that may not exist on this
        // machine. Guessing the types would be worse than leaving them blank —
        // see `project::UNRESOLVED`.
        let found = lib_path.as_deref().map(pkg::bindgen::placeholder_symbols).unwrap_or_default();
        if !found.is_empty() {
            let names: Vec<&str> = found.keys().map(String::as_str).collect();
            let empty = std::collections::BTreeMap::new();
            manifest::set_bindings(&root, name, &found, &empty, &[], &[], &[])
                .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));

            // Lock and copy the artifact, but do not try to bind it — filling
            // in the prototypes is then the only thing left between here and a
            // working dependency.
            let m = load_or_exit(&root);
            relock_and_fetch(&root, &m)
                .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));

            println!("added {name} to jade.toml");
            println!(
                "{} of its symbols are listed there with no signature: {}",
                names.len(),
                summarize(&names)
            );
            // The header leads, because it answers this for every symbol at
            // once and cannot be got wrong. The hand-written form names the C
            // type rather than a Jade one: with no header the shim writes each
            // declaration itself, so `int` there would be Jade's width standing
            // in for the library's, which `cshim` refuses by name.
            eprintln!(
                "note: A shared library says what it exports and nothing more — C keeps no \
                 argument or return\n  types in a compiled artifact — so no header was found to \
                 read them from. If you have\n  one, this generates the whole table:\n    \
                 jade pkg bind {name} --header <its header.h>\n  \
                 Otherwise replace each \"?\" under [dependencies.{name}.symbols] by hand, \
                 naming the C\n  type the library declares:\n    \
                 [dependencies.{name}.symbols.{}]\n    \
                 args = [\"scalar:<ctype>\", \"scalar:<ctype>\"]\n    \
                 ret  = \"scalar:<ctype>\"",
                names[0]
            );
            return;
        }

        // Nothing readable at all: a URL dependency has no artifact yet, and a
        // library that exports nothing bindable has no names to offer either.
        println!("added {name} to jade.toml");
        eprintln!(
            "note: {name} is a C library and no header for it was found, so it has no symbols \
             yet.\n  A shared library carries no headers — only symbol names, and C does not \
             mangle them — so\n  one has to be pointed at:\n    \
             jade pkg add {name} --path <the .so> --header <its header.h>\n  \
             Or write a [dependencies.{name}.symbols] table by hand, which can cover what the\n  \
             generator will not guess at."
        );
        return;
    }

    let manifest = load_or_exit(&root);
    try_relock_and_install(&root, &manifest)
        .unwrap_or_else(|e| fail_new_dependency(&root, name, existed, e));
    println!("added {name}");

    // A Jade package can say what it needs installed beside it. Adding one
    // brings those with it, so a package with dependencies of its own works
    // without the user reading its documentation and adding them by hand.
    if abi == Abi::Jade
        && let Some(installed) = lib_path.as_deref().and_then(pkg::declared_dependencies)
    {
        add_declared_dependencies(&root, name, installed);
    }
}

/// Add a package's own dependencies to this project.
///
/// Written into `jade.toml` rather than straight into the lock, so the two stay
/// in agreement — a lock entry with no manifest entry is exactly what
/// `verify_in_sync` refuses, and rightly: the manifest is what a person reads to
/// know what their project depends on. A transitive dependency is a real
/// dependency, so it says so.
///
/// **Only a `url` dependency travels.** A `path` names a file on the machine
/// that built the package, and that path means nothing here — the directory may
/// not exist, and if it does it may hold something else. Rather than write a
/// reference that resolves to the wrong file or to none, those are named and
/// left to the user. That is the honest boundary of what an artifact can tell
/// you about itself.
///
/// Flat entries, like every other one: this adds no version solving. A name
/// already present at a *different* version is refused, and not only because
/// there is no solver — two versions are two files, two paths, and therefore two
/// loaded copies with their own state, which for a library that owns a device is
/// two devices.
fn add_declared_dependencies(
    root: &std::path::Path,
    from: &str,
    declared: Vec<pkg::lock::LockedPackage>,
) {
    let manifest = load_or_exit(root);
    let empty = Default::default();
    let have = manifest.dependencies.as_ref().unwrap_or(&empty);

    let mut added: Vec<String> = Vec::new();
    let mut local: Vec<String> = Vec::new();
    let mut upgraded: Vec<String> = Vec::new();

    for dep in declared {
        if let Some(existing) = have.get(&dep.name) {
            let mine = existing.version.as_deref().unwrap_or(pkg::LOCAL_VERSION);
            if mine == dep.version {
                // Two packages needing the same dependency is the ordinary case.
                continue;
            }

            // One version is loaded per program, so the two have to become one.
            // The higher of them is chosen — Go's rule, and the only one
            // available without a registry: there is no third version to go and
            // fetch, so the choice is between the two already named.
            //
            // Only when both are URLs and both are orderable. A path names a
            // file on this machine and a version like `local` orders against
            // nothing, so those fall through to the refusal below and the user
            // decides.
            let both_urls = existing.url.is_some() && dep.source.starts_with("url+");
            match pkg::compare_versions(&dep.version, mine).filter(|_| both_urls) {
                // Keep what is here. The package asked for an older one and will
                // get this instead.
                Some(std::cmp::Ordering::Less) => {
                    upgraded.push(format!(
                        "{} {} over the {} it asked for",
                        dep.name, mine, dep.version
                    ));
                    continue;
                }
                // Take the package's, which is newer than what this project had.
                Some(std::cmp::Ordering::Greater) => {
                    let url = dep.source.strip_prefix("url+").expect("checked by both_urls");
                    let abi = if dep.abi == project::Abi::C.as_str() {
                        project::Abi::C
                    } else {
                        project::Abi::Jade
                    };
                    manifest::add_dependency(
                        root,
                        &dep.name,
                        manifest::Source::Url(url),
                        Some(dep.version.as_str()),
                        abi,
                        None,
                    )
                    .unwrap_or_else(|e| fail(e));
                    upgraded.push(format!(
                        "{} {} over the {} this project had",
                        dep.name, dep.version, mine
                    ));
                    continue;
                }
                Some(std::cmp::Ordering::Equal) => continue,
                None => {}
            }

            fail(format!(
                "'{from}' needs {} {}, and this project already has {} {}.\n  \
                 One version of a dependency is loaded per program, so both cannot be \
                 installed —\n  a second copy would have its own state. The higher of two \
                 versions is normally\n  chosen, but these two cannot be ordered: that needs \
                 both to come from a URL and to be\n  written as dotted numbers. Align them, \
                 or drop one of the two packages that disagree.",
                dep.name, dep.version, dep.name, mine
            ));
        }

        match dep.source.strip_prefix("url+") {
            Some(url) => {
                let version = (dep.version != pkg::LOCAL_VERSION).then_some(dep.version.as_str());
                let abi = if dep.abi == project::Abi::C.as_str() {
                    project::Abi::C
                } else {
                    project::Abi::Jade
                };
                manifest::add_dependency(
                    root,
                    &dep.name,
                    manifest::Source::Url(url),
                    version,
                    abi,
                    None,
                )
                .unwrap_or_else(|e| fail(e));
                added.push(dep.name);
            }
            None => local.push(dep.name),
        }
    }

    if !local.is_empty() {
        eprintln!(
            "note: '{from}' also needs {}, which it names by a local path.\n  \
             A path points at a file on the machine that built '{from}', so it cannot be \
             followed\n  from here. Add each one yourself:\n    \
             jade pkg add {} --path <where it is on this machine>",
            local.join(", "),
            local[0]
        );
    }

    // Said out loud, never silently. Choosing the higher version means one of
    // the two packages runs against something other than what it named, and if
    // that version dropped something it uses, the failure is a missing symbol at
    // run time. Naming the substitution is what makes that traceable.
    for note in &upgraded {
        println!("using {note}");
    }

    if added.is_empty() && upgraded.is_empty() {
        return;
    }
    if !added.is_empty() {
        println!("{from} also needs {}", added.join(", "));
    }

    let manifest = load_or_exit(root);
    try_relock_and_install(root, &manifest).unwrap_or_else(|e| fail(e));
}

/// A readable list of names, cut off before it fills the terminal.
fn summarize(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    let mut s = names.iter().take(SHOWN).copied().collect::<Vec<_>>().join(", ");
    if names.len() > SHOWN {
        s.push_str(&format!(", and {} more", names.len() - SHOWN));
    }
    s
}

/// Which ABI an artifact speaks, read from the artifact itself.
///
/// A Jade package exports `jade_pkg_init`; a plain C library does not. That is
/// not a heuristic — it is the same symbol the loader requires at run time, so
/// anything answering "Jade" here is exactly what `use` will later accept.
/// Both kinds are a `.dylib`, so the file extension says nothing and only the
/// symbol table can tell them apart.
///
/// `None` when the table cannot be read, which is a reason to fall back on what
/// the user said rather than to guess.
fn detect_abi(lib: &std::path::Path) -> Option<Abi> {
    let syms = pkg::bindgen::exported_symbols(lib)?;
    Some(if syms.contains("jade_pkg_init") { Abi::Jade } else { Abi::C })
}

// ── binding a C library ───────────────────────────────────────────────────────

/// The header path and include directories to record for a dependency.
///
/// Absolute, because the shim is compiled inside `libs/<dep>/` rather than
/// wherever the command was run — a relative `-I` resolves against the wrong
/// directory, and the failure is a "file not found" from cc at install time,
/// well away from the cause.
///
/// **A header is recorded the way it was written wherever that is possible, and
/// by its bare name only when it is not.** Recording `netlink/netlink.h` as
/// `netlink.h` makes it resolvable only by putting `/usr/include/libnl3/netlink`
/// on the include path — and libnl ships `netlink/errno.h`, so the shim's own
/// `#include <errno.h>` five lines from the top then binds to a file of `NLE_*`
/// constants. The shim fails to compile on `errno`, and the compiler's note says
/// to include `<errno.h>`, which is already there. Nineteen headers on an
/// ordinary Linux image can shadow one the shim includes — `uv/errno.h`,
/// `linux/string.h`, all of `bsd/` — and each needs only one fallible symbol in
/// the binding to bring `errno` into the shim and trip it.
///
/// Keeping the nested spelling means the *root* goes on the path and the leaf
/// directory never does, which removes the whole class rather than one header of
/// it. So exactly one directory is dropped: the header's own. Its parent stays,
/// because that is what `brotli/encode.h`'s `#include <brotli/port.h>` resolves
/// against.
///
/// This is the one place where what clang was given and what `cc` will be given
/// deliberately differ, and only by that leaf. Reading the header keeps the
/// wider list, so nothing the parse could reach becomes unreachable; the shim
/// compile reaches the same neighbours through the nested spelling.
pub(crate) fn header_locations(
    header: &std::path::Path,
    include: &[String],
) -> (Vec<String>, Vec<String>) {
    // The same list clang was given, so the shim compile cannot be missing a
    // directory the parse needed. The two used to be computed separately, and
    // the parse got the smaller set.
    let mut dirs = pkg::bindgen::include_roots(header, include);

    let Some((spelling, root)) = nested_spelling(header, include, std::path::Path::new(".")) else {
        let bare = header
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| header.to_string_lossy().into_owned());
        return (vec![bare], dirs);
    };

    if let Some(own) = header.parent().filter(|p| !p.as_os_str().is_empty()).map(absolute) {
        dirs.retain(|d| *d != own);
    }
    if !dirs.contains(&root) {
        // The directory the spelling resolves against. Only reached when the
        // header was written relative to the working directory, since a root
        // the user named with -I is already in the list.
        dirs.push(root);
    }
    (vec![spelling], dirs)
}

/// How to spell a header so a directory-qualified `#include` finds it, and the
/// directory that has to be on the include path for that to work.
///
/// `None` when there is no such spelling — a header sitting directly in an
/// include root, which is the common case and needs nothing done to it.
///
/// Two ways to arrive at one, and both are the user's own words rather than a
/// guess. A relative path is taken as written: `--header inc/mylib.h` becomes
/// `#include <inc/mylib.h>` against the working directory. An absolute path is
/// spelled relative to the deepest `-I` directory that contains it, because
/// naming a directory with `-I` is the user saying it is an include root.
///
/// A path derived from the header rather than named by the user is deliberately
/// not a candidate. `/opt/homebrew/include/libfdt.h` sits under `/opt/homebrew`
/// as `include/libfdt.h`, and taking that spelling would drop
/// `/opt/homebrew/include` from the path — where `libfdt.h`'s own
/// `#include <libfdt_env.h>` has to find its neighbour.
///
/// `cwd` is what a relative header is relative *to*. It is a parameter so the
/// tests can name a directory of their own: `cargo test` runs in parallel, so
/// nothing here may change the process's working directory.
pub(crate) fn nested_spelling(
    header: &std::path::Path,
    include: &[String],
    cwd: &std::path::Path,
) -> Option<(String, String)> {
    use std::path::{Component, Path};

    let has_dir =
        |p: &Path| p.parent().is_some_and(|d| !d.as_os_str().is_empty() && d != Path::new("."));

    // Written relative, with a directory in it. `..` is excluded: it spells the
    // same file from the working directory only, and the recorded path is
    // replayed from `libs/<dep>/`.
    if header.is_relative() && has_dir(header) {
        let plain = header.components().all(|c| matches!(c, Component::Normal(_)));
        if plain {
            return Some((slashed(header), absolute(cwd)));
        }
    }

    // Under a directory the user named with -I. The deepest one wins: it gives
    // the shortest spelling, and it is the directory the library's own headers
    // include each other through.
    let full = std::fs::canonicalize(header).ok()?;
    let mut best: Option<(String, String)> = None;
    for dir in include {
        let Ok(root) = std::fs::canonicalize(dir) else { continue };
        let Ok(rel) = full.strip_prefix(&root) else { continue };
        if !has_dir(rel) {
            continue;
        }
        let better = best.as_ref().is_none_or(|(s, _)| slashed(rel).len() < s.len());
        if better {
            best = Some((slashed(rel), root.to_string_lossy().into_owned()));
        }
    }
    best
}

/// A path as an `#include` spells it, with forward slashes.
fn slashed(p: &std::path::Path) -> String {
    p.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The same absolute spelling `bindgen::include_roots` produces, so the two
/// lists can be compared string against string.
fn absolute(p: &std::path::Path) -> String {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
}

/// Why a header produced no symbol table, and what `jade.toml` holds because of
/// it.
///
/// The two are not the same failure and must not be reported as one. A header
/// that could not be read leaves the manifest untouched; a header that was read
/// and every symbol of which was skipped leaves the header *recorded*, which is
/// the state a hand-written table needs — without it `int` in that table means
/// Jade's 64-bit width rather than whatever the library declared, which is
/// exactly the trap the width check exists to catch.
///
/// So `jade pkg add` has to tell them apart before deciding whether to roll its
/// entry back. It used to roll back on both, which deleted the header the
/// message in the same breath said had been recorded.
pub(crate) enum BindFailure {
    /// Nothing was written. The header could not be read, or it describes some
    /// other library.
    Unwritten(String),
    /// The header was read and recorded on the dependency; no symbol survived
    /// binding.
    NothingBound(String),
}

impl std::fmt::Display for BindFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindFailure::Unwritten(m) | BindFailure::NothingBound(m) => f.write_str(m),
        }
    }
}

/// Read a header and write the tables into `jade.toml`. Shared by `add`,
/// `install` and `bind`, so all three produce the same manifest.
pub(crate) fn bind_header(
    root: &std::path::Path,
    name: &str,
    header: &str,
    opts: HeaderOptions<'_>,
    quiet: bool,
    lib: Option<&std::path::Path>,
) -> Result<(), BindFailure> {
    let header_path = std::path::Path::new(header);
    if !header_path.exists() {
        return Err(BindFailure::Unwritten(format!("no such header: {header}")));
    }
    check_defines(opts.defines).map_err(BindFailure::Unwritten)?;

    // Read the export table first: it is both the check below and, for an
    // umbrella header that declares nothing itself, what decides which of the
    // headers it includes to bind.
    let exported = lib.and_then(pkg::bindgen::exported_symbols);
    // An artifact that was named and could not be read is worth saying out
    // loud. Without a table an umbrella header fails with a message telling you
    // to pass `--path`, which you just did — and every other header quietly
    // binds less, since what it includes has nothing to be selected against.
    if lib.is_some() && exported.is_none() {
        return Err(BindFailure::Unwritten(format!(
            "could not read the export table of {}. Nothing is wrong with the header — the \
             library's symbols are what say which of its declarations to bind, and `nm` reported \
             none.\n  A stripped static archive does that, as does a file that is not a library at \
             all. Check that\n  the path names a shared library, and that `nm -D {}` lists \
             something.",
            lib.map(|l| l.display().to_string()).unwrap_or_default(),
            lib.map(|l| l.display().to_string()).unwrap_or_default()
        )));
    }
    let binding = pkg::bindgen::from_header(
        header_path,
        opts.include,
        opts.defines,
        opts.only,
        exported.as_ref(),
    )
    .map_err(BindFailure::Unwritten)?;

    // Check the header against the library it is supposed to describe. A header
    // declaring symbols the library does not export is the wrong header, and
    // the shim would fail to link with an undefined-symbol error naming none of
    // this.
    if let Some(exported) = &exported {
        let (covered, total) = pkg::bindgen::coverage(&binding, exported);
        if covered == 0 && !binding.symbols.is_empty() {
            return Err(BindFailure::Unwritten(format!(
                "{header} declares none of the {total} symbols {} exports — it looks like the \
                 wrong header for this library.",
                lib.map(|l| l.display().to_string()).unwrap_or_default()
            )));
        }
        if !quiet {
            println!("covers {covered} of the {total} symbols the library exports");
        }
    }

    // Where the header is gets recorded even when nothing came out of it, and
    // that is the whole point of writing it before the check below. `--only` on
    // a symbol the generator refuses is an ordinary way to arrive here: the skip
    // report says to write the stanza by hand, and a stanza written against a
    // dependency with no `headers` is a *headerless* binding, where `int` means
    // Jade's 64-bit width rather than whatever the library declared. Following
    // the instruction landed the user in exactly the trap the width check exists
    // to catch. A header clang could read is a fact about the dependency,
    // whether or not any symbol survived it.
    let (headers, dirs) = header_locations(header_path, opts.include);
    manifest::set_bindings(
        root,
        name,
        &binding.symbols,
        &binding.structs,
        &headers,
        &dirs,
        opts.defines,
    )
    .map_err(BindFailure::Unwritten)?;

    if binding.symbols.is_empty() {
        return Err(BindFailure::NothingBound(format!(
            "{}\nnothing in {header} could be bound. The reasons above say why; a symbol table \
             written by hand can still cover what this could not — and [dependencies.{name}] now \
             records the header, so what you write is checked against the library's own \
             prototypes rather than standing in for them.",
            binding.report()
        )));
    }

    if !quiet {
        println!("{}", binding.report());
    }
    Ok(())
}

// ── bind ──────────────────────────────────────────────────────────────────────

/// `jade pkg bind <name> --header <h.h> [-I dir] [--only text] [--dry-run]`
///
/// Reads the header with clang and writes the symbol table into `jade.toml`, so
/// a library with two hundred entry points does not have to be transcribed by
/// hand. What it *could not* bind is printed, with reasons: a generator that
/// silently covers two thirds of an API is how the missing third is found at
/// run time.
pub fn run_bind(name: &str, header: &str, opts: HeaderOptions<'_>, dry_run: bool) {
    let root = root_or_exit();
    check_defines(opts.defines).unwrap_or_else(|e| fail(e));

    if dry_run {
        // Report only. Useful for looking at a large header before committing
        // its table to the manifest.
        let header_path = std::path::Path::new(header);
        if !header_path.exists() {
            fail(format!("no such header: {header}"));
        }
        // A dry run may still know the artifact, and an umbrella header cannot
        // be read without it.
        let exported = load_or_exit(&root)
            .dependencies
            .as_ref()
            .and_then(|d| d.get(name))
            .and_then(|e| e.path.clone())
            .map(|p| root.join(p))
            .filter(|p| p.exists())
            .and_then(|p| pkg::bindgen::exported_symbols(&p));
        let binding = pkg::bindgen::from_header(
            header_path,
            opts.include,
            opts.defines,
            opts.only,
            exported.as_ref(),
        )
        .unwrap_or_else(|e| fail(e));
        println!("{}", binding.report());
        println!("\n(dry run — jade.toml unchanged)");
        return;
    }

    // The dependency's own artifact, so the header can be checked against the
    // library it claims to describe before anything is written.
    let lib = load_or_exit(&root)
        .dependencies
        .as_ref()
        .and_then(|d| d.get(name))
        .and_then(|e| e.path.clone())
        .map(|p| root.join(p))
        .filter(|p| p.exists());

    bind_header(&root, name, header, opts, false, lib.as_deref()).unwrap_or_else(|e| fail(e));
    println!("\nwrote [dependencies.{name}.symbols] to jade.toml");

    // Build it too. Re-binding and then leaving the shim stale is never what
    // anyone wanted, and it is the step that would otherwise be forgotten.
    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);
    println!("installed {name}");
}

/// Bind every `abi = "c"` dependency that names a header but has no symbols.
/// Returns whether the manifest was changed.
fn bind_missing_symbols(root: &std::path::Path, manifest: &ProjectManifest) -> bool {
    let Some(deps) = &manifest.dependencies else { return false };
    let mut changed = false;

    for (name, entry) in deps {
        if entry.abi != Abi::C || entry.symbols.is_some() {
            continue;
        }
        let Some(headers) = entry.headers.as_ref().filter(|h| !h.is_empty()) else { continue };

        // The manifest records the header the way an `#include` spells it, plus
        // the directories to find it in, so the lookup here is the same one the
        // shim compile will do.
        let dirs = entry.include_dirs.clone().unwrap_or_default();
        let found = dirs
            .iter()
            .map(|d| std::path::Path::new(d).join(&headers[0]))
            .find(|p| p.exists())
            .or_else(|| Some(root.join(&headers[0])).filter(|p| p.exists()));

        let lib = entry.path.as_ref().map(|p| root.join(p)).filter(|p| p.exists());
        let Some(path) = found else {
            eprintln!(
                "note: dependency '{name}' names header '{}' but it was not found, so no symbols \
                 were generated. Point at it with\n  jade pkg bind {name} --header <path>",
                headers[0]
            );
            continue;
        };

        println!("binding {name} from {}", headers[0]);
        let opts = HeaderOptions { include: &dirs, ..Default::default() };
        match bind_header(root, name, &path.to_string_lossy(), opts, false, lib.as_deref()) {
            Ok(()) => changed = true,
            Err(e) => eprintln!("note: could not bind '{name}': {e}"),
        }
    }
    changed
}

// ── remove ────────────────────────────────────────────────────────────────────

/// `jade pkg remove <name>` — drop it from the manifest, the lock, and `libs/`.
pub fn run_remove(name: &str) {
    let root = root_or_exit();

    // Capture the install directory before the lock loses the entry.
    let install_dir =
        lock::read(&root).ok().flatten().and_then(|l| l.get(name).map(|p| p.install_dir()));

    let removed = manifest::remove_dependency(&root, name).unwrap_or_else(|e| fail(e));
    if !removed {
        eprintln!("error: no dependency named '{name}' in jade.toml");
        std::process::exit(1);
    }

    let manifest = load_or_exit(&root);
    relock_and_install(&root, &manifest);

    if let Some(dir) = install_dir {
        let path = root.join(pkg::LIBS_DIR).join(dir);
        // Already gone is fine — `libs/` is disposable and may never have been
        // populated on this machine.
        match std::fs::remove_dir_all(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("warning: could not remove {}: {e}", path.display());
            }
            _ => {}
        }
    }

    println!("removed {name}");
}

// ── install ───────────────────────────────────────────────────────────────────

/// `jade pkg install [--locked]`
///
/// Without `--locked`, resolves anything the lock is missing and writes it.
/// With `--locked`, refuses to change the lock — the CI mode, where a manifest
/// edit that was never locked should fail rather than silently resolve.
pub fn run_install(locked: bool) {
    let root = root_or_exit();
    let mut manifest = load_or_exit(&root);

    let has_deps = manifest.dependencies.as_ref().is_some_and(|d| !d.is_empty());
    if !has_deps {
        println!("no dependencies");
        return;
    }

    // Bind anything that says which header to read but has no symbols yet.
    // Without this a hand-written entry naming a header would install a shim
    // with nothing in it, and the user would have to know that a separate
    // command exists to fill it.
    //
    // Only when `symbols` is *absent*: a committed manifest already carries
    // them, so a fresh clone installs without needing clang at all. Re-running
    // after a header changes is `jade pkg bind`, which is an explicit act.
    if !locked && bind_missing_symbols(&root, &manifest) {
        manifest = load_or_exit(&root);
    }

    let existing = lock::read(&root).unwrap_or_else(|e| fail(e));

    if locked {
        let lock = existing.unwrap_or_else(|| fail("--locked was given but there is no jade.lock"));
        pkg::verify_in_sync(&manifest, &lock).unwrap_or_else(|e| fail(e));
        // A rebuilt local dependency is a stale lock, and this mode is where a
        // stale lock has to be an error rather than a fixup.
        pkg::verify_local_unchanged(&root, &lock).unwrap_or_else(|e| fail(e));
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        pkg::build_c_shims(&root, &lock, &manifest).unwrap_or_else(|e| fail(e));
        println!("installed {} dependencies from jade.lock", lock.packages.len());
        return;
    }

    // An in-sync lock is authoritative for anything with a URL: reuse it so
    // `jade pkg install` does not re-fetch every artifact just to recompute
    // digests it already has. A local `path` dependency is the exception — its
    // source is a file the user rebuilds, so it gets re-hashed every time.
    if let Some(mut lock) = existing.filter(|l| pkg::verify_in_sync(&manifest, l).is_ok()) {
        let changed = pkg::refresh_local(&root, &mut lock);
        if !changed.is_empty() {
            lock::write(&root, &lock).unwrap_or_else(|e| fail(e));
            println!("re-pinned {} (local source changed)", changed.join(", "));
        }
        pkg::materialize(&root, &lock, &HttpFetcher::new()).unwrap_or_else(|e| fail(e));
        pkg::build_c_shims(&root, &lock, &manifest).unwrap_or_else(|e| fail(e));
        println!("installed {} dependencies", lock.packages.len());
        return;
    }

    relock_and_install(&root, &manifest);
    let n = manifest.dependencies.as_ref().map_or(0, |d| d.len());
    println!("installed {n} dependencies");
}

// ── update ────────────────────────────────────────────────────────────────────

/// `jade pkg update [name]` — re-resolve against the manifest and rewrite the lock.
///
/// This re-fetches to pick up an artifact republished at the same URL. It
/// cannot find a *newer version*: with no registry there is nothing to ask, so
/// moving to 2.0.0 means editing `jade.toml` (or `jade pkg add <name> --version`).
pub fn run_update(name: Option<&str>) {
    let root = root_or_exit();
    let manifest = load_or_exit(&root);

    if let Some(name) = name {
        let known = manifest.dependencies.as_ref().is_some_and(|d| d.contains_key(name));
        if !known {
            eprintln!("error: no dependency named '{name}' in jade.toml");
            std::process::exit(1);
        }
    }

    relock_and_install(&root, &manifest);
    match name {
        Some(n) => println!("updated {n}"),
        None => println!("updated all dependencies"),
    }
}

// ── list ──────────────────────────────────────────────────────────────────────

/// `jade pkg list` — what is locked, and whether it is installed here.
pub fn run_list() {
    let root = root_or_exit();

    let Some(lock) = lock::read(&root).unwrap_or_else(|e| fail(e)) else {
        println!("no dependencies (no jade.lock)");
        return;
    };
    if lock.packages.is_empty() {
        println!("no dependencies");
        return;
    }

    let platform = pkg::fetch::platform_tag().unwrap_or("unsupported");
    for p in &lock.packages {
        let dir = root.join(pkg::LIBS_DIR).join(p.install_dir());
        let here = p.artifacts.get(platform).or_else(|| p.artifacts.get(pkg::ANY_PLATFORM));

        let status = match here {
            // A local dependency whose source has been rebuilt is installed but
            // out of date, and saying only "installed" is how that goes
            // unnoticed. The next `jade pkg install` or `jade run` re-pins it.
            Some(a) if dir.join(&a.file).exists() => match pkg::local_drift(&root, p) {
                true => "installed (local source changed — run `jade pkg install`)",
                false => "installed",
            },
            Some(_) => "not installed",
            None => "unavailable on this platform",
        };
        let platforms: Vec<&str> = p.artifacts.keys().map(String::as_str).collect();
        println!(
            "{name} {version}  [{abi}]  {status}\n  platforms: {plats}",
            name = p.name,
            version = p.version,
            abi = p.abi,
            status = status,
            plats = platforms.join(", "),
        );
    }
}
