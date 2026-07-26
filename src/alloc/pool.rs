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

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the *wrapper*, not the pool — `jade_runtime::pool` has its
    // own tests for size classes and the free list. What is checked here is that
    // each `GlobalAlloc` method reaches the pool with its arguments intact, which
    // is the only thing this file can get wrong.
    //
    // `PoolAlloc` is driven as an ordinary value rather than installed as the
    // global allocator: the harness keeps the system allocator, so a bug fails an
    // assertion instead of corrupting the process running the assertion.
    const A: PoolAlloc = PoolAlloc;

    // Tests that assert on *addresses* must not share a size class with another
    // test, because `cargo test` runs them in parallel against one process-wide
    // free list. Each such test below owns a class: 32 (recycling), 128
    // (zeroing), 256/512 (realloc). Keep that disjoint when adding one.
    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).expect("valid layout")
    }

    #[test]
    fn alloc_returns_usable_aligned_memory() {
        for (size, align) in [(1, 1), (8, 8), (8, 16), (4096, 8)] {
            let l = layout(size, align);
            unsafe {
                let p = A.alloc(l);
                assert!(!p.is_null(), "alloc({size}, {align}) returned null");
                assert_eq!(p as usize % align, 0, "alloc({size}, {align}) is misaligned");
                // Writing the whole block proves it is really `size` bytes wide.
                std::ptr::write_bytes(p, 0xAB, size);
                assert_eq!(std::ptr::read(p.add(size - 1)), 0xAB);
                A.dealloc(p, l);
            }
        }
    }

    #[test]
    fn freed_block_is_recycled_by_the_pool() {
        // The delegation check. A block handed back and immediately re-requested
        // at the same size comes back at the same address only if it went onto
        // the pool's free list — the system allocator promises no such thing. So
        // this failing means the wrapper stopped reaching `jade_runtime::pool`.
        let l = layout(24, 8); // class 32, owned by this test
        unsafe {
            let first = A.alloc(l);
            A.dealloc(first, l);
            let second = A.alloc(l);
            assert_eq!(first, second, "freed block should come back from the pool");
            A.dealloc(second, l);
        }
    }

    #[test]
    fn alloc_zeroed_clears_a_recycled_block() {
        // `PoolAlloc` does not override `alloc_zeroed`, so it inherits the trait
        // default: `alloc`, then zero `layout.size()` bytes. That inherited
        // behavior is load-bearing here in a way it is not for the system
        // allocator — a pooled block is recycled dirty, so without the zeroing a
        // caller would read the previous occupant's bytes.
        let l = layout(100, 8); // class 128, owned by this test
        unsafe {
            let dirty = A.alloc(l);
            std::ptr::write_bytes(dirty, 0xFF, 100);
            A.dealloc(dirty, l);

            let fresh = A.alloc_zeroed(l);
            assert_eq!(fresh, dirty, "expected the dirtied block back");
            let bytes = std::slice::from_raw_parts(fresh, 100);
            assert!(bytes.iter().all(|&b| b == 0), "alloc_zeroed left stale bytes behind");
            A.dealloc(fresh, l);
        }
    }

    #[test]
    fn realloc_preserves_contents_across_size_classes() {
        // 200 and 500 land in different classes (256 and 512), so this takes the
        // copying path rather than the in-place one. It is the case a wrapper
        // that transposed the old and new size would silently truncate.
        let old = layout(200, 8);
        let new = layout(500, 8);
        unsafe {
            let p = A.alloc(old);
            for i in 0..200 {
                *p.add(i) = i as u8;
            }
            let q = A.realloc(p, old, new.size());
            assert!(!q.is_null(), "realloc returned null");
            let kept = std::slice::from_raw_parts(q, 200);
            assert!(
                kept.iter().enumerate().all(|(i, &b)| b == i as u8),
                "realloc did not preserve the original contents"
            );
            A.dealloc(q, new);
        }
    }

    #[test]
    fn large_and_overaligned_requests_round_trip() {
        // Both fall past the pool onto the system allocator. The wrapper still
        // has to pass size and align through faithfully, because a block that
        // came from the system path has to be freed on the same path.
        unsafe {
            let big = layout(1 << 20, 8);
            let p = A.alloc(big);
            assert!(!p.is_null(), "large allocation returned null");
            std::ptr::write_bytes(p, 1, 1 << 20);
            A.dealloc(p, big);

            let over = layout(8, 4096); // align exceeds the class size
            let q = A.alloc(over);
            assert!(!q.is_null(), "over-aligned allocation returned null");
            assert_eq!(q as usize % 4096, 0, "over-aligned block is misaligned");
            A.dealloc(q, over);
        }
    }
}
