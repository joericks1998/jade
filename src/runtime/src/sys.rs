//! Raw system-allocator bindings and the fatal-abort primitive.
//!
//! The runtime allocates through the **system** allocator (`malloc`/`free`),
//! not Rust's `std` allocator, so heap objects are interchangeable with the C
//! runtime and codegen-emitted globals: a string `malloc`'d here can be
//! `free`'d by C, and a value boxed by C can be freed here. This is what lets
//! the shared crate replace `runtime_lib/common.c`'s allocations symbol-for-
//! symbol without a mismatched-allocator hazard.

unsafe extern "C" {
    /// System `malloc`. Returns a >= 8-byte-aligned pointer (low 3 bits free
    /// for the value tag), or null on failure.
    pub fn malloc(size: usize) -> *mut u8;
    /// System `free`.
    pub fn free(ptr: *mut u8);
    /// System `strlen` — length of a NUL-terminated string (used by the
    /// tagged-string allocator's dup/concat, matching the C runtime).
    pub fn strlen(s: *const u8) -> usize;
}

/// Abort the process on an unrecoverable allocation failure. Mirrors the C
/// runtime's `jade_rt_fatal("out of memory")` outcome (a hard exit, not a
/// catchable Jade exception).
#[cold]
#[inline(never)]
pub fn oom() -> ! {
    // eprintln keeps this observable; abort avoids unwinding through the
    // C-ABI boundary (there is no Rust frame to unwind to from AOT code).
    eprintln!("jade: out of memory");
    std::process::abort()
}
