//! The unified heap object header shared by every reference-typed Jade value.
//!
//! Both engines allocate reference types (dict, array, struct, string, boxed
//! float, closure, future) with an [`ObjHeader`] at offset 0, followed by the
//! kind-specific payload. The header is the single home for the memory-model
//! machinery the VM previously got from `Arc` and the C runtime got from
//! nothing (malloc-and-leak):
//!
//!  * **Refcounting** — `rc` is the strong count. [`ObjHeader::incref`] /
//!    [`ObjHeader::decref`] are the only mutators; `decref` reports when the
//!    count reaches zero so the caller can run the destructor and `free`.
//!  * **Cycle collection** — Jade arrays/structs are reference types, so
//!    cycles are representable (`a = []; a.push(a)`). Pure refcounting would
//!    leak them, and the AOT target is long-running services, so a
//!    Bacon–Rajan trial-deletion collector (the Nim-ORC approach) runs over
//!    "candidate roots": objects that were decref'd but did not hit zero. The
//!    `color` field and the `BUFFERED` flag are that collector's per-object
//!    state; see [`Color`].
//!
//! The layout is `#[repr(C, align(8))]` so it is ABI-stable for the residual C
//! runtime to share, and so the object pointer stays 8-byte aligned (its low 3
//! bits free for the value tag in [`crate::value`]).
//!
//! Stage 0 note: this defines the header shape and the local refcount
//! operations. Allocation of concrete kinds, the destructor dispatch, and the
//! collector's trial-deletion pass are wired in later stages; nothing
//! constructs an `ObjHeader` yet.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Runtime kind of a heap object. Stored in [`ObjHeader::kind`].
///
/// This is the *object* kind for heap-allocated payloads. It is distinct from
/// the low-bit value tag (which only distinguishes int/float/str/ptr/immediate)
/// — once a value is known to be a non-string pointer, this byte says which
/// kind of object it points at, replacing the VM's `VmValue` enum discriminant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ObjKind {
    /// A boxed `f64` (value kind `TAG_FLOAT`).
    Float = 0,
    /// A heap string (value kind `TAG_STR`); payload is the tagged char data.
    Str = 1,
    /// A growable array.
    Array = 2,
    /// A hash dict.
    Dict = 3,
    /// A struct instance (named type + flat field slots).
    Struct = 4,
    /// A function / closure value.
    Fn = 5,
    /// An async future handle.
    Future = 6,
    /// A prompt value.
    Prompt = 7,
    /// A GBNF grammar value.
    Grammar = 8,
    /// A method bound to a receiver (`let f = obj.greet`). Callable like a
    /// function value, but carries the receiver it will pass as `self`.
    BoundMethod = 9,
}

/// Bacon–Rajan cycle-collector color. Stored in [`ObjHeader::color`].
///
/// Semantics follow the classic synchronous trial-deletion algorithm:
///  * `Black`  — in active use (or free); the default for a live object.
///  * `Gray`   — provisionally visited during trial deletion.
///  * `White`  — proven garbage (member of a dead cycle), awaiting free.
///  * `Purple` — a candidate root: decref'd without reaching zero, so it might
///               be the entry point of a cycle. Purple objects are the roots
///               the collector scans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Gray = 1,
    White = 2,
    Purple = 3,
}

/// Flag bits for [`ObjHeader::flags`].
pub mod flags {
    /// The object is already buffered in the collector's candidate-root set,
    /// so a further decref must not enqueue it again.
    pub const BUFFERED: u8 = 1 << 0;
    /// The object lives in a bump arena (`jade_runtime::arena`): it is NOT
    /// reference-counted — `jrt_incref`/`jrt_decref` no-op on it — and is
    /// reclaimed only in bulk by `ArenaReset`, never by the collector's
    /// `free_obj`. This is what lets an arena pointer flow through refcounted
    /// registers safely: every rc op on it is a no-op, so it can never be freed
    /// out from under a live reference.
    pub const ARENA: u8 = 1 << 1;
}

/// The common header prefixing every reference-typed Jade heap object.
///
/// 16 bytes, 8-aligned. Fields are ordered for a compact, ABI-stable layout the
/// C runtime can `#include` a matching `struct` for.
///
/// ## Why the mutable fields are atomic
///
/// `jrt_spawn` hands a task's arguments — which include live `TAG_PTR`
/// collection pointers — to a real OS worker thread, and the heap is shared
/// rather than copied (matching the VM, where `Arc<Mutex<ArrayObj>>` passed
/// into a task is genuinely aliased). So two threads can reach the same header
/// concurrently. With a plain `u32` counter that is a lost update, then a
/// premature free, then a use-after-free — and the failure is invisible to the
/// stdout-diffing parity gate that guards everything else in this codebase.
///
/// `rc`, `color`, and `flags` are therefore atomic. `kind` is immutable after
/// construction and `len` is only touched under exclusive access to the
/// payload, so both stay plain. All three atomics are `repr(transparent)` over
/// their integer types, so the 16-byte C-ABI layout is unchanged — `runtime.h`
/// and codegen's offset-8 `ObjKind` read both still hold.
///
/// Note this makes the *header* sound, not the payload: `ArrayObj::data` is
/// still an unsynchronized `Vec` on the AOT path.
#[derive(Debug)]
#[repr(C, align(8))]
pub struct ObjHeader {
    /// Strong reference count. Reaches zero → object is destroyed and freed.
    pub rc: AtomicU32,
    /// Element/field count; meaning is kind-dependent (array length, struct
    /// field count, string byte length). `0` for kinds that don't need it.
    pub len: u32,
    /// Which kind of object follows this header (an [`ObjKind`]).
    pub kind: u8,
    /// Cycle-collector color (a [`Color`]).
    pub color: AtomicU8,
    /// Bitset of [`flags`].
    pub flags: AtomicU8,
    /// Reserved to keep the header 8-aligned and give the collector room to
    /// grow (e.g. a generation byte) without an ABI break.
    pub _reserved: u8,
    /// Reserved padding to a 16-byte, 8-aligned header.
    pub _reserved2: u32,
}

impl ObjHeader {
    /// A fresh header for `kind` with `len`, refcount 1, colored `Black`.
    #[inline]
    pub const fn new(kind: ObjKind, len: u32) -> Self {
        ObjHeader {
            rc: AtomicU32::new(1),
            len,
            kind: kind as u8,
            color: AtomicU8::new(Color::Black as u8),
            flags: AtomicU8::new(0),
            _reserved: 0,
            _reserved2: 0,
        }
    }

    /// Increment the strong count. A new reference to a live object also means
    /// it cannot currently be garbage, so it is recolored `Black`.
    ///
    /// Takes `&self`: increfs are the one header operation that genuinely races
    /// (two tasks aliasing the same array), and requiring `&mut` would be a lie
    /// about exclusivity. `Relaxed` suffices — an incref publishes nothing, it
    /// only has to not be lost, and the caller already holds a live reference
    /// that establishes the necessary happens-before.
    #[inline]
    pub fn incref(&self) {
        self.rc.fetch_add(1, Ordering::Relaxed);
        self.color.store(Color::Black as u8, Ordering::Relaxed);
    }

    /// Whether this object is arena-allocated (see [`flags::ARENA`]).
    #[inline]
    pub fn is_arena(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & flags::ARENA != 0
    }

    /// Mark this object as arena-allocated so the refcount ops no-op on it.
    #[inline]
    pub fn set_arena(&self) {
        self.flags.fetch_or(flags::ARENA, Ordering::Relaxed);
    }

    /// Decrement the strong count. Returns `true` when it reaches zero (the
    /// caller must then run the destructor and free the allocation). When it
    /// does *not* reach zero the object becomes a cycle-collection candidate;
    /// the caller is responsible for buffering it as a `Purple` root (guarded
    /// by [`flags::BUFFERED`]).
    ///
    /// The `Release` on the decrement and the `Acquire` fence before returning
    /// `true` are the standard `Arc` discipline: every thread's writes to the
    /// payload must be visible to whichever thread ends up running the
    /// destructor. Without the fence, the destructor could read a stale payload
    /// and free the wrong child pointers.
    ///
    /// Debug-asserts against decref of an already-dead object.
    #[inline]
    #[must_use = "a true result means the object must be destroyed and freed"]
    pub fn decref(&self) -> bool {
        let prev = self.rc.fetch_sub(1, Ordering::Release);
        debug_assert!(prev > 0, "decref on object with rc == 0 (double free)");
        if prev == 1 {
            core::sync::atomic::fence(Ordering::Acquire);
            return true;
        }
        false
    }

    /// The current strong count. For tests and the heap instrument only —
    /// a load is a snapshot and is stale the instant it returns.
    #[inline]
    pub fn rc(&self) -> u32 {
        self.rc.load(Ordering::Relaxed)
    }

    /// The current cycle-collector color.
    #[inline]
    pub fn color(&self) -> u8 {
        self.color.load(Ordering::Relaxed)
    }

    /// Whether this object is currently buffered as a candidate root.
    #[inline]
    pub fn is_buffered(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & flags::BUFFERED != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_16_bytes_8_aligned() {
        // ABI contract with the residual C runtime and the pointer-tag scheme.
        assert_eq!(core::mem::size_of::<ObjHeader>(), 16);
        assert_eq!(core::mem::align_of::<ObjHeader>(), 8);
    }

    #[test]
    fn new_header_is_live_and_black() {
        let h = ObjHeader::new(ObjKind::Array, 3);
        assert_eq!(h.rc(), 1);
        assert_eq!(h.len, 3);
        assert_eq!(h.kind, ObjKind::Array as u8);
        assert_eq!(h.color(), Color::Black as u8);
        assert!(!h.is_buffered());
    }

    #[test]
    fn incref_decref_reaches_zero() {
        let h = ObjHeader::new(ObjKind::Dict, 0);
        h.incref(); // rc = 2
        assert!(!h.decref()); // rc = 1
        assert!(h.decref()); // rc = 0 → destroy
    }

    #[test]
    fn incref_recolors_black() {
        let h = ObjHeader::new(ObjKind::Struct, 1);
        h.color.store(Color::Purple as u8, Ordering::Relaxed);
        h.incref();
        assert_eq!(h.color(), Color::Black as u8);
    }

    /// The reason `rc` is atomic. With a plain `u32` this loses updates and the
    /// final count lands below the true one — which in production means a
    /// premature free while another task still holds the object.
    #[test]
    fn incref_decref_is_race_free_across_threads() {
        const THREADS: usize = 8;
        const ITERS: usize = 20_000;

        let h = ObjHeader::new(ObjKind::Array, 0);
        std::thread::scope(|s| {
            for _ in 0..THREADS {
                s.spawn(|| {
                    for _ in 0..ITERS {
                        h.incref();
                        assert!(!h.decref(), "count must never reach zero: rc starts at 1");
                    }
                });
            }
        });

        // Every incref was matched by a decref, so the initial reference is all
        // that remains. A non-atomic counter fails this well below 1.
        assert_eq!(h.rc(), 1);
    }
}
