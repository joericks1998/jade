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
}

/// The common header prefixing every reference-typed Jade heap object.
///
/// 16 bytes, 8-aligned. Fields are ordered for a compact, ABI-stable layout the
/// C runtime can `#include` a matching `struct` for.
#[derive(Debug)]
#[repr(C, align(8))]
pub struct ObjHeader {
    /// Strong reference count. Reaches zero → object is destroyed and freed.
    pub rc: u32,
    /// Element/field count; meaning is kind-dependent (array length, struct
    /// field count, string byte length). `0` for kinds that don't need it.
    pub len: u32,
    /// Which kind of object follows this header (an [`ObjKind`]).
    pub kind: u8,
    /// Cycle-collector color (a [`Color`]).
    pub color: u8,
    /// Bitset of [`flags`].
    pub flags: u8,
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
            rc: 1,
            len,
            kind: kind as u8,
            color: Color::Black as u8,
            flags: 0,
            _reserved: 0,
            _reserved2: 0,
        }
    }

    /// Increment the strong count. A new reference to a live object also means
    /// it cannot currently be garbage, so it is recolored `Black`.
    #[inline]
    pub fn incref(&mut self) {
        self.rc += 1;
        self.color = Color::Black as u8;
    }

    /// Decrement the strong count. Returns `true` when it reaches zero (the
    /// caller must then run the destructor and free the allocation). When it
    /// does *not* reach zero the object becomes a cycle-collection candidate;
    /// the caller is responsible for buffering it as a `Purple` root (guarded
    /// by [`flags::BUFFERED`]).
    ///
    /// Debug-asserts against decref of an already-dead object.
    #[inline]
    #[must_use = "a true result means the object must be destroyed and freed"]
    pub fn decref(&mut self) -> bool {
        debug_assert!(self.rc > 0, "decref on object with rc == 0 (double free)");
        self.rc -= 1;
        self.rc == 0
    }

    /// Whether this object is currently buffered as a candidate root.
    #[inline]
    pub fn is_buffered(&self) -> bool {
        self.flags & flags::BUFFERED != 0
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
        assert_eq!(h.rc, 1);
        assert_eq!(h.len, 3);
        assert_eq!(h.kind, ObjKind::Array as u8);
        assert_eq!(h.color, Color::Black as u8);
        assert!(!h.is_buffered());
    }

    #[test]
    fn incref_decref_reaches_zero() {
        let mut h = ObjHeader::new(ObjKind::Dict, 0);
        h.incref(); // rc = 2
        assert!(!h.decref()); // rc = 1
        assert!(h.decref()); // rc = 0 → destroy
    }

    #[test]
    fn incref_recolors_black() {
        let mut h = ObjHeader::new(ObjKind::Struct, 1);
        h.color = Color::Purple as u8;
        h.incref();
        assert_eq!(h.color, Color::Black as u8);
    }
}
