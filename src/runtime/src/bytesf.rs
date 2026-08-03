//! `BytesObj` — a binary blob on the heap, shared by both engines.
//!
//! A `bytes` value is a counted sequence of raw octets. It is deliberately *not*
//! a string: Jade strings are UTF-8 and NUL-terminated, and neither holds for
//! arbitrary bytes. A PNG contains NUL bytes and byte sequences that are not
//! valid UTF-8, so storing one in a `str` would truncate it at the first NUL and
//! corrupt it on any operation that assumes valid UTF-8. Conversion between the
//! two is explicit in both directions (`encode` / `decode`), never implicit.
//!
//! ## Trust
//!
//! `BytesObj` carries a trust byte, exactly as [`crate::trust::JStr`] does, and
//! for the same reason: `fs.read_bytes` returns data from outside the program.
//! Without it, `fs.read_bytes(p).decode()` would hand back a *clean* string and
//! walk straight past the check in `sh.exec` that `fs.read(p)` cannot. The trust
//! byte lives in the object rather than at `data[-1]` the way a string's does,
//! because a bytes payload has no reserved prefix byte to hide one in.
//!
//! ## Reclamation
//!
//! The payload is a `Vec<u8>` — *not* tagged words. So the `free_obj` arm
//! reclaims the block with no child cascade, the way [`crate::task::FutureObj`]
//! does and unlike `ArrayObj`, whose arm decrefs every element. Getting those
//! two the wrong way round is a double free in one direction and a leak in the
//! other, which is why they are named here.

use core::ffi::c_void;

use crate::heap::{ObjHeader, ObjKind};
use crate::trust::TRUSTED;

/// A binary blob: a header, a trust byte, and owned octets.
///
/// `repr(C)` and header-first like every other kind — `gc::free_obj` and the
/// refcount ops read the kind byte at offset 8 before they know what they are
/// looking at.
#[repr(C)]
pub struct BytesObj {
    /// Kind = [`ObjKind::Bytes`].
    pub header: ObjHeader,
    /// Provenance, not identity: two blobs with the same octets are equal
    /// whatever they were derived from.
    pub trust: u8,
    /// The octets. Owned outright; there are no tagged child words here.
    pub data: Vec<u8>,
}

impl BytesObj {
    pub fn new(data: Vec<u8>, trust: u8) -> Self {
        let len = data.len();
        BytesObj { header: ObjHeader::new(ObjKind::Bytes, len as u32), trust, data }
    }

    /// A blob built by the program itself.
    pub fn trusted(data: Vec<u8>) -> Self {
        Self::new(data, TRUSTED)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn is_tainted(&self) -> bool {
        crate::trust::is_tainted(self.trust)
    }
}

impl PartialEq for BytesObj {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}
impl Eq for BytesObj {}

impl core::fmt::Debug for BytesObj {
    /// Length and trust, never the payload: a blob can be megabytes, and a
    /// panic message that dumps a file is not a useful panic message.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BytesObj({} byte(s), trust={})", self.data.len(), self.trust)
    }
}

// ── C ABI ─────────────────────────────────────────────────────────────────────

/// Allocate a bytes value from `len` octets at `src`. Returns the raw pointer;
/// codegen tags it `TAG_PTR`.
///
/// # Safety
/// `src` must point at `len` readable bytes, or be null when `len` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_new(src: *const u8, len: usize, trust: u8) -> *mut c_void {
    let data = if src.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { core::slice::from_raw_parts(src, len) }.to_vec()
    };
    crate::gc::leak_obj(BytesObj::new(data, trust))
}

/// Octet count.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_len(p: *const c_void) -> i64 {
    unsafe { (*(p as *const BytesObj)).data.len() as i64 }
}

/// The `i`-th octet as a plain `i64` in 0..=255, or -1 if out of range.
///
/// An octet is an *int*, not a char: a byte is not a Unicode scalar, and
/// pretending otherwise would make `b[0]` and `s[0]` look interchangeable when
/// they mean different things on any non-ASCII input.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_get(p: *const c_void, i: i64) -> i64 {
    let b = unsafe { &*(p as *const BytesObj) };
    if i < 0 || i as usize >= b.data.len() {
        return -1;
    }
    b.data[i as usize] as i64
}

/// Borrow the payload. Valid until the object is freed.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_data(p: *const c_void) -> *const u8 {
    unsafe { (*(p as *const BytesObj)).data.as_ptr() }
}

/// The trust byte a blob is carrying.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_trust(p: *const c_void) -> u8 {
    unsafe { (*(p as *const BytesObj)).trust }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_carries_its_kind_and_length() {
        let b = BytesObj::trusted(vec![1, 2, 3]);
        assert_eq!(b.header.kind, ObjKind::Bytes as u8);
        assert_eq!(b.len(), 3);
        assert!(!b.is_tainted());
    }

    /// The whole reason `bytes` is not modelled as a string: a NUL terminates a
    /// Jade string, and 0xFF is not valid UTF-8.
    #[test]
    fn a_blob_survives_a_nul_byte_and_invalid_utf8() {
        let raw = vec![b'a', 0, b'b', 0xFF, 0xFE];
        let b = BytesObj::trusted(raw.clone());
        assert_eq!(b.len(), 5, "a NUL must not terminate the payload");
        assert_eq!(b.as_slice(), &raw[..]);
        assert!(core::str::from_utf8(b.as_slice()).is_err());
    }

    #[test]
    fn equality_ignores_trust() {
        let a = BytesObj::new(vec![7, 8], crate::trust::TRUSTED);
        let b = BytesObj::new(vec![7, 8], crate::trust::TAINTED);
        assert_eq!(a, b, "trust is provenance, not identity");
        assert!(b.is_tainted() && !a.is_tainted());
    }

    #[test]
    fn an_octet_reads_back_as_an_int_in_range() {
        let b = BytesObj::trusted(vec![0, 127, 255]);
        let p = &b as *const BytesObj as *const c_void;
        assert_eq!(unsafe { jrt_bytes_get(p, 0) }, 0);
        assert_eq!(unsafe { jrt_bytes_get(p, 2) }, 255);
        assert_eq!(unsafe { jrt_bytes_get(p, 3) }, -1, "out of range reports, not panics");
        assert_eq!(unsafe { jrt_bytes_get(p, -1) }, -1);
    }
}
