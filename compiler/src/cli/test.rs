use std::{path::PathBuf, process};

/// `jade test [pattern] [-v]`
pub fn run_test(pattern: Option<&str>, verbose: bool) {
    // Find test files relative to project root (or CWD).
    let root = crate::project::find_project_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let files = crate::project::find_test_files(&root, pattern);

    if files.is_empty() {
        if let Some(pat) = pattern {
            eprintln!("no test files matching '{}' found", pat);
        } else {
            eprintln!("no test files found (expected test_*.jde or *_test.jde)");
        }
        return;
    }

    println!("running {} test{}", files.len(), if files.len() == 1 { "" } else { "s" });

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for file in &files {
        let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();

        print!("  {} ... ", name);

        match run_test_file(file, verbose) {
            Ok(()) => {
                println!("ok");
                passed += 1;
            }
            Err(msg) => {
                println!("FAILED");
                failures.push((name, msg));
                failed += 1;
            }
        }
    }

    println!();
    if failures.is_empty() {
        println!("test result: ok. {} passed; 0 failed", passed);
    } else {
        println!("failures:");
        for (name, msg) in &failures {
            println!("  {} — {}", name, msg);
        }
        println!();
        println!("test result: FAILED. {} passed; {} failed", passed, failed);
        process::exit(1);
    }
}

/// Build and run a single test file. Returns `Ok(())` on a clean exit, and
/// `Err(msg)` naming the build error or the binary's failure otherwise.
fn run_test_file(path: &std::path::Path, _verbose: bool) -> Result<(), String> {
    let scratch =
        std::env::temp_dir().join(format!("jade-test-{}-{}", process::id(), unique_suffix(path)));
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("could not create '{}': {e}", scratch.display()))?;
    let bin = scratch.join("test");
    let result = (|| {
        super::build::try_compile_file(&path.to_string_lossy(), &bin, crate::build::Emit::Binary)?;
        let out = process::Command::new(&bin)
            .output()
            .map_err(|e| format!("could not run '{}': {e}", bin.display()))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(format!("{}: {}", out.status, detail.trim()))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// A short name for `path`, so two test files never share a scratch directory.
fn unique_suffix(path: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("{:x}", h.finish())
}
