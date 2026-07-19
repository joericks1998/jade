//! Heap accounting — the instrument the cycle collector is verified against.
//!
//! AOT-compiled binaries allocate every collection (array/dict/struct) with
//! [`leak_obj`] and, today, never free it (the pre-collector status quo, matching
//! the old C `JK*` objects). The problem the plan calls out is that **no difftest
//! gate can observe a leak** — golden output is identical whether or not memory is
//! reclaimed — and, once refcounting lands, a *premature* free is equally
//! invisible to golden comparison (it corrupts memory, not stdout). So before any
//! refcounting is wired, this module makes the heap population *measurable*:
//!
//!  * every collection allocation goes through [`leak_obj`], which bumps a global
//!    live-object counter;
//!  * the eventual destructor calls [`record_free`], which decrements it (and
//!    debug-asserts against underflow — the signature of a double free);
//!  * [`jrt_heap_report`], which codegen emits just before `main` returns, prints
//!    the live count when `JADE_HEAP_REPORT` is set, so a leak (a positive count
//!    at exit) or a premature free (underflow panic) is observable end to end.
//!
//! Only the AOT path calls these: the VM constructs [`crate::coll`] payloads
//! directly and manages their lifetime with `Arc`, never touching this counter.
//!
//! This is B4.0. The refcount mutators, the child-cascading destructor, and the
//! Bacon–Rajan trial-deletion pass (which will call [`record_free`]) are later
//! bricks; until they land the counter only rises — quantifying exactly how much
//! the AOT path leaks.

use core::ffi::c_void;
use core::sync::atomic::{AtomicI64, Ordering};

/// Number of heap collection objects currently live (allocated minus freed).
///
/// A positive value at program exit is a leak; a value that would go negative is
/// a double/over free. `Relaxed` is sufficient — this is a monotone population
/// count with no ordering dependency on other memory, and correctness of the
/// collector never hinges on the exact interleaving of concurrent counter ops.
static LIVE_OBJECTS: AtomicI64 = AtomicI64::new(0);

/// Box `obj` onto the heap, leak it as a raw type-erased pointer, and record the
/// allocation. **Every** AOT collection allocation must go through here so the
/// live-object count stays authoritative — that is the whole point of the
/// instrument. The returned pointer is `>= 8`-aligned (the payload leads with an
/// `align(8)` [`crate::heap::ObjHeader`]), so codegen's `TAG_PTR` tagging is
/// valid. Reclaimed later by the destructor via `Box::from_raw` + [`record_free`].
#[inline]
pub fn leak_obj<T>(obj: T) -> *mut c_void {
    LIVE_OBJECTS.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(Box::new(obj)) as *mut c_void
}

/// Record that one heap object was reclaimed (the destructor calls this right
/// after `Box::from_raw` drops the payload). Debug-asserts that the counter never
/// goes negative — an underflow means the destructor ran on an object that was
/// already freed (a double free), the exact bug class the instrument exists to
/// catch.
#[inline]
pub fn record_free() {
    let prev = LIVE_OBJECTS.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(
        prev > 0,
        "jade heap free underflow: freed more objects than were allocated (double free?)"
    );
}

/// Live heap-object count (allocations minus frees). Exposed over the C ABI so a
/// test harness — or a future `jade build --leak-check` — can read it directly.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_heap_live_count() -> i64 {
    LIVE_OBJECTS.load(Ordering::Relaxed)
}

/// Print the live heap-object count to stderr **iff** `JADE_HEAP_REPORT` is set in
/// the environment. Codegen emits a call to this immediately before `main`'s
/// `return 0`, so the leak (and, once refcounting lands, a clean zero) is
/// observable without a debugger or a sanitizer build. A no-op — costing only a
/// `getenv` — when the variable is unset, so ordinary binaries are unaffected.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_heap_report() {
    if std::env::var_os("JADE_HEAP_REPORT").is_some() {
        eprintln!("jade-heap: {} live object(s) at exit", jrt_heap_live_count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coll::ArrayObj;

    #[test]
    fn leak_obj_increments_the_live_count() {
        // Race-free under parallel tests: our own allocation guarantees the count
        // is strictly greater afterward; concurrent allocations only add more.
        let before = jrt_heap_live_count();
        let p = leak_obj(ArrayObj::<i64>::new());
        assert!(jrt_heap_live_count() > before, "leak_obj must bump the live count");

        // Reclaim exactly as the destructor will: drop the box, then account it.
        unsafe { drop(Box::from_raw(p as *mut ArrayObj<i64>)) };
        record_free();
    }

    #[test]
    fn balanced_alloc_free_never_underflows() {
        // `record_free` debug-asserts against underflow; a matched sequence of
        // allocations and frees must pass it cleanly. (No assertion on the global
        // count itself — it is shared with concurrently-running tests, so only
        // per-op invariants, not its absolute value, are race-free to check.)
        let ptrs: Vec<*mut ArrayObj<i64>> =
            (0..8).map(|_| leak_obj(ArrayObj::<i64>::new()) as *mut ArrayObj<i64>).collect();
        for p in ptrs {
            unsafe { drop(Box::from_raw(p)) };
            record_free();
        }
        // Reached here without the underflow debug-assert firing.
        let _ = jrt_heap_report(); // exercises the report path (no-op unless env set)
    }
}
