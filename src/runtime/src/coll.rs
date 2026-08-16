//! The shared heap-collection payloads: array, dict, and struct.
//!
//! One implementation of each, used by **both** engines. It replaces three
//! former copies — the VM's `Arc`-backed `VmValue::Array/Dict/Struct`, the C
//! runtime's legacy `JadeDict`/`jrt_array_*`, and the C "Chunk backend"
//! kind-tagged `JK*` objects.
//!
//! ## One representation, two element types
//!
//! Each payload is generic over the element **word type** `T`:
//!
//!  * the AOT backend instantiates `T = i64` — a tagged [`crate::value::JadeValue`]
//!    word (matching the C `int64_t` element ABI);
//!  * the VM instantiates `T = VmValue` — its native fat enum.
//!
//! The value/reference semantics fall out of `T: Clone` **for free**, which is
//! the point of the generic design:
//!
//!  * cloning an `i64` word is a shallow copy that *shares* whatever heap object
//!    the word points at (matching the C runtime's "values are shared words");
//!  * cloning a `VmValue` clones its inner `Arc`, likewise *sharing* the pointee
//!    (matching the VM's shallow dict copy).
//!
//! So [`DictObj::value_copy`] reproduces the VM's clone-on-mutation dict
//! semantics identically under either instantiation, with no per-engine code.
//!
//! ## Backing allocator
//!
//! Each payload is *also* generic over an **allocator** `A` (defaulting to the
//! global allocator [`Global`]). The backing store is an
//! [`allocator_api2::vec::Vec`], which — because [`ArenaAlloc`] is zero-sized —
//! is layout-identical to a `Vec<T>`. This lets a collection the compiler proves
//! does not escape live in a per-frame bump arena
//! ([`crate::arena::ArenaAlloc`]) while staying byte-compatible with the heap
//! form, so the C-ABI accessors (`jrt_coll_*`) read either without change. The
//! default `Global` case behaves exactly as a plain `Vec<T>` did.
//!
//! [`ArenaAlloc`]: crate::arena::ArenaAlloc
//!
//! ## Layout
//!
//! Every payload is `#[repr(C)]` with an [`ObjHeader`] at offset 0, so the AOT
//! side can read an object's [`ObjKind`]/length through a `*const ObjHeader`
//! without knowing the concrete payload type. `header.len` is kept in sync with
//! the live element count so a `jrt_len` accessor can read it in O(1). The VM
//! never relies on the C-ABI offset (it reaches fields through the Rust API);
//! the header's refcount/color are inert until the cycle collector is wired
//! (a later brick) — the VM keeps using `Arc` for lifetime.
//!
//! Keys (dict) and the type name (struct) are stored as owned byte buffers with
//! no trust tag, matching both prior engines: the VM's `HashMap<String, _>` and
//! the C runtime's plain `char*` keys neither carry nor compare key trust.

use crate::heap::{ObjHeader, ObjKind};
use allocator_api2::alloc::{Allocator, Global};
use allocator_api2::vec::Vec as AVec;

// ── Array ───────────────────────────────────────────────────────────────────

/// A growable, **reference-semantic** array (mutations are visible to all
/// aliases). Mirrors the VM's `Arc<Mutex<Vec<VmValue>>>` and the C `JKArray`.
///
/// Generic over the backing allocator `A` (defaults to [`Global`]); an
/// arena-backed instance is built with [`ArrayObj::new_in`].
#[repr(C)]
pub struct ArrayObj<T, A: Allocator = Global> {
    /// Kind = [`ObjKind::Array`]; `len` tracks `data.len()`.
    pub header: ObjHeader,
    data: AVec<T, A>,
}

/// `set` was given an index past the end of the collection.
///
/// A named type rather than `Err(())`, which says a call failed and nothing
/// about why — every caller has to read the function to find out which failure
/// it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBounds;

impl<T: Clone, A: Allocator> ArrayObj<T, A> {
    /// A fresh empty array backed by `alloc`.
    #[inline]
    pub fn new_in(alloc: A) -> Self {
        ArrayObj { header: ObjHeader::new(ObjKind::Array, 0), data: AVec::new_in(alloc) }
    }

    /// Append an element (grows in place; reference semantics).
    #[inline]
    pub fn push(&mut self, v: T) {
        self.data.push(v);
        self.sync_len();
    }

    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Element at `i`, or `None` if out of bounds. Negative indices are the
    /// caller's responsibility (the VM/AOT reject them before calling).
    #[inline]
    pub fn get(&self, i: usize) -> Option<&T> {
        self.data.get(i)
    }

    /// Remove and return the last element (syncing the header length), or `None`
    /// if empty. Matches the VM's `array.pop` (nil on empty).
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        let r = self.data.pop();
        if r.is_some() {
            self.sync_len();
        }
        r
    }

    /// Overwrite element `i` in place, or report that `i` is past the end.
    #[inline]
    pub fn set(&mut self, i: usize, v: T) -> Result<(), OutOfBounds> {
        match self.data.get_mut(i) {
            Some(slot) => {
                *slot = v;
                Ok(())
            }
            None => Err(OutOfBounds),
        }
    }

    /// Read-only view of the backing slice (for rendering/iteration).
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    #[inline]
    fn sync_len(&mut self) {
        self.header.len = self.data.len() as u32;
    }
}

impl<T: Clone> ArrayObj<T, Global> {
    /// A fresh empty array (heap-allocated).
    #[inline]
    pub fn new() -> Self {
        ArrayObj { header: ObjHeader::new(ObjKind::Array, 0), data: AVec::new() }
    }

    /// Build an array from an existing (heap) `Vec` (the VM's construction
    /// path). The header length is synced once; subsequent `DerefMut` mutations
    /// do not re-sync it (see the `Deref` note), which is fine because only the
    /// AOT side reads `header.len`, and AOT arrays are built through `push`.
    #[inline]
    pub fn from_vec(data: std::vec::Vec<T>) -> Self {
        let mut a = ArrayObj {
            header: ObjHeader::new(ObjKind::Array, 0),
            data: data.into_iter().collect(),
        };
        a.sync_len();
        a
    }
}

impl<T: Clone> Default for ArrayObj<T, Global> {
    fn default() -> Self {
        Self::new()
    }
}

/// `Deref`/`DerefMut` to the backing `Vec` so the VM can treat an `ArrayObj`
/// exactly like the `Vec<VmValue>` it replaced — `push`, `iter`, indexing,
/// `sort_by`, etc. all resolve through here with no per-call-site churn.
/// NB: mutating through `DerefMut` does **not** keep `header.len` in sync; that
/// field is an AOT-only fast path (`jrt_coll_len`) and VM arrays never reach it.
/// `ArrayObj` deliberately does not implement `Clone`, so `guard.clone()` resolves
/// to `Vec::clone` here — matching the pre-migration `Arc<Mutex<Vec>>` behavior.
impl<T, A: Allocator> core::ops::Deref for ArrayObj<T, A> {
    type Target = AVec<T, A>;
    #[inline]
    fn deref(&self) -> &AVec<T, A> {
        &self.data
    }
}

impl<T, A: Allocator> core::ops::DerefMut for ArrayObj<T, A> {
    #[inline]
    fn deref_mut(&mut self) -> &mut AVec<T, A> {
        &mut self.data
    }
}

// ── Dict ──────────────────────────────────────────────────────────────────────

/// An insertion-ordered, string-keyed, **value-semantic** dict: assignment /
/// index-mutation clones the map (see [`value_copy`](DictObj::value_copy)), while
/// values are shared words. Mirrors the VM's `HashMap<String, VmValue>` (cloned
/// on mutation) and the C `JKDict` + `jk_dict_copy`.
///
/// **A compact hash map.** Entries live in one insertion-ordered vector, and a
/// separate open-addressed table maps a key's hash to its position in it. So a
/// lookup is O(1) expected, insertion order is still what `entries()` hands
/// back, and `value_copy` output stays stable (rendering sorts keys separately).
/// Keys are unique; setting an existing key updates in place.
///
/// It was a bare slot vector until v1.3.22, searched by linear scan. That made
/// every `get` and every `set` O(n) and therefore building a dict O(n²) — 4,000
/// keys took seconds against a rounding error for the same number of array
/// pushes. Nothing about a dict's behaviour depended on the scan, which is why
/// this could change underneath both engines at once.
///
/// **Small dicts skip the index entirely.** Below [`DICT_SCAN_MAX`] a scan of a
/// contiguous vector beats hashing, and most dicts in real programs are that
/// size — a config, an options bag, an HTTP header set. The index is built the
/// first time a dict grows past it, so nothing pays for a table it would not
/// use.
///
/// Generic over the backing allocator `A` (defaults to [`Global`]); an
/// arena-backed instance is built with [`DictObj::new_in`].
#[repr(C)]
pub struct DictObj<T, A: Allocator = Global> {
    /// Kind = [`ObjKind::Dict`]; `len` tracks the entry count.
    pub header: ObjHeader,
    /// The entries, in insertion order. `entries()` hands this out directly.
    slots: AVec<(String, T), A>,
    /// Open-addressed table of positions in `slots`, or empty while the dict is
    /// small enough to scan. Length is always a power of two so the probe can
    /// mask rather than divide; [`EMPTY_SLOT`] marks a free bucket.
    index: AVec<u32, A>,
}

/// Entry count below which a linear scan beats hashing, so no index is built.
pub const DICT_SCAN_MAX: usize = 8;

/// A free bucket in the index. `u32::MAX` rather than a sentinel of its own:
/// a dict with 4 billion entries is not the case being optimised for, and one
/// this big would have exhausted memory long before.
const EMPTY_SLOT: u32 = u32::MAX;

/// FNV-1a over the key's bytes.
///
/// Deliberately not `std`'s `RandomState`: the two engines share this file, a
/// dict's iteration order is its insertion order, and `keys()` sorts — so the
/// hash never reaches anything observable, and a small deterministic one keeps
/// the runtime free of a `std::collections` dependency it does not otherwise
/// need.
fn hash_key(key: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

impl<T: Clone, A: Allocator + Clone> DictObj<T, A> {
    /// A fresh empty dict backed by `alloc`.
    #[inline]
    pub fn new_in(alloc: A) -> Self {
        DictObj {
            header: ObjHeader::new(ObjKind::Dict, 0),
            slots: AVec::new_in(alloc.clone()),
            index: AVec::new_in(alloc),
        }
    }

    // ── The index ─────────────────────────────────────────────────────────────

    /// Position of `key` in `slots`, or `None`.
    ///
    /// Scans while the dict is small and probes once it is not. Both answer the
    /// same question; only the cost differs.
    fn find(&self, key: &str) -> Option<usize> {
        if self.index.is_empty() {
            return self.slots.iter().position(|(k, _)| k == key);
        }
        let mask = self.index.len() - 1;
        let mut i = (hash_key(key) as usize) & mask;
        // Terminates because the table is never allowed to fill: `reindex`
        // keeps it at least twice the entry count, so an empty bucket exists.
        loop {
            let slot = self.index[i];
            if slot == EMPTY_SLOT {
                return None;
            }
            if self.slots[slot as usize].0 == key {
                return Some(slot as usize);
            }
            i = (i + 1) & mask;
        }
    }

    /// Rebuild the index from `slots`, sized for the current entry count.
    ///
    /// Called after any change that moves entries — a growth past the scan
    /// threshold, or a `remove`, which shifts every position after it. Rebuilding
    /// rather than patching is right for `remove` specifically: the shift already
    /// costs O(n), so a tombstone would buy nothing and would need compaction of
    /// its own.
    fn reindex(&mut self) {
        if self.slots.len() <= DICT_SCAN_MAX {
            self.index.clear();
            return;
        }
        // Load factor 0.5: enough headroom that linear probing stays short.
        let cap = (self.slots.len() * 2).next_power_of_two();
        self.index.clear();
        self.index.resize(cap, EMPTY_SLOT);
        let mask = cap - 1;
        for (pos, (k, _)) in self.slots.iter().enumerate() {
            let mut i = (hash_key(k) as usize) & mask;
            while self.index[i] != EMPTY_SLOT {
                i = (i + 1) & mask;
            }
            self.index[i] = pos as u32;
        }
    }

    /// Record a newly appended entry, growing the index if it is filling up.
    fn index_appended(&mut self) {
        self.sync_len();
        if self.index.is_empty() || self.slots.len() * 2 > self.index.len() {
            self.reindex();
            return;
        }
        let mask = self.index.len() - 1;
        let pos = self.slots.len() - 1;
        let mut i = (hash_key(&self.slots[pos].0) as usize) & mask;
        while self.index[i] != EMPTY_SLOT {
            i = (i + 1) & mask;
        }
        self.index[i] = pos as u32;
    }

    /// Number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Set `key → val`, updating in place if the key is present, else appending
    /// (unique keys, insertion order preserved). The key is copied.
    pub fn set(&mut self, key: &str, val: T) {
        if let Some(i) = self.find(key) {
            self.slots[i].1 = val;
            return;
        }
        self.slots.push((key.to_owned(), val));
        self.index_appended();
    }

    /// Value for `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<&T> {
        self.find(key).map(|i| &self.slots[i].1)
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.find(key).is_some()
    }

    /// Remove `key`, returning its value if it was present (preserving the order
    /// of the remaining entries).
    pub fn remove(&mut self, key: &str) -> Option<T> {
        let i = self.find(key)?;
        let (_, v) = self.slots.remove(i);
        self.sync_len();
        // Every position after `i` just moved, so the index is stale wholesale.
        self.reindex();
        Some(v)
    }

    /// A shallow value-copy with a fresh header: keys are re-owned so the copies
    /// free independently, values are `Clone`d (sharing pointees). This is the
    /// VM's clone-on-mutation semantics — the caller rebinds the variable to the
    /// returned dict, leaving aliases of the original untouched.
    ///
    /// The copy is always [`Global`]-allocated regardless of `A`: a value-copy
    /// escapes by definition (the caller rebinds it and it outlives the current
    /// region), so it must not stay in an arena.
    pub fn value_copy(&self) -> DictObj<T, Global> {
        let slots: AVec<(String, T), Global> =
            self.slots.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut d = DictObj { header: ObjHeader::new(ObjKind::Dict, 0), slots, index: AVec::new() };
        d.sync_len();
        // Cheaper than copying the table, and the copy is already O(n).
        d.reindex();
        d
    }

    /// Read-only view of the entries in insertion order (for rendering/iteration).
    #[inline]
    pub fn entries(&self) -> &[(String, T)] {
        &self.slots
    }

    #[inline]
    fn sync_len(&mut self) {
        self.header.len = self.slots.len() as u32;
    }

    // ── `HashMap`-compatible surface ──────────────────────────────────────────
    // These mirror the `std::collections::HashMap` methods the VM previously used
    // on `VmValue::Dict`, so the migration to `DictObj` is minimal-churn.

    /// Insert `key → val` (HashMap-compatible): returns the previous value if the
    /// key was present, else `None`. Accepts an owned `String` or a `&str`.
    pub fn insert(&mut self, key: impl Into<String>, val: T) -> Option<T> {
        let key = key.into();
        if let Some(i) = self.find(&key) {
            return Some(core::mem::replace(&mut self.slots[i].1, val));
        }
        self.slots.push((key, val));
        self.index_appended();
        None
    }

    /// HashMap-compatible alias for [`contains`](DictObj::contains).
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.contains(key)
    }

    /// Iterator over the keys in insertion order.
    #[inline]
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.slots.iter().map(|(k, _)| k)
    }

    /// Iterator over the values in insertion order.
    #[inline]
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().map(|(_, v)| v)
    }

    /// Iterator over `(&key, &value)` pairs in insertion order (HashMap-like).
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&String, &T)> {
        self.slots.iter().map(|(k, v)| (k, v))
    }
}

impl<T: Clone> DictObj<T, Global> {
    /// A fresh empty dict (heap-allocated).
    #[inline]
    pub fn new() -> Self {
        DictObj { header: ObjHeader::new(ObjKind::Dict, 0), slots: AVec::new(), index: AVec::new() }
    }
}

impl<T: Clone> Default for DictObj<T, Global> {
    fn default() -> Self {
        Self::new()
    }
}

/// Cloning a dict deep-copies it with a fresh header — the value semantics the VM
/// relies on (a dict assignment / mutation must not alias the source). Values are
/// `Clone`d, so `Arc`-backed elements stay shared, matching the former
/// `HashMap<String, VmValue>` clone.
///
/// Only heap ([`Global`]) dicts are `Clone` — a clone escapes, so it must not be
/// arena-backed; see [`value_copy`](DictObj::value_copy).
impl<T: Clone> Clone for DictObj<T, Global> {
    #[inline]
    fn clone(&self) -> Self {
        self.value_copy()
    }
}

impl<T: Clone> FromIterator<(String, T)> for DictObj<T, Global> {
    fn from_iter<I: IntoIterator<Item = (String, T)>>(iter: I) -> Self {
        let mut d = DictObj::new();
        for (k, v) in iter {
            d.insert(k, v);
        }
        d
    }
}

// ── Struct ────────────────────────────────────────────────────────────────────

/// A named struct instance: a type name plus **reference-semantic** named
/// fields (field assignment mutates in place). Mirrors the VM's
/// `Arc<Mutex<VmStruct>>` and the C `JKStruct`.
///
/// Generic over the backing allocator `A` (defaults to [`Global`]); an
/// arena-backed instance is built with [`StructObj::new_in`].
#[repr(C)]
pub struct StructObj<T, A: Allocator = Global> {
    /// Kind = [`ObjKind::Struct`]; `len` tracks the field count.
    pub header: ObjHeader,
    type_name: String,
    fields: AVec<(String, T), A>,
}

impl<T: Clone, A: Allocator> StructObj<T, A> {
    /// A fresh struct of the given type with no fields yet, backed by `alloc`.
    #[inline]
    pub fn new_in(type_name: &str, alloc: A) -> Self {
        StructObj {
            header: ObjHeader::new(ObjKind::Struct, 0),
            type_name: type_name.to_owned(),
            fields: AVec::new_in(alloc),
        }
    }

    /// The struct's type name (for `GetTypeName` / typed `catch`).
    #[inline]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Number of fields.
    #[inline]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Set `field → val`, updating in place if present else appending
    /// (definition order). Reference semantics — no copy.
    pub fn set_field(&mut self, field: &str, val: T) {
        for (k, v) in &mut self.fields {
            if k == field {
                *v = val;
                return;
            }
        }
        self.fields.push((field.to_owned(), val));
        self.sync_len();
    }

    /// Value of `field`, or `None` if the struct has no such field.
    pub fn get_field(&self, field: &str) -> Option<&T> {
        self.fields.iter().find(|(k, _)| k == field).map(|(_, v)| v)
    }

    /// Read-only view of the fields in definition order.
    #[inline]
    pub fn fields(&self) -> &[(String, T)] {
        &self.fields
    }

    #[inline]
    fn sync_len(&mut self) {
        self.header.len = self.fields.len() as u32;
    }
}

impl<T: Clone> StructObj<T, Global> {
    /// A fresh struct of the given type with no fields yet (heap-allocated).
    #[inline]
    pub fn new(type_name: &str) -> Self {
        StructObj {
            header: ObjHeader::new(ObjKind::Struct, 0),
            type_name: type_name.to_owned(),
            fields: AVec::new(),
        }
    }
}

#[cfg(test)]
mod dict_index_tests {
    use super::*;

    /// The index has to agree with a scan for every key, including keys that
    /// are absent — a probe that walked past its run would answer `None` for a
    /// key that is present, and the dict would silently lose entries.
    #[test]
    fn the_index_agrees_with_a_scan_across_the_threshold() {
        let mut d: DictObj<i64> = DictObj::new();
        for i in 0..(DICT_SCAN_MAX as i64 * 40) {
            d.insert(format!("k{i}"), i);
            assert_eq!(d.len(), (i + 1) as usize);
            // Every key inserted so far is still findable, and nothing else is.
            for j in 0..=i {
                assert_eq!(d.get(&format!("k{j}")), Some(&j), "lost k{j} after inserting k{i}");
            }
            assert_eq!(d.get("absent"), None);
        }
    }

    /// Entries stay in insertion order whether or not an index exists — that is
    /// what `entries()` promises and what rendering and `value_copy` rely on.
    #[test]
    fn insertion_order_survives_the_index() {
        let mut d: DictObj<i64> = DictObj::new();
        let n = DICT_SCAN_MAX * 3;
        for i in 0..n {
            d.insert(format!("k{i}"), i as i64);
        }
        let order: Vec<&str> = d.entries().iter().map(|(k, _)| k.as_str()).collect();
        let want: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
        assert_eq!(order, want.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }

    /// A remove shifts every later entry, so the index is rebuilt. If it were
    /// not, the stale positions would read the wrong key's value.
    #[test]
    fn remove_keeps_the_rest_findable() {
        let mut d: DictObj<i64> = DictObj::new();
        let n = DICT_SCAN_MAX * 4;
        for i in 0..n {
            d.insert(format!("k{i}"), i as i64);
        }
        for i in (0..n).step_by(3) {
            assert_eq!(d.remove(&format!("k{i}")), Some(i as i64));
        }
        for i in 0..n {
            let want = if i % 3 == 0 { None } else { Some(i as i64) };
            assert_eq!(d.get(&format!("k{i}")), want.as_ref(), "wrong answer for k{i}");
        }
    }

    /// Overwriting a key updates in place and does not grow the dict — the path
    /// that finds an existing key has to be the same one `get` uses.
    #[test]
    fn overwriting_a_key_updates_in_place() {
        let mut d: DictObj<i64> = DictObj::new();
        let n = DICT_SCAN_MAX * 5;
        for i in 0..n {
            d.insert(format!("k{i}"), i as i64);
        }
        for i in 0..n {
            assert_eq!(d.insert(format!("k{i}"), -(i as i64)), Some(i as i64));
        }
        assert_eq!(d.len(), n);
        for i in 0..n {
            assert_eq!(d.get(&format!("k{i}")), Some(&-(i as i64)));
        }
    }

    /// A copy is an independent dict with a working index of its own.
    #[test]
    fn value_copy_carries_a_working_index() {
        let mut d: DictObj<i64> = DictObj::new();
        let n = DICT_SCAN_MAX * 3;
        for i in 0..n {
            d.insert(format!("k{i}"), i as i64);
        }
        let mut c = d.value_copy();
        c.insert("k0".to_string(), 999);
        assert_eq!(c.get("k0"), Some(&999));
        assert_eq!(d.get("k0"), Some(&0), "the copy must not write through to the source");
        for i in 1..n {
            assert_eq!(c.get(&format!("k{i}")), Some(&(i as i64)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tests instantiate `T = i64` (the AOT element word). Value semantics
    // are exercised through `Clone`; for `i64` a clone is a shallow word copy.

    #[test]
    fn header_offset_zero_and_kind() {
        // The AOT side reads kind/len through a *const ObjHeader at offset 0.
        let a = ArrayObj::<i64>::new();
        let p = &a as *const ArrayObj<i64> as *const ObjHeader;
        unsafe {
            assert_eq!((*p).kind, ObjKind::Array as u8);
            assert_eq!((*p).len, 0);
        }
    }

    #[test]
    fn array_push_get_set_and_len_sync() {
        let mut a = ArrayObj::<i64>::new();
        a.push(10);
        a.push(20);
        a.push(30);
        assert_eq!(a.len(), 3);
        assert_eq!(a.header.len, 3); // header kept in sync for jrt_len
        assert_eq!(a.get(1), Some(&20));
        assert_eq!(a.get(3), None); // out of bounds
        assert!(a.set(1, 99).is_ok());
        assert_eq!(a.get(1), Some(&99));
        assert!(a.set(5, 0).is_err()); // out of bounds
    }

    #[test]
    fn dict_set_updates_in_place_and_appends() {
        let mut d = DictObj::<i64>::new();
        d.set("a", 1);
        d.set("b", 2);
        d.set("a", 100); // update, not append
        assert_eq!(d.len(), 2);
        assert_eq!(d.header.len, 2);
        assert_eq!(d.get("a"), Some(&100));
        assert_eq!(d.get("b"), Some(&2));
        assert_eq!(d.get("missing"), None);
        assert!(d.contains("a"));
        assert_eq!(d.remove("a"), Some(100));
        assert_eq!(d.len(), 1);
        assert_eq!(d.get("a"), None);
    }

    #[test]
    fn dict_value_copy_is_independent() {
        // VM clone-on-mutation: mutating the copy must not touch the original.
        let mut d = DictObj::<i64>::new();
        d.set("k", 1);
        let mut d2 = d.value_copy();
        d2.set("k", 999);
        d2.set("new", 7);
        assert_eq!(d.get("k"), Some(&1)); // original unchanged
        assert_eq!(d.get("new"), None);
        assert_eq!(d2.get("k"), Some(&999));
        assert_eq!(d2.get("new"), Some(&7));
        // A fresh header on the copy (rc back to 1, live).
        assert_eq!(d2.header.rc(), 1);
    }

    #[test]
    fn struct_fields_and_type_name() {
        let mut s = StructObj::<i64>::new("Point");
        s.set_field("x", 3);
        s.set_field("y", 4);
        s.set_field("x", 30); // update in place
        assert_eq!(s.type_name(), "Point");
        assert_eq!(s.len(), 2);
        assert_eq!(s.header.len, 2);
        assert_eq!(s.get_field("x"), Some(&30));
        assert_eq!(s.get_field("y"), Some(&4));
        assert_eq!(s.get_field("z"), None);
    }
}

// ── Generator buffers ─────────────────────────────────────────────────────────
//
// A `yield`ing function fills a buffer and hands it back; a stream *is* that
// buffer. The buffer is an ordinary array, so `len`, indexing, `for`, and
// printing over a stream reuse everything arrays already do.
//
// A stack rather than a single slot, because a generator can call another
// generator and each `yield` must land in its own function's buffer. Mirrors
// `VmState::yield_stack` in the interpreter.

use core::cell::RefCell;

thread_local! {
    static YIELD_STACK: RefCell<Vec<*mut core::ffi::c_void>> = const { RefCell::new(Vec::new()) };
}

/// Begin a generator frame.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_yield_push() {
    let arr = crate::ffi_coll::jrt_karr_new();
    YIELD_STACK.with(|s| s.borrow_mut().push(arr));
}

/// Append one yielded value to the innermost generator's buffer.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_yield_append(val: i64) {
    YIELD_STACK.with(|s| {
        if let Some(&arr) = s.borrow().last() {
            crate::ffi_coll::jrt_karr_push(arr, val);
        }
    });
}

/// End the innermost generator frame, returning its buffer as a tagged array.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_yield_pop() -> i64 {
    YIELD_STACK.with(|s| match s.borrow_mut().pop() {
        Some(arr) => crate::value::JadeValue::from_ptr(arr as *const ()).bits() as i64,
        // Unreachable from source: a generator always pushes before its body.
        None => crate::value::NIL_BITS as i64,
    })
}
