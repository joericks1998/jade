//! Compile the C runtime (`runtime_lib/`) into `libJadeRuntime.a`.
//!
//! Emitted Jade binaries link against this archive (`-lJadeRuntime`). The
//! runtime is split into a platform-agnostic core (`common.c`) plus a swappable
//! platform backend that supplies concurrency + process-exit: we build `posix.c`
//! (the host backend) here. `infer` + `ipc` add the LLM inference path (talks to
//! the inference daemon over a Unix socket at runtime).
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

    let rt = PathBuf::from("src/codegen/runtime_lib");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");

    // Generate the canonical tool-call GBNF as a C string constant straight from
    // the checked-in grammar (the same file jadelang compiles in via include_str!),
    // so `llm.tool_grammar()` returns byte-identical text under the VM and AOT.
    // build.rs runs in the crate root (jadelang), so the grammar is under grammars/.
    let gbnf_src = PathBuf::from("grammars/tool_call.gbnf");
    let gbnf = std::fs::read(&gbnf_src)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", gbnf_src.display()));
    let header = PathBuf::from(&out_dir).join("tool_call_gbnf.h");
    std::fs::write(&header, gen_gbnf_header(&gbnf)).expect("write tool_call_gbnf.h");

    cc::Build::new()
        .file(rt.join("common.c"))
        .file(rt.join("native.c"))
        .file(rt.join("posix.c"))
        .file(rt.join("infer/infer.c"))
        .file(rt.join("infer/tool_call.c"))
        .file(rt.join("ipc/ipc.c"))
        // Stdlib leaf modules, one folder per std:: module. Module .c files
        // include their headers as "<mod>/<mod>.h" (resolved via -I rt), so no
        // per-folder include is added — this keeps libc's <time.h> from being
        // shadowed by time/time.h.
        .include(&rt)
        .include(rt.join("infer"))
        .include(rt.join("ipc"))
        .include(&out_dir) // for the generated tool_call_gbnf.h
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
    //    (see `codegen::runtime_lib_dirs`).
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

    println!("cargo:rerun-if-changed=src/codegen/runtime_lib");
    println!("cargo:rerun-if-changed=src/runtime/src");
    println!("cargo:rerun-if-changed={}", gbnf_src.display());
}

/// Emit `static const char JRT_TOOL_CALL_GBNF[] = "...";` with `bytes` escaped
/// as a C string literal — exact bytes preserved (NUL-terminated by C).
fn gen_gbnf_header(bytes: &[u8]) -> String {
    let mut lit = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        match b {
            b'"' => lit.push_str("\\\""),
            b'\\' => lit.push_str("\\\\"),
            b'\n' => lit.push_str("\\n"),
            b'\r' => lit.push_str("\\r"),
            b'\t' => lit.push_str("\\t"),
            0x20..=0x7e => lit.push(b as char),
            _ => lit.push_str(&format!("\\x{b:02x}")),
        }
    }
    format!(
        "/* Generated by build.rs from jadelang/grammars/tool_call.gbnf — do not edit. */\n\
         static const char JRT_TOOL_CALL_GBNF[] = \"{lit}\";\n"
    )
}
