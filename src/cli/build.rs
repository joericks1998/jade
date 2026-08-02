use std::{
    path::{Path, PathBuf},
    process,
};

/// Where a build lands when the user did not say.
///
/// The default is the source file's own name without its extension, beside the
/// source.  A `--lib` build gets the platform's shared-library extension
/// instead, because `use <name>` resolves a package by stem and the loader
/// needs a real `.dylib`/`.so` to open.
pub(crate) fn output_path(source: &str, output: Option<&str>, lib: bool) -> PathBuf {
    if let Some(o) = output {
        return PathBuf::from(o);
    }
    let src = Path::new(source);
    let stem = src.file_stem().unwrap_or(src.as_os_str());
    let name = if lib {
        let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        PathBuf::from(format!("{}.{ext}", stem.to_string_lossy()))
    } else {
        PathBuf::from(stem)
    };
    src.parent().unwrap_or(Path::new(".")).join(name)
}

/// What a `jade build` invocation resolved to: which file to compile, where the
/// artifact lands, and which functions a `--lib` build binds.
struct Target {
    source: PathBuf,
    output: Option<String>,
    exports: Vec<String>,
}

/// Work out what to build.
///
/// With a file argument this is the long-standing behavior and nothing consults
/// the manifest. Without one, `--lib` builds the project's `[package]` — its
/// entry, its exports, and its artifact name — which is what makes a package's
/// shape a property of `jade.toml` rather than of the command line somebody
/// remembered to type.
fn resolve_target(
    path: Option<&str>,
    output: Option<&str>,
    lib: bool,
    exports: &[String],
) -> Target {
    if let Some(p) = path {
        return Target {
            source: PathBuf::from(p),
            output: output.map(str::to_string),
            exports: exports.to_vec(),
        };
    }

    if !lib {
        eprintln!("error: `jade build` needs a file to compile");
        eprintln!("       run `jade build <file.jde>`, or `jade build --lib` inside a");
        eprintln!("       project whose jade.toml has a [package] section");
        process::exit(1);
    }

    let Some(root) = crate::project::find_project_root() else {
        eprintln!("error: `jade build --lib` without a file builds the [package] in jade.toml,");
        eprintln!("       but this is not inside a Jade project");
        eprintln!("       run `jade build <file.jde> --lib`, or `jade init` to create a project");
        process::exit(1);
    };

    let manifest = match crate::project::load_project(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let Some(pkg) = manifest.package.as_ref() else {
        eprintln!("error: jade.toml has no [package] section, so there is no package to build");
        eprintln!("       add one naming what this package is:");
        eprintln!();
        eprintln!("           [package]");
        eprintln!("           name    = \"mylib\"");
        eprintln!("           version = \"0.1.0\"");
        eprintln!();
        eprintln!("       or name a file directly: `jade build <file.jde> --lib`");
        process::exit(1);
    };

    if let Err(e) = pkg.validate() {
        eprintln!("error: {e}");
        process::exit(1);
    }

    let entry = root.join(pkg.entry_file());
    if !entry.exists() {
        eprintln!(
            "error: [package] entry '{}' does not exist at {}",
            pkg.entry_file(),
            entry.display()
        );
        eprintln!("       set `entry` in [package], or create the file");
        process::exit(1);
    }

    verify_sources(&root, pkg, &entry, &manifest);

    // A command-line --export wins over the manifest: an explicit flag should
    // not be silently ignored because a config file also had an opinion.
    let exports = if exports.is_empty() {
        pkg.exports.clone().unwrap_or_default()
    } else {
        exports.to_vec()
    };

    Target {
        source: entry,
        output: Some(
            output
                .map(str::to_string)
                .unwrap_or_else(|| root.join(pkg.artifact_file()).to_string_lossy().into_owned()),
        ),
        exports,
    }
}

/// Check `[package].sources` against the files the entry actually imports.
///
/// Both directions are errors, and both are mistakes the import graph cannot
/// report on its own: a declared file nothing imports never makes it into the
/// artifact, and an imported file nobody declared ships without being decided
/// on. Skipped entirely when `sources` is absent.
fn verify_sources(
    root: &Path,
    pkg: &crate::project::PackageSection,
    entry: &Path,
    manifest: &crate::project::ProjectManifest,
) {
    let Some(declared) = &pkg.sources else {
        return;
    };

    let source = match std::fs::read_to_string(entry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read '{}': {e}", entry.display());
            process::exit(1);
        }
    };
    // Lex and parse only. The entry's imports are all this needs, and a type
    // error here belongs to the compile below, with its own message.
    let paths = match crate::frontend::lexer::tokenize(&source)
        .and_then(crate::frontend::parser::parse)
    {
        Ok(program) => crate::project::program_import_paths(&program),
        // Let the real compile report it, in its own words and with its span.
        Err(_) => return,
    };

    let libraries = crate::pkg::resolved_libraries(root, manifest);
    let ctx = crate::project::ImportContext {
        libraries: &libraries,
        project_root: Some(root),
        source_dir: entry.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };

    let mut reached = match crate::project::reachable_jade_modules(&paths, &ctx) {
        Ok(s) => s,
        // An unresolvable import is a real error, but the compile reports it
        // with a span pointing at the `use`. Defer to it.
        Err(_) => return,
    };
    // The entry heads the package, so it counts as one of its files.
    if let Ok(c) = entry.canonicalize() {
        reached.insert(c);
    }

    if let Err(message) = compare_sources(root, declared, &reached, &pkg.entry_file()) {
        eprintln!("error: {message}");
        process::exit(1);
    }
}

/// Compare a package's declared sources against the files actually reached.
///
/// Split out from [`verify_sources`] because it is the whole decision: the rest
/// of that function is reading files and walking imports, and this is the part
/// worth pinning in a test.
pub(crate) fn compare_sources(
    root: &Path,
    declared: &[String],
    reached: &std::collections::HashSet<PathBuf>,
    entry: &str,
) -> Result<(), String> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    // Render a path the way the user wrote it in jade.toml where possible, so
    // the error is something they can search their manifest for.
    let show = |p: &Path| p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned();

    let declared_paths: Vec<(&str, PathBuf)> = declared
        .iter()
        .map(|s| (s.as_str(), canon(&root.join(s))))
        .collect();

    let mut unreached: Vec<&str> = declared_paths
        .iter()
        .filter(|(_, p)| !reached.contains(p))
        .map(|(s, _)| *s)
        .collect();
    let mut undeclared: Vec<String> = reached
        .iter()
        .filter(|p| !declared_paths.iter().any(|(_, d)| d == *p))
        .map(|p| show(p))
        .collect();

    if unreached.is_empty() && undeclared.is_empty() {
        return Ok(());
    }
    unreached.sort();
    undeclared.sort();

    let mut msg =
        String::from("[package] sources in jade.toml does not match what the package imports");
    if !unreached.is_empty() {
        msg.push_str(&format!(
            "\n  declared but never imported: {}\
             \n    nothing reaches these from '{entry}', so they would not be in the artifact",
            unreached.join(", ")
        ));
    }
    if !undeclared.is_empty() {
        msg.push_str(&format!(
            "\n  imported but not declared: {}\
             \n    add them to sources, or stop importing them",
            undeclared.join(", ")
        ));
    }
    Err(msg)
}

/// `jade build <file.jde>` — compile to a native binary.
///
/// Runs the whole pipeline in-process: lex → parse → type-infer → TIR, then
/// import resolution, LLVM code generation, and linking.
///
/// With no file and `--lib`, builds the project's `[package]` instead; see
/// [`resolve_target`].
pub fn run_build(
    path: Option<&str>,
    output: Option<&str>,
    emit_ir: bool,
    lib: bool,
    exports: &[String],
) {
    {
        let target = resolve_target(path, output, lib, exports);
        let path = &target.source.to_string_lossy().into_owned();
        let output = target.output.as_deref();
        let exports = &target.exports[..];

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: could not read '{}': {}", path, e);
                process::exit(1);
            }
        };

        // Frontend: lex → parse → type-infer.
        let tokens = match crate::frontend::lexer::tokenize(&source) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{path}: lexer error: {e}");
                process::exit(1);
            }
        };
        let program = match crate::frontend::parser::parse(tokens) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{path}: parse error: {e}");
                process::exit(1);
            }
        };
        let tprogram = match crate::compiler::type_infer::infer(program) {
            Ok(tp) => tp,
            Err(e) => {
                eprintln!("{path}: type error: {e}");
                process::exit(1);
            }
        };

        // Absolute source path so imports resolve relative to it, not the CWD.
        let abs_source = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));

        // Install the project's dependencies before resolving imports, exactly
        // as `jade run` does. Without this the two engines disagree on what a
        // dependency *is*: `run` picks up a rebuilt local library while `build`
        // silently links whatever `libs/` was last left holding, and a fresh
        // clone builds against nothing at all. The project is found from the
        // source file's directory because that is how `aot/imports.rs` finds it.
        if let Some(root) = abs_source
            .parent()
            .and_then(crate::project::find_project_root_from)
            && let Ok(manifest) = crate::project::load_project(&root)
            && let Err(e) = crate::pkg::ensure_ready(&root, &manifest)
        {
            eprintln!("error: {e}");
            process::exit(1);
        }

        let out = output_path(path, output, lib);
        // Make the output path absolute so the artifact lands where the user expects.
        let abs_out = if out.is_absolute() {
            out.clone()
        } else {
            std::env::current_dir()
                .map(|d| d.join(&out))
                .unwrap_or_else(|_| out.clone())
        };

        let emit = if emit_ir {
            crate::build::Emit::Ir
        } else if lib {
            crate::build::Emit::CDylib { exports: exports.to_vec() }
        } else {
            crate::build::Emit::Binary
        };

        if let Err(e) = crate::build::build(&tprogram, &abs_source, &abs_out, emit) {
            eprintln!("{path}: build error: {e}");
            process::exit(1);
        }

        if !emit_ir {
            eprintln!("built: {}", out.display());
        }
    }
}
