//! Segregated free-list allocator — Phase 1 of the allocation remediation.
//!
//! The VM churns millions of tiny, short-lived, promptly-freed objects (Phase 0
//! measured 82% of allocations at ≤15 bytes through a ~150 KB working set). A
//! general-purpose `malloc` pays its full bookkeeping on each; a size-classed
//! free-list turns the steady state into a pop/push of an intrusive list with
//! excellent locality. This recovers the throughput mimalloc gave us — as *our*
//! code, so it composes with the region work planned for the AOT backend.
//!
//! ## Why this is safe where mimalloc was not
//!
//! mimalloc corrupted memory because it was a `#[global_allocator]` declared in
//! `jade-runtime`, which every dlopen'd package *also* statically links — two
//! allocator instances with duplicate `__rust_alloc` symbols freed across each
//! other's heaps. This pool lives in the `jade` **binary** crate and is installed
//! as the global allocator only in `main.rs`. It is never linked into a package,
//! so there is exactly one instance in the process. (Extending it to the AOT
//! backend later means calling it through explicit `jrt_*` functions in
//! `jade-runtime`, never a second `#[global_allocator]`.)
//!
//! ## Design
//!
//! Power-of-two size classes from 8 to 4096 bytes; larger requests pass straight
//! to the system allocator. Each class keeps an intrusive singly-linked free list
//! (a freed block stores the "next" pointer in its own first 8 bytes) guarded by
//! a tiny spinlock — no locks that could allocate, no reentrancy, and the
//! critical section is a couple of instructions. Because `GlobalAlloc::dealloc`
//! is handed the original `Layout`, both `alloc` and `dealloc` derive the class
//! from `layout.size()`, so no per-block header is needed. Blocks, once claimed
//! from the system, stay in their class's free list for the life of the process
//! (bounded by peak live memory — tiny here); the OS reclaims them at exit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Smallest class is 2^3 = 8 bytes — big enough to hold the intrusive "next"
/// pointer and satisfy the alignment of any ≤8-byte allocation.
const MIN_SHIFT: usize = 3;
/// Classes 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096.
const NUM_CLASSES: usize = 10;
/// Largest pooled size; anything larger goes to the system allocator.
const MAX_POOLED: usize = 1 << (MIN_SHIFT + NUM_CLASSES - 1); // 4096

/// One size class: a spinlock guarding the intrusive free-list head.
struct SizeClass {
    lock: AtomicBool,
    head: AtomicPtr<u8>,
}

impl SizeClass {
    #[inline]
    fn lock(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    #[inline]
    fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

static CLASSES: [SizeClass; NUM_CLASSES] = [const {
    SizeClass { lock: AtomicBool::new(false), head: AtomicPtr::new(ptr::null_mut()) }
}; NUM_CLASSES];

/// Class index for a request of `size` bytes, or `None` if it should go to the
/// system allocator. `ceil(log2(max(size, 8)))` shifted so class 0 is 8 bytes.
#[inline]
fn class_index(size: usize) -> Option<usize> {
    if size > MAX_POOLED {
        return None;
    }
    let s = size.max(1 << MIN_SHIFT);
    let idx = (usize::BITS - (s - 1).leading_zeros()) as usize - MIN_SHIFT;
    if idx < NUM_CLASSES { Some(idx) } else { None }
}

/// Byte size of class `idx`.
#[inline]
fn class_size(idx: usize) -> usize {
    1 << (MIN_SHIFT + idx)
}

/// Whether a `layout` is served from the pool. A block of `class_size` bytes is
/// allocated `class_size`-aligned, so it satisfies any alignment up to that; a
/// larger alignment request falls through to the system allocator. `alloc` and
/// `dealloc` agree because both decide purely from the `Layout`.
#[inline]
fn pooled_class(layout: Layout) -> Option<usize> {
    let idx = class_index(layout.size())?;
    if layout.align() <= class_size(idx) { Some(idx) } else { None }
}

pub struct PoolAlloc;

unsafe impl GlobalAlloc for PoolAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(idx) = pooled_class(layout) {
            let class = &CLASSES[idx];
            class.lock();
            let head = class.head.load(Ordering::Relaxed);
            if !head.is_null() {
                // Pop: the block's first 8 bytes hold the next free block.
                let next = unsafe { *(head as *const *mut u8) };
                class.head.store(next, Ordering::Relaxed);
                class.unlock();
                return head;
            }
            class.unlock();
            // Miss: carve a fresh block from the system, sized and aligned to the
            // class so it can be reused for any request that maps here. Never
            // returned to the system (stays in the free list); the OS reclaims at
            // exit. Bounded by peak live memory.
            let csize = class_size(idx);
            let block_layout = unsafe { Layout::from_size_align_unchecked(csize, csize) };
            return unsafe { System.alloc(block_layout) };
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(idx) = pooled_class(layout) {
            let class = &CLASSES[idx];
            class.lock();
            // Push: store the current head in this block, then make it the head.
            let old = class.head.load(Ordering::Relaxed);
            unsafe { *(ptr as *mut *mut u8) = old };
            class.head.store(ptr, Ordering::Relaxed);
            class.unlock();
            return;
        }
        unsafe { System.dealloc(ptr, layout) };
    }

    // `alloc_zeroed` uses the default (self.alloc + zero of `layout.size()`),
    // which correctly clears a recycled block's stale bytes. `realloc` uses the
    // default (alloc + copy + dealloc), which routes through the pool correctly;
    // an in-place same-class fast path is a later refinement.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_boundaries() {
        assert_eq!(class_index(1), Some(0)); // → 8
        assert_eq!(class_index(8), Some(0));
        assert_eq!(class_index(9), Some(1)); // → 16
        assert_eq!(class_index(16), Some(1));
        assert_eq!(class_index(17), Some(2)); // → 32
        assert_eq!(class_index(4096), Some(NUM_CLASSES - 1));
        assert_eq!(class_index(4097), None);
        assert_eq!(class_size(0), 8);
        assert_eq!(class_size(NUM_CLASSES - 1), 4096);
    }

    #[test]
    fn alloc_dealloc_roundtrip_and_reuse() {
        let a = PoolAlloc;
        unsafe {
            let l = Layout::from_size_align(24, 8).unwrap(); // → class 32
            let p1 = a.alloc(l);
            assert!(!p1.is_null());
            // Write the whole class-rounded block to prove it's usable memory.
            ptr::write_bytes(p1, 0xAB, 32);
            a.dealloc(p1, l);
            // The very next same-class allocation must recycle the freed block.
            let p2 = a.alloc(l);
            assert_eq!(p1, p2, "freed block should be recycled");
            a.dealloc(p2, l);
        }
    }

    #[test]
    fn large_falls_back_to_system() {
        let a = PoolAlloc;
        unsafe {
            let l = Layout::from_size_align(1 << 20, 8).unwrap(); // 1 MiB > MAX_POOLED
            assert!(pooled_class(l).is_none());
            let p = a.alloc(l);
            assert!(!p.is_null());
            ptr::write_bytes(p, 0x11, 1 << 20);
            a.dealloc(p, l);
        }
    }

    #[test]
    fn overaligned_falls_back_to_system() {
        // align 64 on an 8-byte request exceeds the class size → system path.
        let l = Layout::from_size_align(8, 64).unwrap();
        assert!(pooled_class(l).is_none());
        let a = PoolAlloc;
        unsafe {
            let p = a.alloc(l);
            assert_eq!(p as usize % 64, 0, "system path must honor alignment");
            a.dealloc(p, l);
        }
    }

    #[test]
    fn zeroed_clears_recycled_block() {
        let a = PoolAlloc;
        unsafe {
            let l = Layout::from_size_align(16, 8).unwrap();
            let p1 = a.alloc(l);
            ptr::write_bytes(p1, 0xFF, 16);
            a.dealloc(p1, l);
            let p2 = a.alloc_zeroed(l);
            assert_eq!(p1, p2);
            for i in 0..16 {
                assert_eq!(*p2.add(i), 0, "alloc_zeroed must clear a recycled block");
            }
            a.dealloc(p2, l);
        }
    }
}
