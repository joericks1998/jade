//! Compile the C runtime (`runtime_aot/`) into `libJadeRuntime.a`.
//!
//! Emitted Jade binaries link against this archive (`-lJadeRuntime`). The
//! runtime is split into a platform-agnostic core (`common.c`) plus a swappable
//! platform backend that supplies concurrency + process-exit: we build `posix.c`
//! (the host backend) here. `infer` adds the LLM inference path, which drives the
//! installed provider package through `native.c` — it used to sit on an `ipc/`
//! socket transport to the inference daemon, and that whole layer is gone.
//!
//! The archive lands in `$OUT_DIR`; we surface that directory to the build
//! driver via the `JADE_RT_LIB_DIR` compile-time env so it can point the linker
//! at it.
//!
//! It also builds the linker into the compiler: `src/backend/lld.cpp`, a C entry
//! point into LLD, linked against LLD's static libraries from the same LLVM 18
//! install `llvm-sys` uses. That is what lets the static backend link a program
//! in-process instead of running the system `cc`. See [`link_lld`].

use std::path::PathBuf;

fn main() {
    // Jade is Unix-only (see the matching `compile_error!` in src/lib.rs). This
    // check has to live here as well, and run first: the C runtime below builds
    // `posix.c`, so a Windows target would otherwise fail inside `cc` with a
    // compiler error about missing POSIX headers instead of saying why.
    //
    // `cfg!(unix)` would describe the *host*, which is wrong when cross-compiling
    // — CARGO_CFG_TARGET_FAMILY is the target's, which is what matters.
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if !family.split(',').any(|f| f == "unix") {
        panic!(
            "Jade supports Unix-like platforms only (macOS and Linux). \
             The target you asked for is not Unix; on Windows, build inside WSL2."
        );
    }

    let rt = PathBuf::from("src/runtime_aot");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");

    // Archived with `llvm-ar`; see `new_build`.
    let mut build = new_build();

    build
        .file(rt.join("common.c"))
        .file(rt.join("native.c"))
        .file(rt.join("posix.c"))
        .file(rt.join("infer/infer.c"))
        // Stdlib leaf modules, one folder per std:: module. Module .c files
        // include their headers as "<mod>/<mod>.h" (resolved via -I rt), so no
        // per-folder include is added — this keeps libc's <time.h> from being
        // shadowed by time/time.h.
        .include(&rt)
        .include(rt.join("infer"))
        .warnings(false)
        // The one warning that is not a style opinion here. Most of this C
        // calls into the Rust runtime across the `jrt_*` ABI, and a call with
        // no prototype in scope is assumed to return `int` — which silently
        // truncates a returned pointer to 32 bits and segfaults at the first
        // use of the result, a long way from the missing declaration. A
        // wrapper added for `str.repeat` did exactly that. `.warnings(false)`
        // above keeps the tree's existing style warnings quiet; this one is an
        // error.
        .flag("-Werror=implicit-function-declaration")
        .compile("JadeRuntime"); // → $OUT_DIR/libJadeRuntime.a

    println!("cargo:rustc-env=JADE_RT_LIB_DIR={out_dir}");

    // The shared Rust runtime (`jade-runtime` workspace member) is built as a
    // staticlib (`libjade_runtime.a`) that emitted binaries also link against —
    // it now supplies runtime symbols moved out of common.c. Cargo uplifts it to
    // the target *profile* directory, three levels up from OUT_DIR
    // (`target/<profile>/build/jade-<hash>/out`).
    let profile_dir = PathBuf::from(&out_dir)
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should sit under target/<profile>/build/<pkg>/out")
        .to_path_buf();
    println!("cargo:rustc-env=JADE_RUST_RT_DIR={}", profile_dir.display());

    // Copy the C archive up beside the Rust one, so both runtime archives a
    // `jade build` needs sit in a single predictable directory rather than one
    // being in a hash-named OUT_DIR. Two things depend on this:
    //
    //  * release packaging can name the files instead of `find`-ing them;
    //  * an *installed* jade resolves both from one directory next to itself
    //    (see `aot::runtime_archive_dirs`).
    //
    // `libJadeRuntime.a` is linked into every binary `jade build` emits, so it
    // is a shipped artifact of the toolchain and belongs somewhere stable —
    // the same reasoning that made jade-runtime a workspace member.
    let c_archive = PathBuf::from(&out_dir).join("libJadeRuntime.a");
    if c_archive.exists() {
        // Best-effort: a failure here only means the dev-tree fallback to
        // OUT_DIR (still emitted above) is what gets used.
        let _ = std::fs::copy(&c_archive, profile_dir.join("libJadeRuntime.a"));
    }

    link_lld();

    println!("cargo:rerun-if-changed=src/runtime_aot");
    println!("cargo:rerun-if-changed=src/runtime/src");
    println!("cargo:rerun-if-changed=src/backend/lld.cpp");
}

/// Purpose: This function makes a `cc` build that archives with `llvm-ar`.
///
/// Apple's `ar` rejects `-D` (deterministic mode). `cc` does not know that in
/// advance: it *probes*, running `ar cqD` and falling back to
/// `ZERO_AR_DATE=1 ar cq` when that fails. The archive it ends up with is
/// correct and deterministic either way, but the failed probe's usage text
/// reaches the build log as a dozen `cargo:warning=` lines on every macOS
/// build, which buries anything worth reading. `llvm-ar` takes the flag, and it
/// is not a new dependency: LLVM 18 has to be installed for this crate to
/// compile at all. If it does not resolve, `cc` is left to probe as before.
fn new_build() -> cc::Build {
    let mut build = cc::Build::new();
    if std::env::var_os("AR").is_none()
        && let Some(prefix) = std::env::var_os("LLVM_SYS_180_PREFIX")
    {
        let llvm_ar = PathBuf::from(prefix).join("bin/llvm-ar");
        if llvm_ar.is_file() {
            build.archiver(&llvm_ar);
        }
    }
    build
}

/// Purpose: This function compiles the LLD bridge and links LLD's libraries.
///
/// The libraries come from the LLVM 18 install `LLVM_SYS_181_PREFIX` or
/// `LLVM_SYS_180_PREFIX` names, as `llvm-sys` finds LLVM itself. LLD needs the
/// LLVM libraries too, and `llvm-sys` already links every one of them. The
/// order matters to a GNU linker: the bridge, then LLD's drivers, then LLD's
/// common code, all ahead of LLVM, which `llvm-sys` emits after this crate.
///
/// On Debian and Ubuntu the static libraries and headers are in
/// `liblld-18-dev`; Homebrew's `llvm@18` carries them.
fn link_lld() {
    let prefix = ["LLVM_SYS_181_PREFIX", "LLVM_SYS_180_PREFIX"]
        .iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
        .expect("set LLVM_SYS_180_PREFIX to an LLVM 18 install; see CLAUDE.md");
    let config = prefix.join("bin/llvm-config");
    let ask = |arg: &str| -> String {
        let out = std::process::Command::new(&config)
            .arg(arg)
            .output()
            .unwrap_or_else(|e| panic!("could not run {}: {e}", config.display()));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let libdir = PathBuf::from(ask("--libdir"));
    let includedir = PathBuf::from(ask("--includedir"));
    for lib in ["lldMachO", "lldELF", "lldCommon"] {
        if !libdir.join(format!("lib{lib}.a")).is_file() {
            panic!(
                "LLD's static library lib{lib}.a is not in {}. `jade build` links programs \
                 with LLD; install it with LLVM 18 (Debian and Ubuntu: liblld-18-dev).",
                libdir.display()
            );
        }
    }

    let mut build = new_build();
    build.cpp(true).file("src/backend/lld.cpp").include(&includedir).warnings(false);
    // LLVM's own flags, so the bridge agrees with how LLVM was built about the
    // language level, RTTI and the standard library.
    for flag in ask("--cxxflags").split_whitespace() {
        if !flag.starts_with("-I") {
            build.flag(flag);
        }
    }
    build.compile("jadelld");

    println!("cargo:rustc-link-search=native={}", libdir.display());
    for lib in ["lldMachO", "lldELF", "lldCommon"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
