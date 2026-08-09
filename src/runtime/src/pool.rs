//! Shared segregated free-list allocator — the one pool both engines use.
//!
//! Phase 1 put a pool behind the VM binary's global allocator. Phase 1b moves the
//! implementation here so the AOT path can use it too: `gc::leak_obj`/`free_obj`
//! call [`alloc`]/[`dealloc`] directly, and the VM's `GlobalAlloc` wrapper (in the
//! `jade` binary) delegates here as well, so there is a single implementation.
//!
//! These are ordinary functions, **never a `#[global_allocator]`**. That is the
//! invariant that keeps them safe in a process that dlopen's a native package:
//! each linked copy of `jade-runtime` (the VM, an AOT binary, a package) has its
//! own pool statics, but no pointer ever crosses between them (the FFI deep-copies
//! at the boundary), so no pool ever frees another's memory. A global-allocator
//! symbol override — the mimalloc mistake — is the only thing that reintroduces
//! the cross-heap hazard.
//!
//! Design: power-of-two size classes 8..4096, each an intrusive free list (a
//! freed block stores the next pointer in its own first 8 bytes) behind a tiny
//! spinlock; larger requests and over-aligned ones pass to the system allocator.
//! The caller supplies `size`/`align` on both alloc and free, so no per-block
//! header is needed. Blocks stay in their class for the life of the process
//! (bounded by peak live memory); the OS reclaims them at exit.

// `System` explicitly — NOT the `std::alloc::alloc`/`dealloc` free functions,
// which route through the *global* allocator. In the `jade` binary the global
// allocator delegates here, so using them for the fallback would recurse
// infinitely (stack overflow) on any request the pool passes through.
use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

const MIN_SHIFT: usize = 3; // smallest class = 8 bytes (holds the intrusive ptr)
const NUM_CLASSES: usize = 10; // 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096
const MAX_POOLED: usize = 1 << (MIN_SHIFT + NUM_CLASSES - 1); // 4096

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

static CLASSES: [SizeClass; NUM_CLASSES] =
    [const { SizeClass { lock: AtomicBool::new(false), head: AtomicPtr::new(ptr::null_mut()) } };
        NUM_CLASSES];

#[inline]
fn class_index(size: usize) -> Option<usize> {
    if size > MAX_POOLED {
        return None;
    }
    let s = size.max(1 << MIN_SHIFT);
    let idx = (usize::BITS - (s - 1).leading_zeros()) as usize - MIN_SHIFT;
    if idx < NUM_CLASSES { Some(idx) } else { None }
}

#[inline]
fn class_size(idx: usize) -> usize {
    1 << (MIN_SHIFT + idx)
}

/// The class to serve `(size, align)` from, or `None` for the system path. A
/// class block is `class_size`-aligned, so it satisfies any alignment up to that;
/// alloc and dealloc agree because both decide purely from `(size, align)`.
#[inline]
fn pooled_class(size: usize, align: usize) -> Option<usize> {
    let idx = class_index(size)?;
    if align <= class_size(idx) { Some(idx) } else { None }
}

/// Allocate at least `size` bytes aligned to `align`. Mirrors `GlobalAlloc::alloc`
/// (null on failure). `size` must be non-zero.
#[inline]
pub fn alloc(size: usize, align: usize) -> *mut u8 {
    if let Some(idx) = pooled_class(size, align) {
        let class = &CLASSES[idx];
        class.lock();
        let head = class.head.load(Ordering::Relaxed);
        if !head.is_null() {
            let next = unsafe { *(head as *const *mut u8) };
            class.head.store(next, Ordering::Relaxed);
            class.unlock();
            return head;
        }
        class.unlock();
        let csize = class_size(idx);
        // SAFETY: csize is a non-zero power of two, a valid Layout.
        let layout = unsafe { Layout::from_size_align_unchecked(csize, csize) };
        return unsafe { System.alloc(layout) };
    }
    // SAFETY: caller guarantees a valid non-zero size; align is a power of two.
    let layout = unsafe { Layout::from_size_align_unchecked(size, align) };
    unsafe { System.alloc(layout) }
}

/// Free a block from [`alloc`]. `size`/`align` must match the allocating call.
///
/// # Safety
/// `ptr` came from [`alloc`] with the same `size`/`align` and is not used again.
#[inline]
pub unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    if let Some(idx) = pooled_class(size, align) {
        let class = &CLASSES[idx];
        class.lock();
        let old = class.head.load(Ordering::Relaxed);
        unsafe { *(ptr as *mut *mut u8) = old };
        class.head.store(ptr, Ordering::Relaxed);
        class.unlock();
        return;
    }
    let layout = unsafe { Layout::from_size_align_unchecked(size, align) };
    unsafe { System.dealloc(ptr, layout) };
}

/// Resize a block, preserving its contents up to `min(old_size, new_size)`.
/// Mirrors `GlobalAlloc::realloc`.
///
/// # Safety
/// `ptr` came from [`alloc`] with `(old_size, align)`.
#[inline]
pub unsafe fn realloc(ptr: *mut u8, old_size: usize, align: usize, new_size: usize) -> *mut u8 {
    // If both sizes land in the same class the block already fits — reuse it.
    if let (Some(a), Some(b)) = (pooled_class(old_size, align), pooled_class(new_size, align)) {
        if a == b {
            return ptr;
        }
    }
    let new = alloc(new_size, align);
    if !new.is_null() {
        let copy = old_size.min(new_size);
        unsafe {
            ptr::copy_nonoverlapping(ptr, new, copy);
            dealloc(ptr, old_size, align);
        }
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_boundaries() {
        assert_eq!(class_index(1), Some(0));
        assert_eq!(class_index(8), Some(0));
        assert_eq!(class_index(9), Some(1));
        assert_eq!(class_index(4096), Some(NUM_CLASSES - 1));
        assert_eq!(class_index(4097), None);
    }

    #[test]
    fn roundtrip_and_reuse() {
        unsafe {
            let p1 = alloc(24, 8);
            assert!(!p1.is_null());
            ptr::write_bytes(p1, 0xAB, 24);
            dealloc(p1, 24, 8);
            let p2 = alloc(24, 8);
            assert_eq!(p1, p2, "freed block should recycle");
            dealloc(p2, 24, 8);
        }
    }

    #[test]
    fn realloc_same_class_is_inplace() {
        unsafe {
            let p = alloc(20, 8); // class 32
            let q = realloc(p, 20, 8, 30); // still class 32
            assert_eq!(p, q);
            dealloc(q, 30, 8);
        }
    }

    #[test]
    fn large_and_overaligned_use_system() {
        assert!(pooled_class(1 << 20, 8).is_none());
        assert!(pooled_class(8, 64).is_none());
        unsafe {
            let p = alloc(8, 64);
            assert_eq!(p as usize % 64, 0);
            dealloc(p, 8, 64);
        }
    }
}
