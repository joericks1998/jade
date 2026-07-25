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
//! reentrant**, which is what makes the `UnsafeCell` access below sound: while a
//! transient `&mut ArenaState` is live, nothing must re-enter an arena op on this
//! thread. The only allocation reachable from `bump` is `self.chunks.push` (a std
//! `Vec` on the **global** allocator) and `sys_alloc` — so the load-bearing
//! invariant is precisely that **the process's global allocator never reads
//! `ARENA`**. That holds today because `PoolAlloc` → `jade_runtime::pool` touches
//! only its own free-lists; do NOT make the global allocator (or an alloc-error
//! hook) arena-aware, or `chunks.push` re-entering `bump` becomes aliasing UB.
//!
//! `reset` runs no destructors — it only moves the cursor — so anything with a
//! non-arena `Drop` obligation placed in an arena collection (notably a
//! `DictObj`'s `String` keys, which allocate globally) leaks until process exit.
//! Arena arrays of scalars have no such payload; that is why the codegen targets
//! them first.

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
            // Round the cursor up to `align`, then work in offsets and derive the
            // result from `chunk.ptr` so the returned pointer keeps the chunk's
            // provenance (sound under strict provenance; no int→ptr cast).
            let aligned = (base + self.pos + align - 1) & !(align - 1);
            let offset = aligned - base;
            let end = offset + size;
            if end <= chunk.cap {
                self.pos = end;
                // SAFETY: `offset <= end <= cap`, so `ptr.add(offset)` is in-bounds
                // of the chunk; `base` is 16-aligned and `align` divides the
                // rounded offset, so the result has `align` alignment.
                return unsafe { chunk.ptr.add(offset) };
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

// ── C-ABI surface for codegen ─────────────────────────────────────────────────
//
// A `Mark` is (chunk index, byte offset). `pos` is the offset within its chunk,
// whose cap is `CHUNK.max(size + align)` — so `pos` can exceed CHUNK for an
// oversized allocation; the real losslessness bound is `pos < 2^32` (a single
// arena allocation must not reach 4 GiB) and `cur < 2^31`. Both hold for any real
// workload, so the pair packs into one `i64` token that codegen stashes in a slot
// and hands back to `reset` at the region's end (correct across early returns —
// each exit resets to its own region's mark).

impl Mark {
    #[inline]
    fn to_token(self) -> i64 {
        debug_assert!(self.pos < (1 << 32) && self.cur < (1 << 30), "arena mark out of token range");
        // Shift left 1 so the token is even (low bit 0). Codegen stores the token
        // in an ordinary refcounted register; an even value reads as an int, so
        // the incref/decref emitted around that register no-op on it (a `TAG_PTR`
        // pattern would send the collector chasing a bogus object).
        (((self.cur as i64) << 32) | (self.pos as i64)) << 1
    }
    #[inline]
    fn from_token(v: i64) -> Self {
        let packed = v >> 1;
        Mark { cur: (packed >> 32) as usize, pos: (packed & 0xFFFF_FFFF) as usize }
    }
}

/// Snapshot the arena cursor as a token (codegen emits this at region entry).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_arena_mark() -> i64 {
    mark().to_token()
}

/// Reset the arena to a token from [`jrt_arena_mark`] (region exit).
///
/// # Safety
/// No arena pointer handed out after the corresponding `mark` may be read after
/// this call — the compiler's escape analysis is responsible for that.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_arena_reset(token: i64) {
    unsafe { reset(Mark::from_token(token)) };
}

// ── Arena collection constructors ─────────────────────────────────────────────
//
// Mirror `ffi_coll`'s heap constructors, but the object header AND its backing
// store live in the arena (via `ArenaAlloc`), and elements are stored WITHOUT a
// retain: arena collections are non-escaping and never refcounted, so `reset`
// frees the whole tree at once with no per-element decref. The compiler must only
// place scalar or other-arena words in them (no owned heap-collection references),
// which the escape analysis guarantees.

use crate::coll::{ArrayObj, DictObj, StructObj};
use crate::cstr;
use crate::value::JadeValue;
use allocator_api2::alloc::Global;
use core::ffi::{c_char, c_void};

/// The AOT element word type (a tagged [`JadeValue`]).
type W = i64;

// The whole scheme rests on an arena-backed collection being byte-identical to
// its heap form, so the ordinary `*const ArrayObj<W, Global>` accessors read it.
// That holds only if the ZST allocator adds no bytes — assert it at compile time
// so a future `allocator_api2` layout change fails the build instead of silently
// mis-reading memory.
const _: () = {
    use core::mem::{align_of, size_of};
    assert!(size_of::<ArrayObj<W, ArenaAlloc>>() == size_of::<ArrayObj<W, Global>>());
    assert!(align_of::<ArrayObj<W, ArenaAlloc>>() == align_of::<ArrayObj<W, Global>>());
    assert!(size_of::<DictObj<W, ArenaAlloc>>() == size_of::<DictObj<W, Global>>());
    assert!(size_of::<StructObj<W, ArenaAlloc>>() == size_of::<StructObj<W, Global>>());
};

/// Move `obj` into a fresh arena block and return it type-erased. The header
/// leads with an `align(8)` `ObjHeader`, so the pointer is `TAG_PTR`-taggable.
/// The header is marked [`flags::ARENA`](crate::heap::flags::ARENA) so the
/// refcount ops no-op on it and the collector never frees it — only `reset` does.
#[inline]
unsafe fn arena_box<T>(obj: T) -> *mut c_void {
    let p = ARENA.with(|a| unsafe {
        (*a.get()).bump(core::mem::size_of::<T>().max(1), core::mem::align_of::<T>())
    }) as *mut T;
    unsafe { p.write(obj) };
    // Every arena object leads with an ObjHeader at offset 0.
    unsafe { (*(p as *const crate::heap::ObjHeader)).set_arena() };
    p as *mut c_void
}

/// `jrt_karr_new` for the arena: an empty arena-backed array.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_karr_new_arena() -> *mut c_void {
    unsafe { arena_box(ArrayObj::<W, ArenaAlloc>::new_in(ArenaAlloc)) }
}

/// `jrt_karr_push` for the arena: append `val` with no retain.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_karr_push_arena(arr: *mut c_void, val: W) {
    unsafe { (*(arr as *mut ArrayObj<W, ArenaAlloc>)).push(val) };
}

/// `jrt_kdict_new` for the arena: an empty arena-backed dict.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kdict_new_arena() -> *mut c_void {
    unsafe { arena_box(DictObj::<W, ArenaAlloc>::new_in(ArenaAlloc)) }
}

/// `jrt_kdict_set` for the arena: `key_word` is a tagged string (bytes copied
/// into the key `String`), `val` stored with no retain.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kdict_set_arena(dict: *mut c_void, key_word: W, val: W) {
    unsafe {
        let key_ptr = JadeValue::from_bits(key_word as u64).as_ptr() as *const c_char;
        (*(dict as *mut DictObj<W, ArenaAlloc>)).set(cstr::borrow(key_ptr), val);
    }
}

/// `jrt_kstruct_new` for the arena.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kstruct_new_arena(type_name: *const c_char) -> *mut c_void {
    let name = unsafe { cstr::borrow(type_name) };
    unsafe { arena_box(StructObj::<W, ArenaAlloc>::new_in(name, ArenaAlloc)) }
}

/// `jrt_kstruct_set` for the arena: set `field` to `val` with no retain.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_kstruct_set_arena(s: *mut c_void, field: *const c_char, val: W) {
    unsafe {
        let f = cstr::borrow(field);
        (*(s as *mut StructObj<W, ArenaAlloc>)).set_field(f, val);
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

    fn int_word(i: i64) -> W {
        JadeValue::from_int(i).bits() as i64
    }

    /// A tagged, NUL-terminated string word for use as a dict key.
    unsafe fn str_word(s: &[u8]) -> W {
        let p = crate::string::new(s.len(), crate::string::TRUSTED);
        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len()) };
        JadeValue::from_str_ptr(p as *const ()).bits() as i64
    }

    #[test]
    fn arena_array_reads_through_standard_accessors() {
        use crate::ffi_coll::{jrt_coll_array_get, jrt_coll_array_len, jrt_kind_of};
        use crate::heap::ObjKind;
        let m = mark();
        let a = jrt_karr_new_arena();
        jrt_karr_push_arena(a, int_word(10));
        jrt_karr_push_arena(a, int_word(20));
        jrt_karr_push_arena(a, int_word(30));
        // The array lives in the arena but is byte-compatible with the heap form,
        // so the ordinary C accessors read it with no special-casing.
        assert_eq!(jrt_kind_of(a), ObjKind::Array as i64);
        assert_eq!(jrt_coll_array_len(a), 3);
        assert_eq!(jrt_coll_array_get(a, 0), int_word(10));
        assert_eq!(jrt_coll_array_get(a, 2), int_word(30));
        unsafe { reset(m) };
    }

    #[test]
    fn arena_dict_reads_through_standard_accessors() {
        use crate::ffi_coll::{jrt_coll_dict_get, jrt_kind_of};
        use crate::heap::ObjKind;
        let m = mark();
        let d = jrt_kdict_new_arena();
        unsafe {
            jrt_kdict_set_arena(d, str_word(b"id"), int_word(7));
            jrt_kdict_set_arena(d, str_word(b"n"), int_word(42));
        }
        assert_eq!(jrt_kind_of(d), ObjKind::Dict as i64);
        let mut out: W = 0;
        assert_eq!(jrt_coll_dict_get(d, b"id\0".as_ptr() as *const c_char, &mut out), 1);
        assert_eq!(out, int_word(7));
        assert_eq!(jrt_coll_dict_get(d, b"n\0".as_ptr() as *const c_char, &mut out), 1);
        assert_eq!(out, int_word(42));
        assert_eq!(jrt_coll_dict_get(d, b"x\0".as_ptr() as *const c_char, &mut out), 0);
        unsafe { reset(m) };
    }

    #[test]
    fn mark_token_roundtrips() {
        let m = mark();
        let t = m.to_token();
        let back = Mark::from_token(t);
        assert_eq!(back.cur, m.cur);
        assert_eq!(back.pos, m.pos);
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
