//! Compile the C runtime (`runtime_aot/`) into `libJadeRuntime.a`.
//!
//! Emitted Jade binaries link against this archive (`-lJadeRuntime`). The
//! runtime is split into a platform-agnostic core (`common.c`) plus a swappable
//! platform backend that supplies concurrency + process-exit: we build `posix.c`
//! (the host backend) here. `infer` adds the LLM inference path; the socket
//! transport beneath it is Rust, in `jade-runtime`'s `infer` module, shared with
//! the VM — `ipc/` keeps only the header declaring those entry points.
//!
//! The archive lands in `$OUT_DIR`; we surface that directory to the daemon via
//! the `JADE_RT_LIB_DIR` compile-time env so it can point the linker at it.

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

    cc::Build::new()
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
        .include(rt.join("ipc"))
        .warnings(false)
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

    println!("cargo:rerun-if-changed=src/runtime_aot");
    println!("cargo:rerun-if-changed=src/runtime/src");
}
