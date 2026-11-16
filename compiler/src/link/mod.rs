//! Linking object files into a program, with LLD inside the compiler.
//!
//! Codegen writes the program's object file through LLVM, beside the cached
//! objects of the library instances it uses (`codegen/libcache/`). The driver
//! in `build/` hands the list here, and they are linked in-process through
//! `lld.cpp`, the C entry point into LLVM's own linker. No `cc`, no Xcode and
//! no development packages are needed: a program links the C library and
//! nothing else, so all the linker needs is that library.
//!
//! *macOS.* The C library is `libSystem.B.dylib`, which lives in the dyld
//! shared cache rather than on disk, so a linker normally reads a text stub of
//! it, `libSystem.tbd`, from Apple's SDK. This file writes that stub itself,
//! listing exactly the symbols the objects import, which is what one leaves
//! undefined and none defines: what `libSystem` exports is a fact of the
//! operating system, and the objects say which of it the program uses. LLD
//! signs the binary, as Apple Silicon requires.
//!
//! *Linux.* The C library is glibc's `libc.so.6`, which every glibc system has
//! as a runtime file, or on a system without glibc, such as Alpine, musl's
//! loader, which is its C library too. The start-up code that development
//! packages normally supply as `Scrt1.o` is three instructions of setup and a
//! call to `__libc_start_main`; codegen writes it into the program as
//! `_start` (`codegen/start.rs`), so linking needs nothing a minimal system
//! lacks.
//!
//! *A program with a Rust package* is linked with the project's staticlib too
//! (`crate::rustpkg`), and with the system libraries rustc's `native-static-libs`
//! note names for it. On Linux each is found as a shared object beside the C
//! library. On macOS a staticlib imports from whatever its `-sys` crates link,
//! and which archive member imports what is the linker's decision, so such a
//! program links against Apple's SDK stubs instead of a stub written here. Cargo
//! needs that SDK already: rustc links every build script and proc macro with
//! the system's `cc`.

use std::ffi::{CString, c_char, c_int};
use std::path::{Path, PathBuf};

use crate::rustpkg::Link;

unsafe extern "C" {
    /// Runs LLD on a command line; see `lld.cpp`.
    fn jade_lld_link(
        argc: c_int,
        argv: *const *const c_char,
        msg: *mut c_char,
        msg_len: usize,
    ) -> c_int;
}

/// Purpose: This function links object files into a program.
///
/// Inputs:
/// - `objs`: the object files: the program's first, then the cached library
///   instances it uses, whose shared state the program's defines.
/// - `out`: the program to write.
/// - `rust`: the Rust packages' staticlib and its system libraries, when the
///   program calls one.
///
/// Outputs: `Ok(())`, or what the linker said.
pub fn link(objs: &[PathBuf], out: &Path, rust: Option<&Link>) -> Result<(), String> {
    let first = objs.first().ok_or("internal compiler error: nothing to link")?;
    if cfg!(target_os = "macos") {
        link_macho(first, objs, out, rust)
    } else {
        link_elf(objs, out, rust)
    }
}

/// Purpose: This function links on macOS: against a `libSystem` stub written
/// for these objects, or with a Rust staticlib, against the SDK.
///
/// Inputs:
/// - `first`: the program's object, beside which the stubs are written.
/// - `objs`: every object, the first included.
/// - `out`, `rust`: as for [`link`].
fn link_macho(
    first: &Path,
    objs: &[PathBuf],
    out: &Path,
    rust: Option<&Link>,
) -> Result<(), String> {
    // What the objects import from the system is what one of them leaves
    // undefined and none defines: a library object's call into the program's
    // error slot is not an import.
    let mut undefined = std::collections::BTreeSet::new();
    let mut defined = std::collections::BTreeSet::new();
    for obj in objs {
        let bytes =
            std::fs::read(obj).map_err(|e| format!("cannot read {}: {e}", obj.display()))?;
        let syms = macho_symbols(&bytes)?;
        undefined.extend(syms.undefined);
        defined.extend(syms.defined);
    }
    let imports: Vec<String> = undefined.difference(&defined).cloned().collect();
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    let version = macos_version();
    let mut args = vec![
        "ld64.lld".to_string(),
        "-arch".into(),
        arch.into(),
        "-platform_version".into(),
        "macos".into(),
        version.clone(),
        version,
        "-o".into(),
        out.display().to_string(),
    ];
    args.extend(objs.iter().map(|o| o.display().to_string()));
    if let Some(r) = rust {
        args.push(r.staticlib.display().to_string());
        args.extend(["-syslibroot".to_string(), macos_sdk()?, "-lSystem".to_string()]);
        for (install, _) in by_library(&imports).iter().skip(1) {
            if install.ends_with("libobjc.A.dylib") {
                args.push("-lobjc".into());
            } else if install.contains("Metal.framework") {
                args.extend(["-framework".to_string(), "Metal".to_string()]);
            }
        }
        args.extend(r.native_libs.iter().cloned());
        return run_lld(&args);
    }
    // One stub per library the object reaches into: the C library always, and
    // for a program that runs a model, the Objective-C runtime and Metal.
    let mut stubs = Vec::new();
    for (i, (install, symbols)) in by_library(&imports).iter().enumerate() {
        let stub = first.with_extension(format!("{i}.tbd"));
        std::fs::write(&stub, stub_tbd(arch, install, symbols))
            .map_err(|e| format!("cannot write {}: {e}", stub.display()))?;
        args.push(stub.display().to_string());
        stubs.push(stub);
    }
    let r = run_lld(&args);
    for s in stubs {
        std::fs::remove_file(&s).ok();
    }
    r
}

/// Purpose: This function sorts imported symbols by the system library that
/// exports them.
///
/// Inputs:
/// - `symbols`: the symbol names, with Mach-O's leading underscore.
///
/// Outputs: each library's install name and its symbols; `libSystem` first,
/// and always present.
fn by_library(symbols: &[String]) -> Vec<(&'static str, Vec<String>)> {
    const OBJC: &str = "/usr/lib/libobjc.A.dylib";
    const METAL: &str = "/System/Library/Frameworks/Metal.framework/Versions/A/Metal";
    let mut system = Vec::new();
    let mut objc = Vec::new();
    let mut metal = Vec::new();
    for s in symbols {
        if s.starts_with("_objc_") || s.starts_with("_sel_") {
            objc.push(s.clone());
        } else if s.starts_with("_MTL") {
            metal.push(s.clone());
        } else {
            system.push(s.clone());
        }
    }
    let mut out = vec![("/usr/lib/libSystem.B.dylib", system)];
    if !objc.is_empty() {
        out.push((OBJC, objc));
    }
    if !metal.is_empty() {
        out.push((METAL, metal));
    }
    out
}

/// Purpose: This function writes a stub for one system library exporting the
/// given symbols, for one architecture.
///
/// Inputs:
/// - `arch`: `arm64` or `x86_64`.
/// - `install`: the library's install name, where the loader finds it.
/// - `symbols`: the symbol names, with Mach-O's leading underscore.
///
/// Outputs: the stub, in Apple's text format, version 4.
fn stub_tbd(arch: &str, install: &str, symbols: &[String]) -> String {
    let target = format!("{arch}-macos");
    let system = install.ends_with("libSystem.B.dylib");
    let mut names: Vec<String> = symbols.iter().map(|s| format!("'{s}'")).collect();
    // Lazy binding asks libSystem for the stub binder when the loader does not
    // use chained fixups; listing it costs nothing when it is not needed.
    if system && !symbols.iter().any(|s| s == "dyld_stub_binder") {
        names.push("'dyld_stub_binder'".into());
    }
    let version = if system { 1351 } else { 1 };
    format!(
        "--- !tapi-tbd\n\
         tbd-version:     4\n\
         targets:         [ {target} ]\n\
         install-name:    '{install}'\n\
         current-version: {version}\n\
         exports:\n  \
         - targets:         [ {target} ]\n    \
         symbols:         [ {} ]\n\
         ...\n",
        names.join(", ")
    )
}

/// Purpose: This struct holds a Mach-O object's external symbols.
#[derive(Debug, Default)]
struct MachoSymbols {
    /// What it leaves undefined: what it imports.
    undefined: Vec<String>,
    /// What it defines, hidden symbols included.
    defined: Vec<String>,
}

/// Purpose: This function lists the external symbols a 64-bit Mach-O object
/// leaves undefined and the ones it defines.
///
/// Inputs:
/// - `b`: the object file's bytes.
///
/// Outputs: both lists, sorted, or a message when the file is not one.
fn macho_symbols(b: &[u8]) -> Result<MachoSymbols, String> {
    let bad = || "the object file is not a 64-bit Mach-O file".to_string();
    let u32_at = |o: usize| -> Option<u32> {
        b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    };
    if u32_at(0) != Some(0xfeed_facf) {
        return Err(bad());
    }
    let ncmds = u32_at(16).ok_or_else(bad)? as usize;
    // The 64-bit header is 32 bytes; load commands follow it.
    let mut at = 32usize;
    for _ in 0..ncmds {
        let cmd = u32_at(at).ok_or_else(bad)?;
        let size = u32_at(at + 4).ok_or_else(bad)? as usize;
        // LC_SYMTAB: where the symbol table and its strings are.
        if cmd == 0x2 {
            let symoff = u32_at(at + 8).ok_or_else(bad)? as usize;
            let nsyms = u32_at(at + 12).ok_or_else(bad)? as usize;
            let stroff = u32_at(at + 16).ok_or_else(bad)? as usize;
            let mut out = MachoSymbols::default();
            for i in 0..nsyms {
                // `nlist_64 { u32 n_strx; u8 n_type; u8 n_sect; u16 n_desc; u64 n_value }`.
                let e = symoff + 16 * i;
                let strx = u32_at(e).ok_or_else(bad)? as usize;
                let ty = *b.get(e + 4).ok_or_else(bad)?;
                let value = b.get(e + 8..e + 16).ok_or_else(bad)?;
                // N_STAB, then N_TYPE: N_UNDF is 0 and N_SECT is 0xe; N_EXT.
                let debug = ty & 0xe0 != 0;
                let undefined = ty & 0x0e == 0;
                let in_section = ty & 0x0e == 0x0e;
                let external = ty & 0x01 != 0;
                // A common symbol is undefined with a size; LLVM emits none.
                let common = value.iter().any(|v| *v != 0);
                if debug || !external || (undefined && common) || !(undefined || in_section) {
                    continue;
                }
                let name = b.get(stroff + strx..).ok_or_else(bad)?;
                let end = name.iter().position(|c| *c == 0).ok_or_else(bad)?;
                let name = String::from_utf8_lossy(&name[..end]).into_owned();
                if undefined { out.undefined.push(name) } else { out.defined.push(name) }
            }
            for list in [&mut out.undefined, &mut out.defined] {
                list.sort();
                list.dedup();
            }
            return Ok(out);
        }
        at += size;
    }
    Ok(MachoSymbols::default())
}

/// Purpose: This function finds Apple's SDK, whose library stubs a program with a
/// Rust package links against.
///
/// Outputs: the SDK's path, or a message naming how to get one.
fn macos_sdk() -> Result<String, String> {
    let asked = std::process::Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Path::new(s).is_dir());
    let fallback = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
    asked.or_else(|| Path::new(fallback).is_dir().then(|| fallback.to_string())).ok_or_else(|| {
        "linking a program that uses a Rust package needs Apple's SDK, which cargo's own builds \
         need too, and none was found; install the Command Line Tools with \
         `xcode-select --install`"
            .to_string()
    })
}

/// Purpose: This function returns the running macOS version as
/// `major.minor`, the oldest a built program is marked to run on.
fn macos_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().splitn(3, '.').take(2).collect::<Vec<_>>().join("."))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "11.0".to_string())
}

/// Purpose: This function links on Linux, against the system's C library:
/// glibc, or musl where glibc is not installed. The objects resolve each
/// other's hidden symbols, the library objects' references to the program's
/// shared state among them, before any shared library is searched.
fn link_elf(objs: &[PathBuf], out: &Path, rust: Option<&Link>) -> Result<(), String> {
    let libc = c_library()?;
    let mut args: Vec<String> = vec![
        "ld.lld".into(),
        "-pie".into(),
        "--eh-frame-hdr".into(),
        "--hash-style=gnu".into(),
        "-z".into(),
        "relro".into(),
        "-z".into(),
        "now".into(),
        "-z".into(),
        "noexecstack".into(),
        "--dynamic-linker".into(),
        libc.loader.clone(),
        "-o".into(),
        out.display().to_string(),
    ];
    args.extend(objs.iter().map(|o| o.display().to_string()));
    args.extend([
        // libc's own dependencies, the loader among them, are the loader's to
        // find at run time.
        "--allow-shlib-undefined".to_string(),
        "--as-needed".to_string(),
    ]);
    if let Some(r) = rust {
        args.push(r.staticlib.display().to_string());
        // Rust's standard library on glibc always needs these, its unwinder in
        // libgcc_s first; they stand in when cargo did not hand back rustc's note.
        const GLIBC_RUST_LIBS: [&str; 7] =
            ["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc"];
        let libs: Vec<String> = if r.native_libs.is_empty() && libc.loader.contains("ld-linux") {
            GLIBC_RUST_LIBS.iter().map(|s| s.to_string()).collect()
        } else {
            r.native_libs.clone()
        };
        args.extend(rust_system_libs(&libs, Path::new("/"), &libc)?);
    }
    args.extend(libc.libs);
    run_lld(&args)
}

/// Purpose: This function finds the shared objects a Rust staticlib's
/// `native-static-libs` note names, such as `libgcc_s` for Rust's unwinder.
///
/// Inputs:
/// - `libs`: the note's linker arguments, such as `-lgcc_s`.
/// - `root`: the file system's root, `/` outside tests.
/// - `libc`: the C library found, whose directory is searched first.
///
/// Outputs: each library's path. One that glibc folds into `libc.so.6`, such as
/// `-lpthread` on a new glibc, is skipped when it is not there; any other that
/// is missing stops the link, naming it.
fn rust_system_libs(libs: &[String], root: &Path, libc: &CLibrary) -> Result<Vec<String>, String> {
    const IN_LIBC: [&str; 6] = ["c", "m", "pthread", "dl", "rt", "util"];
    let mut dirs: Vec<std::path::PathBuf> = libc
        .libs
        .iter()
        .filter_map(|l| Path::new(l).parent().map(Path::to_path_buf))
        .filter(|d| d.is_absolute())
        .collect();
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    for d in [
        format!("lib/{arch}-linux-gnu"),
        format!("usr/lib/{arch}-linux-gnu"),
        "lib64".to_string(),
        "usr/lib64".to_string(),
        "lib".to_string(),
        "usr/lib".to_string(),
    ] {
        dirs.push(root.join(d));
    }
    let mut out = Vec::new();
    for name in libs.iter().filter_map(|a| a.strip_prefix("-l")) {
        let plain = format!("lib{name}.so");
        let found = dirs.iter().find_map(|d| {
            if d.join(&plain).is_file() {
                return Some(d.join(&plain));
            }
            let mut versioned: Vec<_> = std::fs::read_dir(d)
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&format!("{plain}.")))
                })
                .collect();
            versioned.sort();
            versioned.into_iter().next()
        });
        match found {
            Some(p) => out.push(p.display().to_string()),
            None if IN_LIBC.contains(&name) => {}
            None => {
                return Err(format!(
                    "the program's Rust packages need the system library lib{name}, which is in \
                     none of {}",
                    dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    Ok(out)
}

/// Purpose: This struct holds what linking against a C library takes.
#[derive(Debug, PartialEq)]
struct CLibrary {
    /// The dynamic loader a program names, which the kernel runs first.
    loader: String,
    /// The shared libraries to link, as linker arguments.
    libs: Vec<String>,
}

/// Purpose: This function finds the system's C library.
///
/// Outputs: glibc when `libc.so.6` is installed, else musl when its loader is,
/// else a message naming both.
fn c_library() -> Result<CLibrary, String> {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    c_library_in(Path::new("/"), arch)
}

/// Purpose: This function finds a C library under a root directory, for one
/// architecture.
///
/// Inputs:
/// - `root`: the file system's root, `/` outside tests.
/// - `arch`: `x86_64` or `aarch64`.
///
/// Outputs: the C library, or a message naming where it looked.
fn c_library_in(root: &Path, arch: &str) -> Result<CLibrary, String> {
    let at = |p: &str| root.join(p.trim_start_matches('/'));
    let glibc_loader = if arch == "aarch64" {
        "/lib/ld-linux-aarch64.so.1"
    } else {
        "/lib64/ld-linux-x86-64.so.2"
    };
    let multiarch = format!("{arch}-linux-gnu");
    let dirs = [
        format!("/lib/{multiarch}"),
        format!("/usr/lib/{multiarch}"),
        "/lib64".to_string(),
        "/usr/lib64".to_string(),
        "/lib".to_string(),
        "/usr/lib".to_string(),
    ];
    if let Some(dir) = dirs.iter().find(|d| at(d).join("libc.so.6").is_file()) {
        // Older glibc keeps threads, `dlopen` and the math library apart from
        // libc; newer keeps only math apart. `--as-needed` drops what is unused.
        let libs = ["libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2"]
            .iter()
            .map(|lib| at(dir).join(lib))
            .filter(|p| p.is_file())
            .map(|p| p.display().to_string())
            .collect();
        return Ok(CLibrary { loader: glibc_loader.to_string(), libs });
    }

    // musl's loader is its C library too, threads, `dlopen` and math included.
    // Programs name it by the soname Alpine gives it, which the loader answers
    // to as itself.
    let musl_loader = format!("/lib/ld-musl-{arch}.so.1");
    if at(&musl_loader).is_file() {
        let soname = format!("libc.musl-{arch}.so.1");
        let lib = if at("/lib").join(&soname).is_file() {
            vec!["-L".to_string(), at("/lib").display().to_string(), format!("-l:{soname}")]
        } else {
            vec![at(&musl_loader).display().to_string()]
        };
        return Ok(CLibrary { loader: musl_loader, libs: lib });
    }

    Err(format!(
        "linking needs a C library: glibc's libc.so.6, which is in none of {}, or musl's \
         loader {musl_loader}, which is not there either",
        dirs.join(", ")
    ))
}

/// Purpose: This function runs LLD.
///
/// Inputs:
/// - `args`: the command line, its first word naming the flavor.
///
/// Outputs: `Ok(())`, or what the linker printed.
fn run_lld(args: &[String]) -> Result<(), String> {
    let owned: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()))
        .collect::<Result<_, _>>()
        .map_err(|_| "a path passed to the linker holds a NUL byte".to_string())?;
    let argv: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
    let mut msg = vec![0 as c_char; 16 * 1024];
    // SAFETY: `argv` points at NUL-terminated strings `owned` keeps alive for
    // the call, and `msg` is a writable buffer of the length passed, which the
    // bridge writes at most that many bytes of, NUL included.
    let code =
        unsafe { jade_lld_link(argv.len() as c_int, argv.as_ptr(), msg.as_mut_ptr(), msg.len()) };
    if code == 0 {
        return Ok(());
    }
    let bytes: Vec<u8> = msg.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    Err(format!("linking failed\n{}", String::from_utf8_lossy(&bytes).trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_libsystem_stub_lists_what_the_object_imports() {
        let tbd =
            stub_tbd("arm64", "/usr/lib/libSystem.B.dylib", &["_malloc".into(), "___error".into()]);
        assert!(tbd.starts_with("--- !tapi-tbd\n"));
        assert!(tbd.contains("install-name:    '/usr/lib/libSystem.B.dylib'"));
        assert!(tbd.contains("[ arm64-macos ]"));
        assert!(tbd.contains("'_malloc', '___error', 'dyld_stub_binder'"));
    }

    /// Purpose: This function makes an empty file and the directories above it.
    fn touch(root: &Path, p: &str) {
        let f = root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, b"").unwrap();
    }

    #[test]
    fn glibc_is_found_in_its_multiarch_directory() {
        let root = std::env::temp_dir().join(format!("jade_link_glibc_{}", std::process::id()));
        touch(&root, "lib/x86_64-linux-gnu/libc.so.6");
        touch(&root, "lib/x86_64-linux-gnu/libm.so.6");
        let c = c_library_in(&root, "x86_64").unwrap();
        assert_eq!(c.loader, "/lib64/ld-linux-x86-64.so.2");
        assert_eq!(c.libs.len(), 2);
        // With musl beside it, glibc still wins: it is what the system's own
        // programs use.
        touch(&root, "lib/ld-musl-x86_64.so.1");
        assert_eq!(c_library_in(&root, "x86_64").unwrap().loader, "/lib64/ld-linux-x86-64.so.2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn musl_is_found_where_there_is_no_glibc() {
        let root = std::env::temp_dir().join(format!("jade_link_musl_{}", std::process::id()));
        touch(&root, "lib/ld-musl-aarch64.so.1");
        let c = c_library_in(&root, "aarch64").unwrap();
        assert_eq!(c.loader, "/lib/ld-musl-aarch64.so.1");
        assert_eq!(c.libs, vec![root.join("lib/ld-musl-aarch64.so.1").display().to_string()]);
        // Alpine's soname, when it is there, is what the program records.
        touch(&root, "lib/libc.musl-aarch64.so.1");
        let c = c_library_in(&root, "aarch64").unwrap();
        assert_eq!(c.libs.last().unwrap(), "-l:libc.musl-aarch64.so.1");
        let _ = std::fs::remove_dir_all(&root);
        assert!(c_library_in(&root, "aarch64").is_err());
    }

    #[test]
    fn imports_are_sorted_by_the_library_that_exports_them() {
        let syms: Vec<String> =
            ["_malloc", "_objc_msgSend", "_sel_registerName", "_MTLCreateSystemDefaultDevice"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let libs = by_library(&syms);
        assert_eq!(libs[0].1, vec!["_malloc".to_string()]);
        assert!(libs[1].0.ends_with("libobjc.A.dylib"));
        assert_eq!(libs[1].1.len(), 2);
        assert!(libs[2].0.ends_with("Metal"));
        // Only libSystem's stub lists the stub binder.
        assert!(!stub_tbd("arm64", libs[2].0, &libs[2].1).contains("dyld_stub_binder"));
    }

    #[test]
    fn a_rust_staticlibs_system_libraries_are_found_beside_the_c_library() {
        let root = std::env::temp_dir().join(format!("jade_link_rustlibs_{}", std::process::id()));
        touch(&root, "lib/x86_64-linux-gnu/libc.so.6");
        touch(&root, "lib/x86_64-linux-gnu/libgcc_s.so.1");
        let libc = c_library_in(&root, "x86_64").unwrap();
        let libs: Vec<String> =
            ["-lgcc_s", "-lutil", "-lpthread", "-lc"].iter().map(|s| s.to_string()).collect();
        let found = rust_system_libs(&libs, &root, &libc).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].ends_with("libgcc_s.so.1"));
        assert!(found[1].ends_with("libc.so.6"));
        let missing = rust_system_libs(&["-lsqlite3".to_string()], &root, &libc);
        assert!(missing.unwrap_err().contains("libsqlite3"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_is_not_mach_o_is_refused() {
        assert!(macho_symbols(b"\x7fELF").is_err());
    }
}
