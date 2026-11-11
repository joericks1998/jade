//! Builds the linker into the compiler.
//!
//! `src/backend/lld.cpp` is a C entry point into LLD, LLVM's own linker, and
//! this compiles it and links LLD's static libraries from the same LLVM 18
//! install `llvm-sys` uses. That is what lets `jade build` link a program
//! in-process instead of running the system `cc`. See [`link_lld`].
//!
//! Until phase 6 of `design/v2-static-values.md` this script also compiled the
//! C runtime v1's binaries linked against; that runtime is gone.

use std::path::PathBuf;

fn main() {
    // Jade is Unix-only (see the matching `compile_error!` in src/lib.rs). The
    // check runs here too, first, so a Windows build says why it stops instead
    // of failing somewhere in LLD's headers.
    //
    // `cfg!(unix)` would describe the *host*, which is wrong when cross-compiling
    // — CARGO_CFG_TARGET_FAMILY is the target's, which is what matters.
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if !family.split(',').any(|f| f == "unix") {
        panic!(
            "Jade supports Unix-like platforms only (macOS and Linux); on Windows, build \
             inside WSL2."
        );
    }
    link_lld();
    println!("cargo:rerun-if-changed=src/backend/lld.cpp");
    println!("cargo:rerun-if-changed=build.rs");
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
