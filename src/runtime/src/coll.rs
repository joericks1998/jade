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

// ── Array ───────────────────────────────────────────────────────────────────

/// A growable, **reference-semantic** array (mutations are visible to all
/// aliases). Mirrors the VM's `Arc<Mutex<Vec<VmValue>>>` and the C `JKArray`.
#[repr(C)]
pub struct ArrayObj<T> {
    /// Kind = [`ObjKind::Array`]; `len` tracks `data.len()`.
    pub header: ObjHeader,
    data: Vec<T>,
}

impl<T: Clone> ArrayObj<T> {
    /// A fresh empty array.
    #[inline]
    pub fn new() -> Self {
        ArrayObj { header: ObjHeader::new(ObjKind::Array, 0), data: Vec::new() }
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

    /// Overwrite element `i` in place; `Err(())` if out of bounds.
    #[inline]
    pub fn set(&mut self, i: usize, v: T) -> Result<(), ()> {
        match self.data.get_mut(i) {
            Some(slot) => {
                *slot = v;
                Ok(())
            }
            None => Err(()),
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

impl<T: Clone> ArrayObj<T> {
    /// Build an array from an existing `Vec` (the VM's construction path). The
    /// header length is synced once; subsequent `DerefMut` mutations do not
    /// re-sync it (see the `Deref` note), which is fine because only the AOT
    /// side reads `header.len`, and AOT arrays are built through `push`.
    #[inline]
    pub fn from_vec(data: Vec<T>) -> Self {
        let mut a = ArrayObj { header: ObjHeader::new(ObjKind::Array, 0), data };
        a.sync_len();
        a
    }
}

impl<T: Clone> Default for ArrayObj<T> {
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
impl<T> core::ops::Deref for ArrayObj<T> {
    type Target = Vec<T>;
    #[inline]
    fn deref(&self) -> &Vec<T> {
        &self.data
    }
}

impl<T> core::ops::DerefMut for ArrayObj<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.data
    }
}

// ── Dict ──────────────────────────────────────────────────────────────────────

/// An insertion-ordered, string-keyed, **value-semantic** dict: assignment /
/// index-mutation clones the map (see [`value_copy`](DictObj::value_copy)), while
/// values are shared words. Mirrors the VM's `HashMap<String, VmValue>` (cloned
/// on mutation) and the C `JKDict` + `jk_dict_copy`.
///
/// Lookup is a linear probe over an insertion-ordered slot vector — dicts are
/// small, and preserving insertion order keeps `value_copy` output stable
/// (rendering sorts keys separately). Keys are unique; setting an existing key
/// updates in place.
#[repr(C)]
pub struct DictObj<T> {
    /// Kind = [`ObjKind::Dict`]; `len` tracks the entry count.
    pub header: ObjHeader,
    slots: Vec<(String, T)>,
}

impl<T: Clone> DictObj<T> {
    /// A fresh empty dict.
    #[inline]
    pub fn new() -> Self {
        DictObj { header: ObjHeader::new(ObjKind::Dict, 0), slots: Vec::new() }
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
        for (k, v) in &mut self.slots {
            if k == key {
                *v = val;
                return;
            }
        }
        self.slots.push((key.to_owned(), val));
        self.sync_len();
    }

    /// Value for `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<&T> {
        self.slots.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.slots.iter().any(|(k, _)| k == key)
    }

    /// Remove `key`, returning its value if it was present (preserving the order
    /// of the remaining entries).
    pub fn remove(&mut self, key: &str) -> Option<T> {
        if let Some(i) = self.slots.iter().position(|(k, _)| k == key) {
            let (_, v) = self.slots.remove(i);
            self.sync_len();
            Some(v)
        } else {
            None
        }
    }

    /// A shallow value-copy with a fresh header: keys are re-owned so the copies
    /// free independently, values are `Clone`d (sharing pointees). This is the
    /// VM's clone-on-mutation semantics — the caller rebinds the variable to the
    /// returned dict, leaving aliases of the original untouched.
    pub fn value_copy(&self) -> Self {
        let slots: Vec<(String, T)> =
            self.slots.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut d = DictObj { header: ObjHeader::new(ObjKind::Dict, 0), slots };
        d.sync_len();
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
        for (k, v) in &mut self.slots {
            if *k == key {
                return Some(core::mem::replace(v, val));
            }
        }
        self.slots.push((key, val));
        self.sync_len();
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

impl<T: Clone> Default for DictObj<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Cloning a dict deep-copies it with a fresh header — the value semantics the VM
/// relies on (a dict assignment / mutation must not alias the source). Values are
/// `Clone`d, so `Arc`-backed elements stay shared, matching the former
/// `HashMap<String, VmValue>` clone.
impl<T: Clone> Clone for DictObj<T> {
    #[inline]
    fn clone(&self) -> Self {
        self.value_copy()
    }
}

impl<T: Clone> FromIterator<(String, T)> for DictObj<T> {
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
#[repr(C)]
pub struct StructObj<T> {
    /// Kind = [`ObjKind::Struct`]; `len` tracks the field count.
    pub header: ObjHeader,
    type_name: String,
    fields: Vec<(String, T)>,
}

impl<T: Clone> StructObj<T> {
    /// A fresh struct of the given type with no fields yet.
    #[inline]
    pub fn new(type_name: &str) -> Self {
        StructObj {
            header: ObjHeader::new(ObjKind::Struct, 0),
            type_name: type_name.to_owned(),
            fields: Vec::new(),
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
        assert_eq!(d2.header.rc, 1);
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
