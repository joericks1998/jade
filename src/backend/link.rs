//! Linking an object file into a program, with LLD inside the compiler.
//!
//! The static backend writes an object file through LLVM and links it here,
//! in-process, through `lld.cpp`, the C entry point into LLVM's own linker. No
//! `cc`, no Xcode and no development packages are needed: a program links the
//! C library and nothing else, so all the linker needs is that library.
//!
//! *macOS.* The C library is `libSystem.B.dylib`, which lives in the dyld
//! shared cache rather than on disk, so a linker normally reads a text stub of
//! it, `libSystem.tbd`, from Apple's SDK. This file writes that stub itself,
//! listing exactly the symbols the object imports: what `libSystem` exports is
//! a fact of the operating system, and the object says which of it the program
//! uses. LLD signs the binary, as Apple Silicon requires.
//!
//! *Linux.* The C library is glibc's `libc.so.6`, which every glibc system has
//! as a runtime file, or on a system without glibc, such as Alpine, musl's
//! loader, which is its C library too. The start-up code that development
//! packages normally supply as `Scrt1.o` is three instructions of setup and a
//! call to `__libc_start_main`; the backend emits it as `_start` (see
//! `START_X86_64`), so linking needs nothing a minimal system lacks. musl's
//! `__libc_start_main` reads only `main`, `argc` and `argv`, the first three of
//! the arguments glibc's takes, so one `_start` serves both.

use std::ffi::{CString, c_char, c_int};
use std::path::Path;

unsafe extern "C" {
    /// Runs LLD on a command line; see `lld.cpp`.
    fn jade_lld_link(
        argc: c_int,
        argv: *const *const c_char,
        msg: *mut c_char,
        msg_len: usize,
    ) -> c_int;
}

/// The program `_start` a glibc executable begins at on x86-64, as glibc's own
/// `sysdeps/x86_64/start.S` writes it for a position-independent executable:
/// clear the frame pointer, take `argc` and `argv` off the stack the kernel
/// built, align the stack, and hand `main` to `__libc_start_main`, which never
/// returns. `_IO_stdin_used` tells glibc the program uses the current stdio.
pub const START_X86_64: &str = "\
    .text
    .globl _start
    .type _start,@function
_start:
    xorl %ebp, %ebp
    movq %rdx, %r9
    popq %rsi
    movq %rsp, %rdx
    andq $-16, %rsp
    pushq %rax
    pushq %rsp
    xorl %r8d, %r8d
    xorl %ecx, %ecx
    movq main@GOTPCREL(%rip), %rdi
    call *__libc_start_main@GOTPCREL(%rip)
    hlt
    .size _start, . - _start
    .section .rodata
    .globl _IO_stdin_used
    .p2align 2
_IO_stdin_used:
    .long 0x20001
    .text
";

/// The same for AArch64, from glibc's `sysdeps/aarch64/start.S`.
pub const START_AARCH64: &str = "\
    .text
    .globl _start
    .type _start,%function
_start:
    mov x29, #0
    mov x30, #0
    mov x5, x0
    ldr x1, [sp]
    add x2, sp, #8
    mov x6, sp
    mov x3, #0
    mov x4, #0
    adrp x0, :got:main
    ldr x0, [x0, #:got_lo12:main]
    bl __libc_start_main
    bl abort
    .size _start, . - _start
    .section .rodata
    .globl _IO_stdin_used
    .p2align 2
_IO_stdin_used:
    .word 0x20001
    .text
";

/// Purpose: This function returns the start-up code this target's programs
/// need in their module, if any.
///
/// Outputs: the assembly for Linux, nothing for macOS, whose loader calls
/// `main` itself.
pub fn start_code() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        None
    } else if cfg!(target_arch = "aarch64") {
        Some(START_AARCH64)
    } else {
        Some(START_X86_64)
    }
}

/// Purpose: This function links an object file into a program.
///
/// Inputs:
/// - `obj`: the object file.
/// - `out`: the program to write.
///
/// Outputs: `Ok(())`, or what the linker said.
pub fn link(obj: &Path, out: &Path) -> Result<(), String> {
    if cfg!(target_os = "macos") { link_macho(obj, out) } else { link_elf(obj, out) }
}

/// Purpose: This function links on macOS, against a `libSystem` stub written
/// for this object.
fn link_macho(obj: &Path, out: &Path) -> Result<(), String> {
    let bytes = std::fs::read(obj).map_err(|e| format!("cannot read {}: {e}", obj.display()))?;
    let imports = macho_imports(&bytes)?;
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
        obj.display().to_string(),
    ];
    // One stub per library the object reaches into: the C library always, and
    // for a program that runs a model, the Objective-C runtime and Metal.
    let mut stubs = Vec::new();
    for (i, (install, symbols)) in by_library(&imports).iter().enumerate() {
        let stub = obj.with_extension(format!("{i}.tbd"));
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

/// Purpose: This function writes a `libSystem.tbd` exporting the given
/// symbols, for one architecture.
///
/// Inputs:
/// - `arch`: `arm64` or `x86_64`.
/// - `symbols`: the symbol names, with Mach-O's leading underscore.
///
/// Outputs: the stub, in Apple's text format, version 4.
fn libsystem_tbd(arch: &str, symbols: &[String]) -> String {
    stub_tbd(arch, "/usr/lib/libSystem.B.dylib", symbols)
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

/// Purpose: This function lists the external symbols a 64-bit Mach-O object
/// leaves undefined: what it imports.
///
/// Inputs:
/// - `b`: the object file's bytes.
///
/// Outputs: the symbol names, or a message when the file is not one.
fn macho_imports(b: &[u8]) -> Result<Vec<String>, String> {
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
            let mut out = Vec::new();
            for i in 0..nsyms {
                // `nlist_64 { u32 n_strx; u8 n_type; u8 n_sect; u16 n_desc; u64 n_value }`.
                let e = symoff + 16 * i;
                let strx = u32_at(e).ok_or_else(bad)? as usize;
                let ty = *b.get(e + 4).ok_or_else(bad)?;
                let value = b.get(e + 8..e + 16).ok_or_else(bad)?;
                let debug = ty & 0xe0 != 0;
                let undefined = ty & 0x0e == 0;
                let external = ty & 0x01 != 0;
                // A common symbol is undefined with a size; LLVM emits none.
                let common = value.iter().any(|v| *v != 0);
                if debug || !undefined || !external || common {
                    continue;
                }
                let name = b.get(stroff + strx..).ok_or_else(bad)?;
                let end = name.iter().position(|c| *c == 0).ok_or_else(bad)?;
                out.push(String::from_utf8_lossy(&name[..end]).into_owned());
            }
            out.sort();
            out.dedup();
            return Ok(out);
        }
        at += size;
    }
    Ok(Vec::new())
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
/// glibc, or musl where glibc is not installed.
fn link_elf(obj: &Path, out: &Path) -> Result<(), String> {
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
        obj.display().to_string(),
        // libc's own dependencies, the loader among them, are the loader's to
        // find at run time.
        "--allow-shlib-undefined".into(),
        "--as-needed".into(),
    ];
    args.extend(libc.libs);
    run_lld(&args)
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
        let tbd = libsystem_tbd("arm64", &["_malloc".into(), "___error".into()]);
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
    fn a_file_that_is_not_mach_o_is_refused() {
        assert!(macho_imports(b"\x7fELF").is_err());
    }
}
