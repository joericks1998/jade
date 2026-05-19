use std::{path::PathBuf, process};

include!(concat!(env!("OUT_DIR"), "/jade_rt_sources.rs"));

// ── Public entry point ────────────────────────────────────────────────────────

/// `jade rt build [--target jade-os] [--output <dir>] [--cc <cc>] [--ar <ar>]`
pub fn run_rt_build(target: &str, output: Option<&str>, cc: &str, ar: &str) {
    let (header, pthread_src, jadeos_src) = match (RT_HEADER, RT_PTHREAD, RT_JADEOS) {
        (Some(h), Some(pt), Some(jo)) => (h, pt, jo),
        _ => {
            eprintln!("error: jade runtime sources are not available in this build");
            eprintln!("       Build jadec from jade-os with JADE_RT_SRC_DIR set.");
            process::exit(1);
        }
    };

    let jadeos = target == "jade-os";

    // Resolve output directory / file path.
    let out_lib: PathBuf = match output {
        Some(p) => {
            let pb = PathBuf::from(p);
            if pb.is_dir() || p.ends_with('/') || p.ends_with('\\') {
                pb.join("libJadeRuntime.a")
            } else {
                pb
            }
        }
        None => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("libJadeRuntime.a"),
    };

    // Create a scratch directory inside the system temp dir.
    let scratch = make_scratch_dir().unwrap_or_else(|e| {
        eprintln!("error: could not create temp directory: {}", e);
        process::exit(1);
    });

    // Write the bundled C sources into the scratch dir.
    let header_path = scratch.join("jade_rt.h");
    let src_name    = if jadeos { "jade_rt_jadeos.c" } else { "jade_rt_pthread.c" };
    let src_path    = scratch.join(src_name);
    let obj_path    = scratch.join("jade_rt.o");

    std::fs::write(&header_path, header).unwrap_or_else(|e| {
        eprintln!("error: {}", e); process::exit(1);
    });
    std::fs::write(&src_path, if jadeos { jadeos_src } else { pthread_src })
        .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1); });

    // ── Compile: cc -O2 -Wall -std=c11 -fPIC -c -o jade_rt.o <src> ──────────
    let mut cflags: Vec<&str> = vec!["-O2", "-Wall", "-std=c11", "-fPIC", "-c"];
    if jadeos {
        cflags.push("-D__JADE_OS__");
    }

    let compile_status = process::Command::new(cc)
        .args(&cflags)
        .arg("-o").arg(&obj_path)
        .arg(&src_path)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("error: C compiler '{}' not found: {}", cc, e);
            eprintln!("       Install a C compiler or pass --cc <path>");
            process::exit(1);
        });

    if !compile_status.success() {
        cleanup(&scratch);
        eprintln!("error: compilation failed");
        process::exit(1);
    }

    // ── Archive: ar rcs libJadeRuntime.a jade_rt.o ───────────────────────────
    let archive_status = process::Command::new(ar)
        .args(["rcs"])
        .arg(&out_lib)
        .arg(&obj_path)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("error: archiver '{}' not found: {}", ar, e);
            eprintln!("       Install binutils or pass --ar <path>");
            process::exit(1);
        });

    cleanup(&scratch);

    if !archive_status.success() {
        eprintln!("error: ar failed to create library");
        process::exit(1);
    }

    let out_display = out_lib.display();
    let out_dir = out_lib
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    eprintln!("built: {}", out_display);
    eprintln!();
    eprintln!("  Link with:    -L{} -lJadeRuntime{}", out_dir,
        if cfg!(target_os = "linux") { " -lpthread" } else { "" });
    eprintln!("  For jade build:  JADE_RT_LIB={} jade build --target jade-os <file.jde>", out_dir);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_scratch_dir() -> Result<PathBuf, std::io::Error> {
    let dir = std::env::temp_dir().join(format!("jade-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}
