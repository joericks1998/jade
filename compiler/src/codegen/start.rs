//! The start-up code a Linux program begins at, before `main`.
//!
//! A linker normally takes `_start` from `Scrt1.o`, which a C library's
//! development package supplies. It is three instructions of setup and a call
//! to `__libc_start_main`, so codegen writes it into the program's module as
//! inline assembly instead, and linking needs nothing a minimal system lacks.
//! musl's `__libc_start_main` reads only `main`, `argc` and `argv`, the first
//! three of the arguments glibc's takes, so one `_start` serves both. macOS's
//! loader calls `main` itself, so a Mac program has none.

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
