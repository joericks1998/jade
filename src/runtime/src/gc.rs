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
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use crate::coll::{ArrayObj, DictObj, StructObj};
use crate::heap::{ObjHeader, ObjKind};
use crate::value::JadeValue;

/// The AOT element word type stored inside collections (a tagged [`JadeValue`]).
type W = i64;

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

// ── Whole-program refcount activation (B4.2) ──────────────────────────────────
//
// Refcounting is only sound when every `TAG_PTR` word is guaranteed to be an
// `ObjHeader` collection. Function-pointer boxes and futures are also `TAG_PTR`
// but carry no header, so incref/decref/cascade would corrupt them. Codegen
// therefore enables refcounting *per program*: if the whole program contains no
// first-class functions and no async (the "collections-only" property), then no
// fn-box/future value exists anywhere, every `TAG_PTR` is a collection, and
// codegen calls `jrt_rc_enable()` once at `jade_toplevel` entry. Otherwise the
// flag stays off and both engines behave exactly as before (leak everything).
//
// The flag gates the *runtime* builders' element retention (below); codegen gates
// its own emitted incref/decref/scope-exit on the same static decision, so the
// two never disagree.

static RC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Turn on reference counting for this process (see the module note on the
/// collections-only precondition). Idempotent; codegen emits one call at program
/// start when — and only when — it proved the program collections-only.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_rc_enable() {
    RC_ACTIVE.store(true, Ordering::Relaxed);
}

/// Whether refcounting is active. The collection builders consult this to decide
/// whether an inserted or copy-shared element must be retained.
#[inline]
pub(crate) fn rc_active() -> bool {
    RC_ACTIVE.load(Ordering::Relaxed)
}

/// Retain `word` on behalf of a container that now references it — but only when
/// refcounting is active (else a no-op, preserving the pre-B4.2 behavior). A
/// no-op for non-collection words regardless (`jrt_incref` checks the tag). The
/// container's destructor cascades a matching `decref` per element, so every
/// `retain` here is balanced by exactly one cascade decref when the container
/// dies.
#[inline]
pub(crate) fn retain(word: i64) {
    if rc_active() {
        jrt_incref(word);
    }
}

/// Codegen's slot-overwrite release: drop the reference a slot (or global cell)
/// held before it is overwritten by `new`. Skipped when `old == new`, which is
/// exactly the in-place-mutation case (an array `SetIndex` returns the *same*
/// pointer, whose count must not change). Codegen emits this only in refcount
/// mode, so no `RC_ACTIVE` check is needed here.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_rc_replace(old: i64, new: i64) {
    if old != new {
        unsafe { decref_word(old) };
    }
}

// ── Refcounting + destructor (B4.1) ───────────────────────────────────────────
//
// The strong-count half of the Bacon–Rajan scheme. `jrt_incref`/`jrt_decref` are
// the C-ABI mutators codegen will emit (B4.2) at every reference gain/loss; a
// decref that drops the count to zero runs the destructor, which cascades a
// decref to each child collection before freeing the object. Cycles survive this
// (a self-reference keeps the count > 0) — reclaiming them is the trial-deletion
// pass (B4.3); this brick is the acyclic-reclamation foundation it builds on.
//
// **Precondition (important).** A `TAG_PTR` word is only known to be an
// `ObjHeader`-prefixed *collection* (Array/Dict/Struct) for the objects this
// crate allocates. Function-pointer boxes and async futures are *also* `TAG_PTR`
// but carry no `ObjHeader`, so passing one of those words here would read/write
// out of bounds. Codegen (B4.2) must therefore emit `jrt_incref`/`jrt_decref`
// only for values it statically knows are collections; a later brick can give
// fn/future values an `ObjHeader` to make the operation uniform over all
// `TAG_PTR` words. These functions are AOT-only (element word = `i64`); the VM
// manages collection lifetime with `Arc` and never calls them.

/// Whether a `TAG_PTR` pointer is a **refcounted collection** (Array/Dict/Struct)
/// rather than a header-carrying but non-refcounted value. Every `TAG_PTR` word a
/// refcount-enabled program can produce is header-prefixed: collections have
/// their `ObjKind`, and a first-class function box carries `ObjKind::Fn` in the
/// same byte (offset 8) precisely so this check no-ops on it. (Futures, native fn
/// values, and prompts are header-less, but a program producing any of those is
/// not collections-only, so refcounting is off and this is never reached for
/// them.)
///
/// # Safety
/// `p` must be a live `TAG_PTR` pointer whose 8-aligned allocation has a readable
/// `ObjKind` byte at offset 8 (every collection and fn-box satisfies this).
#[inline]
unsafe fn is_collection(p: *const c_void) -> bool {
    let k = unsafe { (*(p as *const ObjHeader)).kind };
    k == ObjKind::Array as u8
        || k == ObjKind::Dict as u8
        || k == ObjKind::Struct as u8
        // A future is not a container, but it is header-carrying and owns a
        // heap allocation, so it is refcounted on exactly the same terms. Its
        // destructor arm in `free_obj` reclaims it without a child cascade.
        || k == ObjKind::Future as u8
}

/// Increment the strong count of the collection a `TAG_PTR` word points at. A
/// no-op for non-pointer words (ints/bools/nil), strings/floats (distinct tags),
/// and non-collection heap values such as function boxes (kind-gated).
#[unsafe(no_mangle)]
pub extern "C" fn jrt_incref(word: W) {
    let v = JadeValue::from_bits(word as u64);
    if v.is_ptr() {
        let p = v.as_ptr() as *mut c_void;
        unsafe {
            if is_collection(p) {
                (*(p as *mut ObjHeader)).incref();
            }
        }
    }
}

/// Decrement the strong count of the collection a `TAG_PTR` word points at; when
/// it reaches zero, run the destructor (cascading a decref to each child, then
/// freeing). A no-op for non-pointer words and strings/floats. See the module
/// precondition.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_decref(word: W) {
    unsafe { decref_word(word) };
}

/// The internal recursive decref used by both `jrt_decref` and the destructor's
/// child cascade. If `word` is a collection pointer whose count hits zero, its
/// destructor runs.
///
/// # Safety
/// `word`, if `TAG_PTR`, must point at a live `ObjHeader`-prefixed collection.
#[inline]
unsafe fn decref_word(word: W) {
    let v = JadeValue::from_bits(word as u64);
    if !v.is_ptr() {
        // Ints/bools/nil carry no allocation; strings/floats are non-refcounted
        // leaves (a separate leak-reclamation concern — they cannot form cycles).
        return;
    }
    let p = v.as_ptr() as *mut c_void;
    // Skip header-carrying non-collections (a fn-box, kind Fn): they are not
    // refcounted and their allocation is not a collection payload.
    if !unsafe { is_collection(p) } {
        return;
    }
    if unsafe { (*(p as *mut ObjHeader)).decref() } {
        unsafe { free_obj(p) };
    }
}

/// Destroy and free a collection whose strong count has reached zero: cascade a
/// decref to every child element/value/field (which may recursively free), then
/// reclaim the payload (`Box::from_raw` + drop) and record the free.
///
/// # Safety
/// `ptr` must be a live, `rc == 0`, `ObjHeader`-prefixed collection allocated by
/// [`leak_obj`] with element word type `i64`; it must not be referenced again.
pub(crate) unsafe fn free_obj(ptr: *mut c_void) {
    let kind = unsafe { (*(ptr as *const ObjHeader)).kind };
    // Reclaim the box (freeing its `Vec`), decref'ing children first. Reading the
    // children through the reconstructed box before `drop` is sound: nothing else
    // references it (rc == 0), and each child points at a *different* object.
    if kind == ObjKind::Array as u8 {
        let b = unsafe { Box::from_raw(ptr as *mut ArrayObj<W>) };
        for &child in b.as_slice() {
            unsafe { decref_word(child) };
        }
        drop(b);
    } else if kind == ObjKind::Dict as u8 {
        let b = unsafe { Box::from_raw(ptr as *mut DictObj<W>) };
        for (_, v) in b.entries() {
            unsafe { decref_word(*v) };
        }
        drop(b);
    } else if kind == ObjKind::Struct as u8 {
        let b = unsafe { Box::from_raw(ptr as *mut StructObj<W>) };
        for (_, v) in b.fields() {
            unsafe { decref_word(*v) };
        }
        drop(b);
    } else if kind == ObjKind::Future as u8 {
        // A future is header-carrying but not a collection: its payload is a
        // single result word, not a Vec of children, so there is no cascade to
        // run. `task::destroy` reclaims the box and records the free itself,
        // because the allocation is a `FutureObj` (lock + condvar) rather than
        // one of the `*Obj<W>` shapes above.
        unsafe { crate::task::destroy(ptr as *mut crate::task::FutureObj) };
        return;
    } else {
        // Float/Str/Fn/etc.: no child cascade defined here. The collector only
        // frees collections (the precondition), so this arm is unreached in
        // practice; leaving the allocation is safe (a leak, not corruption).
        return;
    }
    record_free();
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

/// Test-only serialization for the global live-object counter. Exact-count
/// assertions (the destructor-cascade tests here, and the balanced-cleanup tests
/// in `ffi_coll`) would race across cargo's parallel test threads, since every
/// allocation/free mutates one shared atomic. Every counter-touching test holds
/// this lock for its duration, making those observations effectively serial.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the counter lock (ignoring poison from an unrelated failed test —
    /// a poisoned lock still serializes correctly).
    ///
    /// **Any test that allocates through [`super::leak_obj`] must hold this**,
    /// not only the ones that assert on the count. The live count is
    /// process-global and cargo runs the whole crate's tests on many threads in
    /// one process, so an unlocked allocation anywhere in the binary makes every
    /// count assertion elsewhere a race. That is not theoretical: it turned up
    /// as `destroying_a_future_records_the_free` failing by exactly one, roughly
    /// once in twenty-five runs, from allocations in unrelated modules.
    ///
    /// A module with its own serialization (`task`'s `TEST_LOCK`) still needs
    /// this one as well — that lock orders those tests against each other, not
    /// against the rest of the binary.
    pub fn lock_counter() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coll::ArrayObj;
    use super::test_support::lock_counter;

    #[test]
    fn leak_obj_increments_the_live_count() {
        let _g = lock_counter();
        let before = jrt_heap_live_count();
        let p = leak_obj(ArrayObj::<i64>::new());
        assert!(jrt_heap_live_count() > before, "leak_obj must bump the live count");

        // Reclaim exactly as the destructor will: drop the box, then account it.
        unsafe { drop(Box::from_raw(p as *mut ArrayObj<i64>)) };
        record_free();
    }

    // ── Refcount + destructor (B4.1) ──────────────────────────────────────────

    /// A leaked, empty array (rc = 1); returns its pointer and its tagged word.
    fn new_array() -> (*mut c_void, W) {
        let p = leak_obj(ArrayObj::<W>::new());
        (p, JadeValue::from_ptr(p as *const ()).bits() as i64)
    }

    /// Push a raw word into a leaked array (by pointer).
    unsafe fn push(arr: *mut c_void, word: W) {
        unsafe { (*(arr as *mut ArrayObj<W>)).push(word) };
    }

    #[test]
    fn incref_then_decref_reaches_zero_and_frees() {
        let _g = lock_counter();
        let base = jrt_heap_live_count();
        let (_p, w) = new_array(); // rc 1, live +1
        jrt_incref(w); // rc 2
        jrt_decref(w); // rc 1 — not freed
        assert_eq!(jrt_heap_live_count(), base + 1, "must not free while rc > 0");
        jrt_decref(w); // rc 0 — freed
        assert_eq!(jrt_heap_live_count(), base, "must free when rc hits 0");
    }

    #[test]
    fn decref_cascades_to_owned_children() {
        // parent -> child, each rc 1 (child was created then moved into parent,
        // an ownership transfer that needs no incref). Freeing the parent must
        // free the child too.
        let _g = lock_counter();
        let base = jrt_heap_live_count();
        let (_cp, cw) = new_array();
        let (pp, pw) = new_array();
        unsafe { push(pp, cw) };
        assert_eq!(jrt_heap_live_count(), base + 2);
        jrt_decref(pw); // parent rc 0 -> free -> decref child -> child rc 0 -> free
        assert_eq!(jrt_heap_live_count(), base, "child must be reclaimed with parent");
    }

    #[test]
    fn shared_child_freed_only_when_last_parent_drops() {
        // child referenced by two parents: its rc is 2 (the second reference
        // increfs). Dropping one parent must not free the shared child.
        let _g = lock_counter();
        let base = jrt_heap_live_count();
        let (_cp, cw) = new_array(); // rc 1
        jrt_incref(cw); // rc 2 — the second alias
        let (p1, p1w) = new_array();
        let (p2, p2w) = new_array();
        unsafe {
            push(p1, cw);
            push(p2, cw);
        }
        assert_eq!(jrt_heap_live_count(), base + 3);
        jrt_decref(p1w); // p1 freed; child rc 2 -> 1, survives
        assert_eq!(jrt_heap_live_count(), base + 2, "shared child must survive");
        jrt_decref(p2w); // p2 freed; child rc 1 -> 0, freed
        assert_eq!(jrt_heap_live_count(), base, "child freed with last parent");
    }

    #[test]
    fn self_cycle_is_not_reclaimed_by_refcounting() {
        // a = []; a.push(a) — a references itself, so its rc is 2 (the external
        // binding + the self-reference). Dropping the external reference leaves
        // rc 1: pure refcounting leaks the cycle. This is exactly what the
        // Bacon–Rajan trial-deletion pass (B4.3) exists to collect.
        let _g = lock_counter();
        let base = jrt_heap_live_count();
        let (ap, aw) = new_array(); // rc 1
        unsafe { push(ap, aw) }; // self-reference
        jrt_incref(aw); // rc 2 — the self-reference is a real reference
        jrt_decref(aw); // external ref gone: rc 1, NOT freed
        assert_eq!(jrt_heap_live_count(), base + 1, "cycle survives refcounting (B4.3 collects it)");

        // Manual teardown for the test: reclaim directly WITHOUT the cascade
        // (the cascade would re-enter free_obj on the self-word → double free).
        unsafe { drop(Box::from_raw(ap as *mut ArrayObj<W>)) };
        record_free();
        assert_eq!(jrt_heap_live_count(), base);
    }

    #[test]
    fn balanced_alloc_free_never_underflows() {
        let _g = lock_counter();
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
