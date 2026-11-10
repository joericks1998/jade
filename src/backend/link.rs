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
//! as a runtime file. The start-up code that development packages normally
//! supply as `Scrt1.o` is three instructions of setup and a call to
//! `__libc_start_main`; the backend emits it as `_start` (see `START_X86_64`),
//! so linking needs nothing a minimal system lacks.

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
    let stub = obj.with_extension("tbd");
    std::fs::write(&stub, libsystem_tbd(arch, &imports))
        .map_err(|e| format!("cannot write {}: {e}", stub.display()))?;
    let version = macos_version();
    let args = [
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
        stub.display().to_string(),
    ];
    let r = run_lld(&args);
    std::fs::remove_file(&stub).ok();
    r
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
    let target = format!("{arch}-macos");
    let mut names: Vec<String> = symbols.iter().map(|s| format!("'{s}'")).collect();
    // Lazy binding asks for the stub binder when the loader does not use
    // chained fixups; listing it costs nothing when it is not needed.
    if !symbols.iter().any(|s| s == "dyld_stub_binder") {
        names.push("'dyld_stub_binder'".into());
    }
    format!(
        "--- !tapi-tbd\n\
         tbd-version:     4\n\
         targets:         [ {target} ]\n\
         install-name:    '/usr/lib/libSystem.B.dylib'\n\
         current-version: 1351\n\
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

/// Purpose: This function links on Linux, against the system's glibc.
fn link_elf(obj: &Path, out: &Path) -> Result<(), String> {
    let (interp, dir) = glibc()?;
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
        interp.into(),
        "-o".into(),
        out.display().to_string(),
        obj.display().to_string(),
        // libc's own dependencies, the loader among them, are the loader's to
        // find at run time.
        "--allow-shlib-undefined".into(),
        "--as-needed".into(),
    ];
    // Older glibc keeps threads, `dlopen` and the math library apart from
    // libc; newer keeps only math apart. `--as-needed` drops what is unused.
    for lib in ["libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2"] {
        let p = Path::new(dir).join(lib);
        if p.is_file() {
            args.push(p.display().to_string());
        }
    }
    run_lld(&args)
}

/// Purpose: This function finds glibc: its dynamic loader and the directory
/// holding `libc.so.6`.
fn glibc() -> Result<(&'static str, &'static str), String> {
    let (interp, dirs): (&str, &[&str]) = if cfg!(target_arch = "aarch64") {
        (
            "/lib/ld-linux-aarch64.so.1",
            &[
                "/lib/aarch64-linux-gnu",
                "/usr/lib/aarch64-linux-gnu",
                "/lib64",
                "/usr/lib64",
                "/lib",
                "/usr/lib",
            ],
        )
    } else {
        (
            "/lib64/ld-linux-x86-64.so.2",
            &[
                "/lib/x86_64-linux-gnu",
                "/usr/lib/x86_64-linux-gnu",
                "/lib64",
                "/usr/lib64",
                "/lib",
                "/usr/lib",
            ],
        )
    };
    let dir = dirs.iter().find(|d| Path::new(d).join("libc.so.6").is_file()).ok_or_else(|| {
        format!(
            "linking needs glibc's libc.so.6, which is in none of {}; a system on another C \
                 library is not supported yet",
            dirs.join(", ")
        )
    })?;
    Ok((interp, dir))
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

    #[test]
    fn a_file_that_is_not_mach_o_is_refused() {
        assert!(macho_imports(b"\x7fELF").is_err());
    }
}
