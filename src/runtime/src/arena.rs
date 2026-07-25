//! Per-frame bump arena for non-escaping collections — Phase 2 foundation.
//!
//! A collection the compiler proves does not escape its region (not returned,
//! not stored globally, not passed to a call, not captured across an `await`)
//! can be allocated by bumping a pointer and freed *en masse* when the region
//! ends — no per-object `malloc`/`free` and no refcounting. That is the whole
//! win: the Phase 0 data showed AOT collection churn dominates allocation-heavy
//! code, and most of those collections are frame-local temporaries.
//!
//! ## Shape
//!
//! One arena per thread (async workers each get their own — an arena value must
//! never cross an `await`, which the escape analysis enforces). The arena is a
//! list of system-allocated chunks; [`ArenaAlloc`] bumps within the current
//! chunk and starts a new one on overflow. [`mark`] snapshots the bump cursor and
//! [`reset`] rolls it back, keeping the chunks for reuse — so a loop that resets
//! every iteration recycles the same memory instead of growing without bound.
//!
//! [`ArenaAlloc`] is a zero-sized [`allocator_api2::alloc::Allocator`]: a
//! `Vec<T, ArenaAlloc>` has the same layout as a `Vec<T>`, so an arena-backed
//! collection is byte-compatible with the heap form and the C accessors read
//! either without change. Its `deallocate` is a no-op — the arena reclaims in
//! bulk at [`reset`].
//!
//! ## Safety contract
//!
//! Everything the arena hands out is valid only until the next [`reset`] to a
//! mark at or before the allocation. The compiler is responsible for ensuring no
//! arena pointer is read after its region's `reset`; a violation is a
//! use-after-free the arena cannot detect. Arena operations are **not
//! reentrant** — `bump`/`mark`/`reset` never call back into the arena (a new
//! chunk comes from the system allocator, which does not), so the `UnsafeCell`
//! access below is sound.

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use std::alloc::{alloc as sys_alloc, dealloc as sys_dealloc, Layout};

use allocator_api2::alloc::{AllocError, Allocator};

/// Default chunk size: large enough that a loop body's collections rarely start a
/// second chunk, small enough not to reserve much when the arena is unused.
const CHUNK: usize = 64 * 1024;

struct Chunk {
    ptr: *mut u8,
    cap: usize,
}

struct ArenaState {
    chunks: Vec<Chunk>,
    /// Index of the chunk the cursor is in.
    cur: usize,
    /// Bump offset within `chunks[cur]`.
    pos: usize,
}

/// A saved cursor. `reset(mark(...))` restores the arena to this point.
#[derive(Clone, Copy)]
pub struct Mark {
    cur: usize,
    pos: usize,
}

impl ArenaState {
    const fn new() -> Self {
        ArenaState { chunks: Vec::new(), cur: 0, pos: 0 }
    }

    /// Bump `size` bytes aligned to `align`, allocating a fresh chunk if the
    /// current one cannot hold it.
    unsafe fn bump(&mut self, size: usize, align: usize) -> *mut u8 {
        // Grow the chunk list lazily on first use.
        if self.chunks.is_empty() {
            self.push_chunk(CHUNK.max(size + align));
        }
        loop {
            let chunk = &self.chunks[self.cur];
            let base = chunk.ptr as usize;
            let aligned = (base + self.pos + align - 1) & !(align - 1);
            let end = aligned + size;
            if end <= base + chunk.cap {
                self.pos = end - base;
                return aligned as *mut u8;
            }
            // Does not fit: advance to the next existing chunk, or allocate one.
            if self.cur + 1 < self.chunks.len() {
                self.cur += 1;
                self.pos = 0;
            } else {
                self.push_chunk(CHUNK.max(size + align));
                self.cur = self.chunks.len() - 1;
                self.pos = 0;
            }
        }
    }

    fn push_chunk(&mut self, cap: usize) {
        // SAFETY: cap is non-zero; 16-byte alignment satisfies every collection.
        let layout = Layout::from_size_align(cap, 16).expect("arena chunk layout");
        let ptr = unsafe { sys_alloc(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        self.chunks.push(Chunk { ptr, cap });
    }

    fn mark(&self) -> Mark {
        Mark { cur: self.cur, pos: self.pos }
    }

    fn reset(&mut self, m: Mark) {
        self.cur = m.cur;
        self.pos = m.pos;
    }
}

impl Drop for ArenaState {
    fn drop(&mut self) {
        for c in &self.chunks {
            let layout = Layout::from_size_align(c.cap, 16).expect("arena chunk layout");
            // SAFETY: each chunk came from `sys_alloc` with this exact layout and
            // is not referenced again (the thread and its arena are ending).
            unsafe { sys_dealloc(c.ptr, layout) };
        }
    }
}

thread_local! {
    /// This thread's arena. `UnsafeCell` (not `RefCell`) because arena operations
    /// are non-reentrant — see the module safety note — so no borrow tracking is
    /// needed on this hot path.
    static ARENA: UnsafeCell<ArenaState> = const { UnsafeCell::new(ArenaState::new()) };
}

/// Snapshot the current thread's arena cursor.
#[inline]
pub fn mark() -> Mark {
    ARENA.with(|a| unsafe { (*a.get()).mark() })
}

/// Roll the current thread's arena back to `m`, freeing everything bump-allocated
/// since (in bulk — no per-object work), and keeping the chunks for reuse.
///
/// # Safety
/// No pointer handed out after `m` may be read after this call.
#[inline]
pub unsafe fn reset(m: Mark) {
    ARENA.with(|a| unsafe { (*a.get()).reset(m) });
}

/// A zero-sized allocator that bump-allocates from the current thread's arena.
/// `deallocate` is a no-op; memory is reclaimed in bulk by [`reset`].
#[derive(Clone, Copy, Default)]
pub struct ArenaAlloc;

unsafe impl Allocator for ArenaAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let size = layout.size().max(1);
        let p = ARENA.with(|a| unsafe { (*a.get()).bump(size, layout.align()) });
        let slice = core::ptr::slice_from_raw_parts_mut(p, size);
        NonNull::new(slice).ok_or(AllocError)
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        // No-op: the arena frees in bulk at `reset`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allocator_api2::vec::Vec as AVec;

    #[test]
    fn vec_lives_in_arena_and_resets() {
        let start = mark();
        {
            // A Vec backed by the arena — its storage comes from `bump`.
            let mut v: AVec<i64, ArenaAlloc> = AVec::new_in(ArenaAlloc);
            for i in 0..1000_i64 {
                v.push(i);
            }
            assert_eq!(v.iter().sum::<i64>(), (0..1000_i64).sum::<i64>());
            // Leak the Vec's ownership without running Drop's dealloc bookkeeping
            // path in a way that matters — dealloc is a no-op regardless.
            core::mem::forget(v);
        }
        // Reset reclaims everything; a subsequent allocation reuses the space.
        unsafe { reset(start) };
        let after = mark();
        assert_eq!(after.cur, start.cur);
        assert_eq!(after.pos, start.pos, "reset must roll the cursor back");
    }

    #[test]
    fn survives_multi_chunk_growth() {
        let start = mark();
        // Force several chunks' worth of allocation.
        let mut keep: Vec<AVec<u8, ArenaAlloc>> = Vec::new();
        for _ in 0..8 {
            let mut v: AVec<u8, ArenaAlloc> = AVec::with_capacity_in(CHUNK, ArenaAlloc);
            v.resize(CHUNK, 0xAB);
            assert_eq!(v.len(), CHUNK);
            keep.push(v);
        }
        for v in &keep {
            assert!(v.iter().all(|&b| b == 0xAB));
        }
        keep.into_iter().for_each(core::mem::forget);
        unsafe { reset(start) };
    }
}
