use std::{
    path::{Path, PathBuf},
    process,
};

/// Purpose: This function says where a build writes its binary.
///
/// Inputs:
/// - `source`: the file being compiled.
/// - `output`: the path `-o` gave, if any.
///
/// Outputs: `-o`'s path, else the source's stem beside it.
pub(crate) fn output_path(source: &str, output: Option<&str>) -> PathBuf {
    if let Some(o) = output {
        return PathBuf::from(o);
    }
    let src = Path::new(source);
    let stem = src.file_stem().unwrap_or(src.as_os_str());
    src.parent().unwrap_or(Path::new(".")).join(stem)
}

/// Purpose: This function checks a package's `[package] sources` when the
/// file being built is the package's entry module.
///
/// Inputs:
/// - `source`: the file being built.
///
/// Outputs: none. It exits with a message when the list and the imports
/// disagree.
fn check_package_sources(source: &Path) {
    let Some(dir) = source.canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let Some(root) = crate::project::find_project_root_from(&dir) else { return };
    let Ok(manifest) = crate::project::load_project(&root) else { return };
    let Some(pkg) = manifest.package.as_ref() else { return };
    let entry = root.join(pkg.entry_file());
    let same = |a: &Path, b: &Path| a.canonicalize().ok() == b.canonicalize().ok();
    if same(&entry, source) {
        verify_sources(&root, pkg, &entry, &manifest);
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
    let paths =
        match crate::frontend::lexer::tokenize(&source).and_then(crate::frontend::parser::parse) {
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

    let declared_paths: Vec<(&str, PathBuf)> =
        declared.iter().map(|s| (s.as_str(), canon(&root.join(s)))).collect();

    let mut unreached: Vec<&str> =
        declared_paths.iter().filter(|(_, p)| !reached.contains(p)).map(|(s, _)| *s).collect();
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

/// Purpose: This function runs `jade build`.
///
/// Inputs:
/// - `path`: the file to compile.
/// - `output`: where `-o` says the binary goes, if anywhere.
/// - `emit`: `ir` or `mir` to print that instead of writing a binary.
///
/// Outputs: none. It exits with a message on failure.
pub fn run_build(path: Option<&str>, output: Option<&str>, emit: Option<&str>) {
    let Some(path) = path else {
        eprintln!("error: `jade build` needs a file to compile");
        eprintln!("       run `jade build <file.jde>`");
        process::exit(1);
    };
    check_package_sources(Path::new(path));
    let out = output_path(path, output);

    let emit = match emit {
        None => crate::build::Emit::Binary,
        Some("ir") => crate::build::Emit::Ir,
        Some("mir") => crate::build::Emit::Mir,
        Some(e) => {
            eprintln!("error: unknown --emit format '{e}'; use 'ir' or 'mir'");
            process::exit(1);
        }
    };

    compile_file(path, &out, emit);
    if emit == crate::build::Emit::Binary {
        eprintln!("built: {}", out.display());
    }
}

/// Compile one source file to `out`, exiting with a message on any error.
///
/// Shared by `jade build` and `jade run`, which builds into a scratch directory
/// and then runs the binary. The binary stands alone: a Rust package is linked
/// into it, so there is nothing to copy beside it.
pub(crate) fn compile_file(path: &str, out: &Path, emit: crate::build::Emit) {
    try_compile_file(path, out, emit).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    })
}

/// [`compile_file`], returning the message instead of exiting. `jade test`
/// uses it, since one failing file must not stop the rest.
pub(crate) fn try_compile_file(
    path: &str,
    out: &Path,
    emit: crate::build::Emit,
) -> Result<(), String> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return Err(format!("error: could not read '{}': {}", path, e)),
    };

    // Frontend: lex → parse → the per-file pass.
    let tokens = match crate::frontend::lexer::tokenize(&source) {
        Ok(t) => t,
        Err(e) => return Err(format!("{path}: lexer error: {e}")),
    };
    let program = match crate::frontend::parser::parse(tokens) {
        Ok(p) => p,
        Err(e) => return Err(format!("{path}: parse error: {e}")),
    };
    let program = match crate::typechecker::resolve::resolve(program) {
        Ok(p) => p,
        Err(e) => return Err(format!("{path}: type error: {e}")),
    };

    // Absolute source path so imports resolve relative to it, not the CWD.
    let abs_source = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));

    // Install the project's source packages before resolving imports. Without
    // this a fresh clone builds against nothing at all, and an edited local
    // package is silently ignored in favour of whatever `libs/` last held. The
    // project is found from the source file's directory because that is how
    // `mir/imports.rs` finds it. A manifest that will not parse is reported
    // here rather than skipped, so the error names jade.toml and not a `use`.
    if let Some(root) = abs_source.parent().and_then(crate::project::find_project_root_from) {
        let manifest = crate::project::load_project(&root).map_err(|e| format!("error: {e}"))?;
        crate::pkg::ensure_ready(&root, &manifest).map_err(|e| format!("error: {e}"))?;
    }

    // Make the output path absolute so the artifact lands where the user expects.
    let abs_out = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(out)).unwrap_or_else(|_| out.to_path_buf())
    };

    let lib_cache = crate::codegen::default_lib_cache();
    crate::build::build(&program, &abs_source, &abs_out, emit, Some(&lib_cache))
        .map_err(|e| format!("{path}: build error: {e}"))
}
