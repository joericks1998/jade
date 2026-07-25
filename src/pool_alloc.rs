//! The `jade` binary's global allocator — a thin `GlobalAlloc` shell over the
//! shared pool in `jade_runtime::pool`.
//!
//! The pool itself (a size-classed free-list) lives in `jade-runtime` so both
//! engines share one implementation: the AOT path calls `jade_runtime::pool`
//! directly from `gc::leak_obj`/`free_obj`, and the interpreter reaches it
//! through this global-allocator wrapper. The wrapper is declared as the global
//! allocator only in `main.rs`, in the binary — never in `jade-runtime` — so it
//! applies solely to the `jade` process and can never be linked into a dlopen'd
//! package (the mistake mimalloc made). See `jade_runtime::pool` for the design
//! and the dual-instance-safety argument.

use std::alloc::{GlobalAlloc, Layout};

pub struct PoolAlloc;

unsafe impl GlobalAlloc for PoolAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        jade_runtime::pool::alloc(layout.size(), layout.align())
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { jade_runtime::pool::dealloc(ptr, layout.size(), layout.align()) };
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { jade_runtime::pool::realloc(ptr, layout.size(), layout.align(), new_size) }
    }

    // `alloc_zeroed` uses the default (self.alloc + zero of `layout.size()`),
    // which clears a recycled block's stale bytes.
}
