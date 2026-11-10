use std::{fs, path::Path, process};

// ── Project-aware run ─────────────────────────────────────────────────────────

/// `jade run` with no argument: find project root and run its entry file.
pub fn run_entry(args: &[String]) {
    let root = crate::project::find_project_root().unwrap_or_else(|| {
        // Fall back to running main.jde in CWD.
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // A jade.toml that is simply not there is the fallback above doing its job:
    // `jade run` outside a project runs `main.jde` in the current directory. One
    // that is there and broken is not — it is the file that says which entry to
    // run, so carrying on with the default would run something the user did not
    // ask for while their real mistake goes unmentioned.
    let manifest = match crate::project::load_project(&root) {
        Ok(m) => m,
        Err(e) if e.is_present() => {
            eprintln!("error: {e}");
            process::exit(1);
        }
        Err(_) => crate::project::ProjectManifest::default(),
    };
    let entry = root.join(manifest.entry_file());

    if !entry.exists() {
        eprintln!("error: entry file '{}' not found", entry.display());
        eprintln!("       run 'jade new <name>' to create a project, or 'jade run <file.jde>'");
        process::exit(1);
    }

    run_file(&entry.to_string_lossy(), args);
}

/// `jade run <script>`: run a named script from [scripts] in jade.toml.
pub fn run_script(name: &str) {
    let root = crate::project::find_project_root().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    let manifest = match crate::project::load_project(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    };

    let scripts = manifest.scripts.unwrap_or_default();
    let cmd = scripts.get(name).cloned().unwrap_or_else(|| {
        eprintln!("error: no script named '{}' in jade.toml", name);
        eprintln!(
            "       available scripts: {}",
            scripts.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        process::exit(1);
    });

    // Execute via shell.
    let status = std::process::Command::new("sh").arg("-c").arg(&cmd).status();

    match status {
        Ok(s) => {
            if !s.success() {
                process::exit(s.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("error: could not run script: {}", e);
            process::exit(1);
        }
    }
}

/// `jade run <file.jde>`: build the file, then run the binary.
///
/// Jade has one engine: the compiler. Running a file builds it into a scratch
/// directory, runs the result with the same standard streams and arguments, and
/// exits with its status. A binary killed by a signal exits as a shell reports
/// one, 128 plus the signal number, so a crash is never mistaken for success.
pub fn run_file(path: &str, args: &[String]) {
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned());
    let stem = stem.unwrap_or_else(|| "main".to_string());

    let scratch = std::env::temp_dir().join(format!("jade-run-{}", process::id()));
    if let Err(e) = fs::create_dir_all(&scratch) {
        eprintln!("error: could not create '{}': {e}", scratch.display());
        process::exit(1);
    }
    let bin = scratch.join(&stem);
    super::build::compile_file(path, &bin, crate::build::Emit::Binary);

    let status = process::Command::new(&bin).args(args).status();
    let _ = fs::remove_dir_all(&scratch);
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not run '{}': {e}", bin.display());
            process::exit(1);
        }
    };
    process::exit(exit_code(status));
}

/// The exit code a shell would report for `status`.
fn exit_code(status: process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(sig)) => 128 + sig,
        (None, None) => 1,
    }
}
